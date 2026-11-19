//! OBS in the controller (specs/07-obs.md): scene control for BRB/privacy, scene changes
//! from any source, stream/record anchors and `media/recordings.json`.

use ac_core::event::{By, EventKind, OutputState, ServiceStatus, Source, StreamStatus};
use ac_core::recordings::Reconciled;
use ac_core::session::Status;
use ac_obs::{Request, Snapshot, Update};
use tokio::sync::mpsc;

use super::{Controller, Msg, by_src};

/// How the controller reaches OBS.
pub enum ObsLink {
    Real(mpsc::UnboundedSender<Request>),
    /// `--mock`: scene switches are echoed back through the controller's own queue, in
    /// order with script commands, like OBS reports them.
    Fake {
        scenes: Vec<String>,
        msgs: mpsc::UnboundedSender<Msg>,
    },
}

impl Controller {
    pub(super) fn on_obs(&mut self, update: Update) {
        match update {
            Update::Connected(snapshot) => self.obs_connected(snapshot),
            Update::Down { error, retry_in } => {
                self.obs_scenes = None;
                self.pending_scenes.clear();
                // One error per outage, not one per retry.
                if self.state.services.get("obs") != Some(&ServiceStatus::Down) {
                    self.emit(
                        "obs",
                        EventKind::ServiceError {
                            service: "obs".into(),
                            error: Some(error),
                            retry_in_s: retry_in.as_secs_f64(),
                        },
                    );
                    self.service_state("obs", ServiceStatus::Down);
                }
            }
            Update::Scene(to) => {
                if self.pending_scenes.front() == Some(&to) {
                    // Our own switch: BRB/privacy events were already emitted.
                    self.pending_scenes.pop_front();
                    self.scene_changed(to, false);
                } else {
                    self.scene_changed(to, true);
                }
            }
            Update::Stream(state) => self.stream_changed(state),
            Update::Record { state, path } => self.record_changed(state, path),
            Update::Failed(error) => {
                self.pending_scenes.pop_front();
                self.notice = Some(format!("OBS: {error}"));
            }
        }
    }

    /// Reconciles session state with what OBS reports after a (re)connect.
    fn obs_connected(&mut self, snap: Snapshot) {
        self.service_state("obs", ServiceStatus::Ok);
        self.obs_scenes = Some(snap.scenes);
        let now = self.clock.now_ts();

        if let Some(scene) = snap.scene {
            self.scene_changed(scene, true);
        }

        let streaming = matches!(
            self.state.obs_stream,
            Some(OutputState::Started | OutputState::Reconnecting)
        );
        if snap.stream.active && !streaming {
            let anchors = &mut self.session.meta.anchors;
            if anchors.obs_stream_start.is_none() {
                anchors.obs_stream_start = Some(now.add_secs(-snap.stream.duration_s));
                self.save_session();
            }
            self.emit(
                "obs",
                EventKind::ObsStream {
                    state: OutputState::Started,
                },
            );
            self.stream_restarted();
        } else if !snap.stream.active && streaming {
            self.stream_changed(OutputState::Stopped);
        }

        let rec = snap.record;
        match self
            .recordings
            .reconcile(rec.active, rec.paused, rec.duration_s, now)
        {
            Reconciled::Started => {
                let start = self.recordings.open().map(|r| r.start);
                let anchors = &mut self.session.meta.anchors;
                anchors.obs_record_start = anchors.obs_record_start.or(start);
            }
            Reconciled::Stopped => tracing::warn!("recording stopped while OBS was unreachable"),
            Reconciled::Unchanged => {}
        }
        let want = match (rec.active, rec.paused) {
            (false, _) => OutputState::Stopped,
            (true, true) => OutputState::Paused,
            (true, false) if self.state.obs_record == Some(OutputState::Paused) => {
                OutputState::Resumed
            }
            (true, false) => OutputState::Started,
        };
        let have = match self.state.obs_record {
            None => OutputState::Stopped,
            Some(OutputState::Resumed) => OutputState::Started,
            Some(s) => s,
        };
        if want != have {
            self.emit(
                "obs",
                EventKind::ObsRecord {
                    state: want,
                    path: None,
                },
            );
        }
        self.save_recordings();
    }

    /// A program scene change. `infer` is false for switches aftercast requested itself;
    /// otherwise switching to or from the BRB/privacy scene toggles that mode.
    fn scene_changed(&mut self, to: String, infer: bool) {
        let from = self.state.scene.clone();
        if from.as_ref() == Some(&to) {
            return;
        }
        self.emit(
            "obs",
            EventKind::ObsScene {
                from: from.clone(),
                to: to.clone(),
            },
        );
        if !infer {
            return;
        }
        let scenes = &self.config.obs.scenes;
        let (brb, privacy) = (scenes.brb.clone(), scenes.privacy.clone());
        let left = |name: &str| from.as_deref() == Some(name);
        if to == brb && !self.state.brb {
            self.emit(
                "obs",
                EventKind::BrbStart {
                    return_scene: from.clone(),
                    countdown_s: None,
                },
            );
        } else if left(&brb) && self.state.brb {
            self.emit("obs", EventKind::BrbEnd {});
        }
        if to == privacy && !self.state.privacy {
            self.privacy_return = from.clone();
            self.emit("obs", EventKind::PrivacyStart { by: By::ObsScene });
        } else if left(&privacy) && self.state.privacy {
            self.emit("obs", EventKind::PrivacyEnd {});
        }
    }

    fn stream_changed(&mut self, state: OutputState) {
        self.emit("obs", EventKind::ObsStream { state });
        match state {
            OutputState::Started => {
                if self.session.meta.anchors.obs_stream_start.is_none() {
                    self.session.meta.anchors.obs_stream_start = Some(self.clock.now_ts());
                    self.save_session();
                }
                self.stream_restarted();
            }
            OutputState::Stopped if !self.state.ended => {
                self.emit(
                    "obs",
                    EventKind::StreamEnded {
                        source: Source::Obs,
                    },
                );
                self.set_status(Status::Ended);
            }
            _ => {}
        }
    }

    /// OBS streams again after its stop ended the session (a flaky connection makes OBS
    /// give up, then the streamer restarts): back to live, unless YouTube has already
    /// reported the broadcast's end or the session was finalized.
    fn stream_restarted(&mut self) {
        if !self.state.ended
            || self.session.meta.status != Status::Ended
            || self.session.meta.anchors.yt_actual_end.is_some()
        {
            return;
        }
        self.emit(
            "obs",
            EventKind::StreamStatus {
                status: StreamStatus::Live,
                source: Source::Obs,
            },
        );
        self.session.meta.status = Status::Live;
        self.save_session();
        self.notice = Some("OBS is streaming again; session is live".into());
    }

    fn record_changed(&mut self, state: OutputState, path: Option<String>) {
        let now = self.clock.now_ts();
        let r = &mut self.recordings;
        match state {
            OutputState::Started => {
                r.start(path.clone(), now);
                let anchors = &mut self.session.meta.anchors;
                anchors.obs_record_start = anchors.obs_record_start.or(Some(now));
            }
            OutputState::Stopped => r.stop(path.clone(), now),
            OutputState::Paused => r.pause(now),
            OutputState::Resumed => r.resume(now),
            OutputState::FileChanged => match path.clone() {
                Some(p) => r.file_changed(p, now),
                None => return,
            },
            OutputState::Reconnecting => return,
        }
        self.emit("obs", EventKind::ObsRecord { state, path });
        self.save_recordings();
    }

    /// `b`, `:brb`, `:back`, the BRB hotkey. `on: None` toggles.
    pub(super) fn brb(&mut self, on: Option<bool>, by: By) {
        let on = on.unwrap_or(!self.state.brb);
        if on == self.state.brb {
            self.notice = Some(format!("BRB is already {}", if on { "on" } else { "off" }));
            return;
        }
        let scenes = self.config.obs.scenes.clone();
        if on {
            let return_scene = self.state.scene.clone().filter(|s| *s != scenes.brb);
            self.emit(
                by_src(by),
                EventKind::BrbStart {
                    return_scene,
                    countdown_s: None,
                },
            );
            self.switch_scene(scenes.brb);
        } else {
            let back = self.state.brb_return.clone().unwrap_or(scenes.main);
            self.emit(by_src(by), EventKind::BrbEnd {});
            self.switch_scene(back);
        }
    }

    /// `p`, `:privacy on|off`, the privacy hotkey. `on: None` toggles.
    pub(super) fn privacy(&mut self, on: Option<bool>, by: By) {
        let on = on.unwrap_or(!self.state.privacy);
        if on == self.state.privacy {
            self.notice = Some(format!(
                "privacy is already {}",
                if on { "on" } else { "off" }
            ));
            return;
        }
        let scenes = self.config.obs.scenes.clone();
        if on {
            self.privacy_return = self.state.scene.clone().filter(|s| *s != scenes.privacy);
            self.emit(by_src(by), EventKind::PrivacyStart { by });
            self.switch_scene(scenes.privacy);
        } else {
            // ponytail: the return scene lives in memory; after a restart privacy off goes to main.
            let back = self.privacy_return.take().unwrap_or(scenes.main);
            self.emit(by_src(by), EventKind::PrivacyEnd {});
            self.switch_scene(back);
        }
    }

    /// Asks OBS to switch; the resulting `obs.scene` event comes from OBS's echo.
    pub(super) fn switch_scene(&mut self, name: String) {
        if self.state.scene.as_ref() == Some(&name) {
            return;
        }
        if self.obs_scenes.is_none() {
            self.notice = Some(format!("OBS is not connected; scene {name:?} not switched"));
            return;
        }
        self.pending_scenes.push_back(name.clone());
        let sent = match &self.obs {
            ObsLink::Real(tx) => tx.send(Request::SetScene(name)).is_ok(),
            ObsLink::Fake { scenes, msgs } => {
                let update = if scenes.contains(&name) {
                    Update::Scene(name)
                } else {
                    Update::Failed(format!("no scene named {name:?}"))
                };
                msgs.send(Msg::Obs(update)).is_ok()
            }
        };
        if !sent {
            self.pending_scenes.pop_back();
        }
    }

    fn save_recordings(&mut self) {
        self.session.meta.anchors.recordings = self.recordings.files.clone();
        if let Err(e) = self.recordings.save(&self.session.recordings_file()) {
            tracing::error!("saving recordings.json: {e}");
        }
        self.save_session();
    }
}
