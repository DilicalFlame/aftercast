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

/// `[obs] launch`: starts OBS when its WebSocket is on this computer and it is not running.
/// Never in safe mode (OBS asks after a crash and waits), and detached: it keeps running
/// when aftercast exits, and Ctrl+C in aftercast's terminal does not reach it. The
/// connection retries on its own until OBS is up.
pub(super) fn launch(config: &ac_core::config::Obs) {
    if !config.launch || !is_local(&config.url) || running() {
        return;
    }
    let Some(exe) = exe(&config.path) else {
        tracing::warn!("OBS is not running and was not found; set [obs] path to obs64.exe");
        return;
    };
    let mut cmd = std::process::Command::new(&exe);
    // OBS finds its own files from the folder it starts in.
    if let Some(dir) = exe.parent().filter(|d| !d.as_os_str().is_empty()) {
        cmd.current_dir(dir);
    }
    cmd.arg("--disable-shutdown-check")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    match cmd.spawn() {
        Ok(_) => tracing::info!("started OBS: {}", exe.display()),
        Err(e) => tracing::warn!("starting OBS ({}): {e}", exe.display()),
    }
}

/// The WebSocket is on this computer, so OBS would be too.
fn is_local(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url);
    ["127.0.0.1", "localhost", "[::1]"].iter().any(|h| {
        host.strip_prefix(h)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
    })
}

fn running() -> bool {
    #[cfg(windows)]
    let out = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq obs64.exe", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("obs64.exe"));
    #[cfg(not(windows))]
    let out = std::process::Command::new("pgrep")
        .args(["-x", "obs"])
        .status()
        .map(|s| s.success());
    // Cannot tell: better not to start a second OBS (it would ask about the first).
    out.unwrap_or(true)
}

fn exe(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if !path.as_os_str().is_empty() {
        return Some(path.to_path_buf());
    }
    #[cfg(windows)]
    {
        ["ProgramFiles", "ProgramW6432"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|dir| std::path::Path::new(&dir).join(r"obs-studio\bin\64bit\obs64.exe"))
            .find(|p| p.is_file())
    }
    #[cfg(not(windows))]
    Some("obs".into())
}

/// The URLs our overlay pages have in OBS Browser sources.
fn overlay_prefixes(port: u16) -> Vec<String> {
    ["127.0.0.1", "localhost"]
        .map(|host| format!("http://{host}:{port}/overlay/"))
        .to_vec()
}

impl Controller {
    /// Reloads (without cache) every OBS browser source showing one of our overlays.
    pub(super) fn refresh_overlays(&self) {
        if let (ObsLink::Real(tx), Ok(addr)) = (&self.obs, &self.server) {
            let _ = tx.send(Request::RefreshBrowsers(overlay_prefixes(addr.port())));
        }
    }

    /// Stacks the overlay sources in OBS as `[overlay] order` says.
    pub(super) fn stack_overlays(&self) {
        if let (ObsLink::Real(tx), Ok(addr)) = (&self.obs, &self.server) {
            let order = self.config.overlay.order().into_iter().map(String::from);
            let _ = tx.send(Request::OverlayOrder {
                prefixes: overlay_prefixes(addr.port()),
                order: order.collect(),
            });
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
                // Our own switch: BRB/privacy events were already emitted. Switches before it
                // never came (with a stream delay, privacy on cancels a waiting switch).
                if let Some(i) = self.pending_scenes.iter().position(|s| *s == to) {
                    self.pending_scenes.drain(..=i);
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
                    // The pointer flips the facecam all the time; that is not an event.
                    let cam = &self.config.obs.facecam;
                    if cam.enabled && source == cam.source {
                        return;
                    }
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
            Update::Place {
                scene,
                id,
                rect,
                canvas,
            } => self.facecam_place = Some((scene, id, rect, canvas)),
            Update::Screen { scene, id, index } => self.facecam_screen = Some((scene, id, index)),
            Update::RecordDirectory(dir) => self.record_dir = Some(dir),
            Update::Levels(levels) => {
                if let Ok(mut map) = self.obs_levels.lock() {
                    map.clear();
                    map.extend(levels);
                }
            }
            Update::Items(detail) => self.obs_items = Arc::new(detail),
            Update::Source(detail) => self.obs_source = Arc::new(detail),
            Update::Shot(image) => {
                if let Some(slot) = &self.shot_slot
                    && let Ok(mut g) = slot.lock()
                {
                    *g = image;
                }
            }
            Update::Stream(state) => {
                self.stream_changed(state);
                self.quit_ready();
            }
            Update::Record { state, path } => {
                self.record_changed(state, path);
                self.quit_ready();
            }
            Update::Failed(error) => {
                self.pending_scenes.pop_front();
                self.notice = Some(format!("OBS: {error}"));
            }
        }
    }

    /// Quitting from Home waited for this: OBS has stopped the stream and let go of the
    /// recording's file.
    fn quit_ready(&mut self) {
        let busy = |s: Option<OutputState>, on: &[OutputState]| s.is_some_and(|s| on.contains(&s));
        if self.quit_by.is_some()
            && !busy(
                self.state.obs_record,
                &[
                    OutputState::Started,
                    OutputState::Resumed,
                    OutputState::Paused,
                ],
            )
            && !busy(
                self.state.obs_stream,
                &[OutputState::Started, OutputState::Reconnecting],
            )
        {
            self.done = true;
        }
    }

    /// Reconciles session state with what OBS reports after a (re)connect.
    fn obs_connected(&mut self, snap: Snapshot) {
        self.service_state("obs", ServiceStatus::Ok);
        self.obs_scenes = Some(snap.scenes);
        self.obs_media = Arc::new(snap.media);
        let now = self.output_now();
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
        self.auto_stream();

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

    /// BRB is a chapter of its own: "Break" from its start, then back to the chapter before it
    /// (unless a chapter was set during the break: that one already carries on).
    fn brb_chapter(&mut self, start: bool) {
        if !self.begun || !self.config.stream.chapter_on_brb {
            return;
        }
        let now = self.clock.now_ts();
        let latest = ac_template::chapters::candidates(&self.state, &self.config.stream)
            .into_iter()
            .filter(|c| c.0 <= now)
            .max_by_key(|c| c.0);
        if !start && latest.is_some_and(|c| c.1 != ac_template::chapters::BREAK) {
            return;
        }
        let title = if start {
            ac_template::chapters::BREAK.to_owned()
        } else {
            self.current_chapter()
        };
        self.latency_chapter(title);
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
        // The starting overlay counts up from when the starting scene came on.
        self.starting_since = (to == self.config.obs.scenes.starting)
            .then(|| self.starting_since.unwrap_or_else(|| self.clock.now_ts()));
        // Starting Soon and Ending are chapters of their own, so viewers can skip them; the
        // chapter after Starting Soon is the stream proper. (BRB: `chapter_on_brb`.)
        if self.begun {
            let scenes = &self.config.obs.scenes;
            let title = if to == scenes.starting {
                Some(self.config.stream.starting_chapter_title.clone())
            } else if to == scenes.ending {
                Some(self.config.stream.ending_chapter_title.clone())
            } else if from.as_deref() == Some(scenes.starting.as_str()) {
                Some(self.config.stream.first_chapter_title.clone())
            } else {
                None
            };
            if let Some(title) = title {
                self.latency_chapter(title);
            }
        }
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
            self.brb_chapter(true);
        } else if left(&brb) && self.state.brb {
            self.emit("obs", EventKind::BrbEnd {});
            self.brb_chapter(false);
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
                    self.session.meta.anchors.obs_stream_start = Some(self.output_now());
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

    /// Now, as far as OBS's outputs are concerned: with `[stream] delay_ms` the recording
    /// and stream carry what happened that much earlier, so their anchors move back by it
    /// and post lines events up with the picture.
    fn output_now(&self) -> ac_core::time::Timestamp {
        let delay_ms = match self.obs {
            ObsLink::Real(_) => super::stream_delay_ms(&self.config),
            ObsLink::Fake { .. } => 0,
        };
        self.clock.now_ts().add_secs(-(delay_ms as f64) / 1000.0)
    }

    fn record_changed(&mut self, state: OutputState, path: Option<String>) {
        let now = self.output_now();
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
            let return_scene = self
                .state
                .scene
                .clone()
                .filter(|s| *s != scenes.brb && *s != scenes.privacy);
            self.emit(
                by_src(by),
                EventKind::BrbStart {
                    return_scene,
                    countdown_s: None,
                },
            );
            self.brb_chapter(true);
            self.switch_scene(scenes.brb);
        } else {
            let back = self.state.brb_return.clone().unwrap_or(scenes.main);
            self.emit(by_src(by), EventKind::BrbEnd {});
            self.brb_chapter(false);
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
            self.privacy_return = self
                .state
                .scene
                .clone()
                .filter(|s| *s != scenes.privacy && *s != scenes.brb);
            self.emit(by_src(by), EventKind::PrivacyStart { by });
            self.switch_scene(scenes.privacy);
        } else {
            // ponytail: the return scene lives in memory; after a restart privacy off goes to main.
            let back = self.privacy_return.take().unwrap_or(scenes.main);
            self.emit(by_src(by), EventKind::PrivacyEnd {});
            self.switch_scene(back);
        }
    }

    /// `:end`: the end screen (the `ending` scene) for `screen_s` seconds, then
    /// [`Self::end_now`]. With 0 seconds, or no such scene in OBS, it ends at once.
    pub(super) fn end_stream(&mut self, screen_s: u32) {
        if self.obs_scenes.is_none() {
            self.notice = Some("OBS is not connected".into());
            return;
        }
        if self.ending.is_some() {
            self.notice = Some("already ending; :end cancel stays live".into());
            return;
        }
        let scene = self.config.obs.scenes.ending.clone();
        let has_scene = self.obs_scenes.as_ref().is_some_and(|s| s.contains(&scene));
        if screen_s == 0 || !has_scene {
            self.end_now();
            if screen_s > 0 {
                self.notice = Some(format!(
                    "stream ended without an end screen: OBS has no scene {scene:?}"
                ));
            }
            return;
        }
        self.switch_scene(scene);
        // On the session clock (scripts move it), checked when the timer fires and before
        // every command.
        self.ending = Some(self.clock.now_ts().add_secs(screen_s.into()));
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(screen_s.into())).await;
            let _ = tx.send(Msg::EndDue);
        });
        self.notice = Some(format!(
            "end screen for {screen_s} s, then the stream ends (:end cancel stays live)"
        ));
    }

    /// Ends the stream if the end screen has run its time.
    pub(super) fn end_due(&mut self) {
        if self.ending.is_some_and(|at| self.clock.now_ts() >= at) {
            self.end_now();
        }
    }

    /// `:end cancel`: back to the main scene, still live.
    pub(super) fn cancel_end(&mut self) {
        if self.ending.take().is_none() {
            self.notice = Some("not ending".into());
            return;
        }
        self.switch_scene(self.config.obs.scenes.main.clone());
        self.notice = Some("end cancelled: still live".into());
    }

    /// Stops whatever OBS has running: streaming, recording, the virtual camera.
    fn end_now(&mut self) {
        self.ending = None;
        if matches!(
            self.state.obs_stream,
            Some(OutputState::Started | OutputState::Reconnecting)
        ) {
            self.obs_request(Request::Stream(false));
        }
        if matches!(
            self.state.obs_record,
            Some(OutputState::Started | OutputState::Resumed | OutputState::Paused)
        ) {
            self.obs_request(Request::Record(ac_core::command::OutputAction::Stop));
        }
        self.obs_request(Request::StopVirtualCam);
        // The stream is over from here: say so now (OBS takes seconds to report its output
        // stopped), so the end prompt is up the moment the end screen's time is.
        if !self.state.ended {
            self.emit(
                "obs",
                EventKind::StreamEnded {
                    source: Source::Obs,
                },
            );
            self.set_status(Status::Ended);
        }
        self.notice = Some("stream ended: OBS stopped streaming, recording and the virtual camera; :finalize when ready".into());
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

    /// Every 100 ms: hides the facecam while the mouse pointer is over it, shows it again a
    /// moment after the pointer left (`[obs.facecam]`).
    pub(super) fn pointer_tick(&mut self) {
        // The scene the editor looks at is listed again about once a second.
        self.watch_tick = (self.watch_tick + 1) % 10;
        if self.watch_tick == 0
            && let Some((scene, until)) = self.watch_scene.clone()
        {
            if crate::live::Instant::now() < until {
                self.obs_request(Request::Items { scene });
            } else {
                self.watch_scene = None;
            }
        }
        // A few pictures a second of the scene being edited.
        if self.watch_tick % 3 == 1
            && let Some((scene, until)) = self.watch_shot.clone()
        {
            if crate::live::Instant::now() < until {
                self.obs_request(Request::Shot { scene });
            } else {
                self.watch_shot = None;
            }
        }
        if self.watch_tick == 5
            && let Some((name, until)) = self.watch_source.clone()
        {
            if crate::live::Instant::now() < until {
                self.obs_request(Request::Source { name });
            } else {
                self.watch_source = None;
            }
        }
        let cam = &self.config.obs.facecam;
        if !cam.enabled || self.obs_scenes.is_none() {
            // Switched off meanwhile: the camera must not stay hidden.
            if self.facecam_hidden.is_some() {
                self.facecam_release();
            }
            return;
        }
        let Some((px, py)) = ac_capture::pointer::pointer() else {
            return;
        };
        if self
            .screens
            .as_ref()
            .is_none_or(|(t, _)| t.elapsed() >= std::time::Duration::from_secs(3))
        {
            self.screens = Some((tokio::time::Instant::now(), ac_capture::pointer::screens()));
        }
        let scene = self.state.scene.clone().unwrap_or_default();
        let item = self.obs_media.item(&scene, &cam.source).cloned();
        // The camera's place comes from OBS, scene by scene: asked again each second (and at
        // once in a new scene), since it can be moved or resized any time.
        let mut rect = cam.rect();
        let mut canvas = (1920.0, 1080.0);
        if cam.auto
            && let Some(item) = &item
        {
            let known = self
                .facecam_place
                .as_ref()
                .filter(|p| p.0 == item.scene && p.1 == item.id);
            if let Some(p) = known {
                (rect, canvas) = (p.2, p.3);
            }
            let unknown = known.is_none();
            let stale = self
                .facecam_asked
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(1));
            if stale || unknown {
                self.facecam_asked = Some(tokio::time::Instant::now());
                self.obs_request(Request::Place {
                    scene: item.scene.clone(),
                    id: item.id,
                });
                if self.config.obs.facecam.monitor == 0 {
                    self.obs_request(Request::Screen {
                        scene: item.scene.clone(),
                    });
                }
            }
        }
        // The screen OBS captures in this scene (asked with the camera's place).
        let on_air = self
            .facecam_screen
            .as_ref()
            .filter(|s| s.0 == scene)
            .map_or((None, None), |s| (s.1.as_deref(), s.2));
        let cam = &self.config.obs.facecam;
        let screens = self.screens.as_ref().map_or(&[][..], |s| &s.1[..]);
        let Some(screen) = ac_capture::pointer::choose(screens, cam.monitor, on_air.0, on_air.1)
        else {
            return;
        };
        let (fx, fy) = screen.fraction(px, py);
        let over = cam.over(rect, fx * canvas.0, fy * canvas.1);
        let seen = format!(
            "scene {scene:?} source found {} auto {} screen {} (OBS capture: id {:?}, index {:?}) of {} rect {rect:?} canvas {canvas:?}",
            item.is_some(),
            cam.auto,
            screen.name,
            on_air.0,
            on_air.1,
            screens.len(),
        );
        if seen != self.facecam_logged {
            tracing::info!("facecam: {seen}");
            self.facecam_logged = seen;
        }
        let back = std::time::Duration::from_millis(cam.return_ms.into());
        match self.hover.step(over, std::time::Instant::now(), back) {
            Some(true) => {
                tracing::info!(
                    "facecam: pointer over it at {fx:.2},{fy:.2} of {}",
                    screen.name
                );
                // A camera the owner already hid stays theirs.
                if let Some(item) = item.filter(|i| i.enabled) {
                    self.facecam_hidden = Some((item.scene.clone(), item.id));
                    self.obs_request(Request::ItemEnabled {
                        scene: item.scene,
                        id: item.id,
                        enabled: false,
                    });
                }
            }
            Some(false) => self.facecam_release(),
            None => {}
        }
    }

    /// The editor's "add to scene": the overlay's page as a Browser source the size of the
    /// canvas. When some scene already has one for this overlay its input is shared.
    pub(super) fn add_overlay(&mut self, scene: String, overlay: String) {
        if self.obs_scenes.is_none() {
            self.notice = Some("OBS is not connected".into());
            return;
        }
        let Ok(addr) = &self.server else {
            self.notice = Some("the overlay server is not running".into());
            return;
        };
        if self
            .obs_media
            .items
            .iter()
            .any(|i| i.scene == scene && i.overlay.as_deref() == Some(&overlay))
        {
            self.notice = Some(format!("{scene:?} already has the {overlay} overlay"));
            return;
        }
        let shared = self
            .obs_media
            .items
            .iter()
            .find(|i| i.overlay.as_deref() == Some(&overlay))
            .map(|i| i.source.clone());
        self.notice = Some(format!("{overlay} overlay added to {scene}"));
        self.obs_request(Request::AddBrowser {
            scene,
            existing: shared.is_some(),
            source: shared.unwrap_or_else(|| format!("aftercast {overlay}")),
            url: format!("http://{addr}/overlay/{overlay}"),
            // The alert's sound reaches the stream only through OBS's mixer.
            audio: overlay == "alert",
        });
    }

    /// Shows the facecam again if the pointer hid it.
    pub(super) fn facecam_release(&mut self) {
        if let Some((scene, id)) = self.facecam_hidden.take() {
            self.obs_request(Request::ItemEnabled {
                scene,
                id,
                enabled: true,
            });
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

#[cfg(test)]
mod tests {
    #[test]
    fn launches_only_for_obs_on_this_computer() {
        for url in [
            "ws://127.0.0.1:4455",
            "ws://localhost:4455",
            "ws://[::1]:4455",
            "ws://localhost",
        ] {
            assert!(super::is_local(url), "{url}");
        }
        for url in [
            "ws://192.168.1.20:4455",
            "ws://localhost.example.com:4455",
            "ws://127.0.0.10:4455",
        ] {
            assert!(!super::is_local(url), "{url}");
        }
    }
}
