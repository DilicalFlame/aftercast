//! OBS in the controller (specs/07-obs.md): scene control for BRB/privacy, scene changes
//! from any source, stream/record anchors and `media/recordings.json`.

use ac_core::event::{By, EventKind, OutputState, ServiceStatus, Source, StreamStatus};
use ac_core::recordings::Reconciled;
use ac_core::session::Status;
use std::sync::Arc;

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
    /// Reloads (without cache) every OBS browser source showing one of our overlays.
    pub(super) fn refresh_overlays(&self) {
        if let (ObsLink::Real(tx), Ok(addr)) = (&self.obs, &self.server) {
            let port = addr.port();
            let prefixes = ["127.0.0.1", "localhost"]
                .map(|host| format!("http://{host}:{port}/overlay/"))
                .to_vec();
            let _ = tx.send(Request::RefreshBrowsers(prefixes));
        }
    }

    pub(super) fn on_obs(&mut self, update: Update) {
        match update {
            Update::Connected(snapshot) => self.obs_connected(snapshot),
            Update::Down { error, retry_in } => {
                self.obs_scenes = None;
                self.obs_media = Arc::default();
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
            Update::Media(media) => {
                self.obs_scenes = Some(media.scenes.clone());
                self.obs_media = Arc::new(media);
            }
            Update::Mute { input, muted } => {
                let media = Arc::make_mut(&mut self.obs_media);
                if let Some(a) = media.audio.iter_mut().find(|a| a.name == input)
                    && a.muted != muted
                {
                    a.muted = muted;
                    self.emit("obs", EventKind::ObsMute { input, muted });
                }
            }
            Update::Volume { input, db } => {
                let media = Arc::make_mut(&mut self.obs_media);
                if let Some(a) = media.audio.iter_mut().find(|a| a.name == input) {
                    a.volume_db = db;
                }
            }
            Update::ItemEnabled { scene, id, enabled } => {
                let media = Arc::make_mut(&mut self.obs_media);
                if let Some(item) = media
                    .items
                    .iter_mut()
                    .find(|i| i.scene == scene && i.id == id)
                    && item.enabled != enabled
                {
                    item.enabled = enabled;
                    let source = item.source.clone();
                    self.emit(
                        "obs",
                        EventKind::ObsSource {
                            scene,
                            source,
                            visible: enabled,
                        },
                    );
                }
            }
            Update::RecordDirectory(dir) => self.record_dir = Some(dir),
            Update::Levels(levels) => {
                if let Ok(mut map) = self.obs_levels.lock() {
                    map.clear();
                    map.extend(levels);
                }
            }
            Update::Stream(state) => self.stream_changed(state),
            Update::Record { state, path } => {
                self.record_changed(state, path);
                // Quitting a draft waited for this: OBS has let go of the file.
                if self.quit_by.is_some() && state == OutputState::Stopped {
                    self.done = true;
                }
            }
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
        self.obs_media = Arc::new(snap.media);
        let now = self.clock.now_ts();
        // Overlay pages OBS loaded while aftercast was down show an error and never retry.
        self.refresh_overlays();
        // The rehearse page shows OBS through its Virtual Camera.
        if self.rehearse_feed.is_some()
            && let ObsLink::Real(tx) = &self.obs
        {
            let _ = tx.send(Request::StartVirtualCam);
        }

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

    /// Sends a request to OBS; the mock echoes what OBS would report back.
    pub(super) fn obs_request(&mut self, request: Request) {
        let sent = match &self.obs {
            ObsLink::Real(tx) => tx.send(request).is_ok(),
            ObsLink::Fake { msgs, .. } => {
                let echo = match request {
                    Request::Mute { input, muted } => Update::Mute { input, muted },
                    Request::Volume { input, db } => Update::Volume { input, db },
                    Request::ItemEnabled { scene, id, enabled } => {
                        Update::ItemEnabled { scene, id, enabled }
                    }
                    Request::Record(action) => {
                        use ac_core::command::OutputAction as A;
                        let state = match action {
                            A::Start => OutputState::Started,
                            A::Stop => OutputState::Stopped,
                            A::Pause => OutputState::Paused,
                            A::Resume => OutputState::Resumed,
                        };
                        let path = (action == A::Start).then(|| "D:/OBS/mock.mkv".to_owned());
                        Update::Record { state, path }
                    }
                    Request::Stream(on) => Update::Stream(if on {
                        OutputState::Started
                    } else {
                        OutputState::Stopped
                    }),
                    // The fake OBS has no audio to measure.
                    _ => return,
                };
                msgs.send(Msg::Obs(echo)).is_ok()
            }
        };
        if !sent {
            self.notice = Some("OBS is not connected".into());
        }
    }

    /// `:record`: OBS reports the result as a record state change.
    pub(super) fn record(&mut self, action: ac_core::command::OutputAction) {
        use ac_core::command::OutputAction as A;
        if self.obs_scenes.is_none() {
            self.notice = Some("OBS is not connected".into());
            return;
        }
        let recording = matches!(
            self.state.obs_record,
            Some(OutputState::Started | OutputState::Resumed | OutputState::Paused)
        );
        let paused = self.state.obs_record == Some(OutputState::Paused);
        let refused = match action {
            A::Start if recording => Some("OBS is already recording"),
            A::Stop | A::Pause if !recording => Some("OBS is not recording"),
            A::Pause if paused => Some("the recording is already paused"),
            A::Resume if !paused => Some("the recording is not paused"),
            _ => None,
        };
        if let Some(why) = refused {
            self.notice = Some(why.into());
            return;
        }
        self.obs_request(Request::Record(action));
    }

    /// `:stream start|stop`.
    pub(super) fn stream(&mut self, on: bool) {
        if self.obs_scenes.is_none() {
            self.notice = Some("OBS is not connected".into());
            return;
        }
        let streaming = matches!(
            self.state.obs_stream,
            Some(OutputState::Started | OutputState::Reconnecting)
        );
        if streaming == on {
            self.notice = Some(
                if on {
                    "OBS is already streaming"
                } else {
                    "OBS is not streaming"
                }
                .into(),
            );
            return;
        }
        self.obs_request(Request::Stream(on));
    }

    /// Audio levels are asked of OBS only while a page shows them.
    pub(super) fn meters(&mut self, on: bool) {
        if !on && let Ok(mut map) = self.obs_levels.lock() {
            map.clear();
        }
        if let ObsLink::Real(tx) = &self.obs {
            let _ = tx.send(Request::Meters(on));
        }
    }

    /// The audio input typed as `name`, or a notice listing the ones OBS has.
    fn audio_named(&mut self, name: &str) -> Option<ac_core::obs::Audio> {
        if self.obs_scenes.is_none() {
            self.notice = Some("OBS is not connected".into());
            return None;
        }
        let found = self.obs_media.audio(name).cloned();
        if found.is_none() {
            let have: Vec<&str> = self
                .obs_media
                .audio
                .iter()
                .map(|a| a.name.as_str())
                .collect();
            self.notice = Some(format!(
                "OBS has no audio named {name:?} (have: {})",
                have.join(", ")
            ));
        }
        found
    }

    /// `:mute`: on, off, or (`None`) the opposite of now.
    pub(super) fn mute(&mut self, input: &str, on: Option<bool>) {
        if let Some(a) = self.audio_named(input) {
            let muted = on.unwrap_or(!a.muted);
            self.notice = Some(format!(
                "{} {}",
                a.name,
                if muted { "muted" } else { "unmuted" }
            ));
            self.obs_request(Request::Mute {
                input: a.name,
                muted,
            });
        }
    }

    /// `:volume`: dB, within what OBS's mixer allows.
    pub(super) fn volume(&mut self, input: &str, db: f64) {
        if let Some(a) = self.audio_named(input) {
            let db = db.clamp(-100.0, 26.0);
            self.notice = Some(format!("{} at {db:.1} dB", a.name));
            self.obs_request(Request::Volume { input: a.name, db });
        }
    }

    /// `:show` / `:hide`: a source in `scene` (default: the program scene).
    pub(super) fn source_visible(
        &mut self,
        source: &str,
        scene: Option<&str>,
        visible: Option<bool>,
    ) {
        if self.obs_scenes.is_none() {
            self.notice = Some("OBS is not connected".into());
            return;
        }
        let scene = scene
            .map(str::to_owned)
            .or_else(|| self.state.scene.clone())
            .unwrap_or_default();
        let Some(item) = self.obs_media.item(&scene, source).cloned() else {
            let have: Vec<&str> = self
                .obs_media
                .sources_in(&scene)
                .map(|i| i.source.as_str())
                .collect();
            self.notice = Some(format!(
                "scene {scene:?} has no source {source:?} (has: {})",
                have.join(", ")
            ));
            return;
        };
        let enabled = visible.unwrap_or(!item.enabled);
        self.notice = Some(format!(
            "{} {} in {}",
            item.source,
            if enabled { "shown" } else { "hidden" },
            item.scene
        ));
        self.obs_request(Request::ItemEnabled {
            scene: item.scene,
            id: item.id,
            enabled,
        });
    }

    fn save_recordings(&mut self) {
        self.session.meta.anchors.recordings = self.recordings.files.clone();
        if let Err(e) = self.recordings.save(&self.session.recordings_file()) {
            tracing::error!("saving recordings.json: {e}");
        }
        self.save_session();
    }
}
