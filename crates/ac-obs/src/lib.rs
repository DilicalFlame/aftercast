//! obs-websocket v5 service (specs/07-obs.md): keeps a connection to OBS, reports scene and
//! output changes as [`Update`]s and performs [`Request`]s. Session logic lives in the
//! controller; this crate only talks to OBS.

use std::time::Duration;

use ac_core::event::OutputState;
use ac_core::obs::{Audio, Item, Media};
use futures_util::StreamExt;
use obws::events::{Event as ObsEvent, OutputState as ObsOutput};
use tokio::sync::mpsc;
use tokio::time::sleep;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(15);

/// State read from OBS right after a (re)connect.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub scene: Option<String>,
    pub scenes: Vec<String>,
    pub stream: Output,
    pub record: Output,
    /// Audio inputs and the sources in every scene.
    pub media: Media,
}

/// Status of the stream or record output.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Output {
    pub active: bool,
    pub paused: bool,
    pub reconnecting: bool,
    /// `outputDuration` in seconds.
    pub duration_s: f64,
}

/// Something that happened in OBS (or to the connection).
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Connected(Snapshot),
    /// Connection failed or was lost; the next attempt is in `retry_in`.
    Down {
        error: String,
        retry_in: Duration,
    },
    Scene(String),
    /// Scenes, inputs or sources were added, removed or renamed: the fresh lists.
    Media(Media),
    Mute {
        input: String,
        muted: bool,
    },
    Volume {
        input: String,
        db: f64,
    },
    ItemEnabled {
        scene: String,
        id: i64,
        enabled: bool,
    },
    /// Where an item shows on the canvas (the answer to [`Request::Place`]).
    Place {
        scene: String,
        id: i64,
        /// Left, top, width, height.
        rect: [f32; 4],
        /// The canvas size.
        canvas: (f32, f32),
    },
    /// Which monitor a scene's display capture shows (the answer to [`Request::Screen`]).
    Screen {
        scene: String,
        /// The capture's `monitor_id` (a monitor interface path), when OBS stores one.
        id: Option<String>,
        /// Its `monitor` index.
        index: Option<usize>,
    },
    /// The sources of a scene with their places (the answer to [`Request::Items`]).
    Items(ac_core::obs::SceneDetail),
    /// Audio peaks in dBFS per input (only while meters are on).
    Levels(Vec<(String, f32)>),
    /// Where OBS records now (after aftercast set it).
    RecordDirectory(String),
    Stream(OutputState),
    Record {
        state: OutputState,
        path: Option<String>,
    },
    /// A [`Request`] failed.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    SetScene(String),
    /// Reloads every Browser source whose URL starts with one of these prefixes. OBS does
    /// not retry a page that failed to load (aftercast was not running yet).
    RefreshBrowsers(Vec<String>),
    /// Stacks the overlay sources (Browser sources whose URL starts with one of `prefixes`)
    /// in every scene: `order` holds overlay names, frontmost first.
    OverlayOrder {
        prefixes: Vec<String>,
        order: Vec<String>,
    },
    /// Starts OBS Virtual Camera unless it runs (a rehearse page shows it as the video).
    StartVirtualCam,
    /// Stops OBS Virtual Camera if it runs (`:end`).
    StopVirtualCam,
    Mute {
        input: String,
        muted: bool,
    },
    Volume {
        input: String,
        db: f64,
    },
    ItemEnabled {
        scene: String,
        id: i64,
        enabled: bool,
    },
    /// Asks where an item shows on the canvas; answered with [`Update::Place`] (nothing when
    /// OBS refuses, e.g. the item is gone).
    Place {
        scene: String,
        id: i64,
    },
    /// Lists the sources of `scene` with their places; answered with [`Update::Items`].
    Items {
        scene: String,
    },
    /// Changes a source of `scene` (then the list comes again).
    EditItem {
        scene: String,
        id: i64,
        edit: ac_core::obs::ItemEdit,
    },
    /// Asks which monitor the display capture in `scene` shows; answered with
    /// [`Update::Screen`] (nothing when the scene has none).
    Screen {
        scene: String,
    },
    /// Puts a Browser source in `scene`: a new input called `source` showing `url` at the
    /// canvas size, or (`existing`) a new item of the input of that name, so every scene
    /// shares one source. `audio` hands its sound to OBS's mixer.
    AddBrowser {
        scene: String,
        source: String,
        url: String,
        existing: bool,
        audio: bool,
    },
    Record(ac_core::command::OutputAction),
    Stream(bool),
    /// Receive audio levels (a high-volume event OBS only sends on request).
    Meters(bool),
    /// Record into this folder from now on, and after every reconnect. The folder OBS had
    /// before is put back when the request channel closes (aftercast exits).
    RecordDirectory(String),
    /// Delay OBS's own sources (cameras, captures, audio) so aftercast's overlays can show
    /// captions ahead of the voice. Undone when aftercast exits.
    Delay(Delay),
}

/// `[stream] delay_ms` as OBS applies it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Delay {
    /// 0 to 500 (OBS's Render Delay filter stops at 500 ms).
    pub ms: u32,
    /// Browser sources with these URL prefixes (aftercast's overlays) stay on time.
    pub overlays: Vec<String>,
    /// Scenes switched to at once (privacy); every other switch waits `ms`, so the delayed
    /// picture never shows what was on screen before it.
    pub instant_scenes: Vec<String>,
    /// The audio sync offset aftercast added per input, so a crash is undone next run.
    pub ledger: std::path::PathBuf,
}

/// Whether the last facecam place request failed (so a failure is logged once).
static PLACE_FAILING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Name of the Render Delay filter aftercast adds to video sources.
const DELAY_FILTER: &str = "aftercast delay";

/// Where OBS listens.
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
}

impl Target {
    /// Parses `ws://host:port` (the scheme is optional; the port defaults to 4455).
    pub fn parse(url: &str, password: Option<String>) -> Result<Self, String> {
        let rest = url
            .strip_prefix("ws://")
            .unwrap_or(url)
            .trim_end_matches('/');
        let (host, port) = match rest.rsplit_once(':') {
            Some((host, port)) => (
                host,
                port.parse()
                    .map_err(|_| format!("bad port in OBS url {url:?}"))?,
            ),
            None => (rest, 4455),
        };
        if host.is_empty() || url.starts_with("wss://") {
            return Err(format!(
                "OBS url must look like ws://127.0.0.1:4455, got {url:?}"
            ));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
            password: password.filter(|p| !p.is_empty()),
        })
    }
}

/// Connects to OBS and reconnects forever with backoff (1 s to 15 s), until the request
/// channel closes.
pub async fn run(
    target: Target,
    mut requests: mpsc::UnboundedReceiver<Request>,
    send: impl Fn(Update),
) {
    let mut backoff = BACKOFF_MIN;
    // Survive reconnects: meters stay on if a page still shows them, and the record folder
    // aftercast asked for (with the one OBS had before) is set again.
    let mut meters = false;
    let mut folder = RecordFolder::default();
    let mut delay = Delay::default();
    loop {
        let error = match connect(&target).await {
            Ok(client) => {
                backoff = BACKOFF_MIN;
                match serve(
                    &client,
                    &mut requests,
                    &send,
                    &mut meters,
                    &mut folder,
                    &mut delay,
                )
                .await
                {
                    Ok(()) => return,
                    Err(e) => e,
                }
            }
            Err(e) => e,
        };
        send(Update::Down {
            error,
            retry_in: backoff,
        });
        let wait = sleep(backoff);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                () = &mut wait => break,
                req = requests.recv() => match req {
                    Some(Request::Meters(on)) => meters = on,
                    Some(Request::RecordDirectory(dir)) => folder.wanted = Some(dir),
                    Some(Request::Delay(d)) => delay = d,
                    Some(_) => send(Update::Failed("OBS is not connected".into())),
                    None => return,
                },
            }
        }
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Connects once and reads the snapshot (`doctor`).
pub async fn probe(target: &Target) -> Result<Snapshot, String> {
    let client = connect(target).await?;
    snapshot(&client).await.map_err(|e| error_chain(&e))
}

async fn connect(target: &Target) -> Result<obws::Client, String> {
    obws::Client::connect(&target.host, target.port, target.password.as_deref())
        .await
        .map_err(|e| error_chain(&e))
}

/// Reports the snapshot, then forwards events and performs requests until the connection
/// drops (`Err`) or the request channel closes (`Ok`).
/// The record folder aftercast wants, and the one OBS had before (to put back).
#[derive(Default)]
struct RecordFolder {
    wanted: Option<String>,
    original: Option<String>,
    /// The recording format OBS had (its settings category and value), put back at the end.
    format: Option<(&'static str, String)>,
}

impl RecordFolder {
    /// Points OBS at the wanted folder, remembering OBS's own the first time.
    async fn apply(&mut self, client: &obws::Client, send: &impl Fn(Update)) {
        let Some(dir) = self.wanted.clone() else {
            return;
        };
        if self.original.is_none() {
            match client.config().record_directory().await {
                Ok(original) => self.original = Some(original),
                Err(e) => {
                    send(Update::Failed(format!(
                        "reading the record folder: {}",
                        error_chain(&e)
                    )));
                    return;
                }
            }
        }
        self.fragment(client).await;
        match client.config().set_record_directory(&dir).await {
            Ok(()) => send(Update::RecordDirectory(dir)),
            Err(e) => send(Update::Failed(format!(
                "recording into {dir}: {} (needs OBS 30.2 or newer)",
                error_chain(&e)
            ))),
        }
    }

    /// Records fragmented mp4: the file plays while it grows, so the rehearse page's player can
    /// go back in it. Takes effect with the next recording.
    async fn fragment(&mut self, client: &obws::Client) {
        if self.format.is_some() {
            return;
        }
        let profiles = client.profiles();
        let advanced = profiles
            .parameter("Output", "Mode")
            .await
            .ok()
            .and_then(|p| p.value)
            .is_some_and(|m| m == "Advanced");
        let category = if advanced { "AdvOut" } else { "SimpleOutput" };
        let Some(current) = profiles
            .parameter(category, "RecFormat2")
            .await
            .ok()
            .and_then(|p| p.value)
        else {
            return;
        };
        if current != "fragmented_mp4" {
            let set = obws::requests::profiles::SetParameter {
                category,
                name: "RecFormat2",
                value: Some("fragmented_mp4"),
            };
            if profiles.set_parameter(set).await.is_ok() {
                self.format = Some((category, current));
            }
        }
    }

    /// Puts OBS's own folder (and recording format) back.
    async fn restore(&mut self, client: &obws::Client) {
        if let Some((category, value)) = self.format.take() {
            let set = obws::requests::profiles::SetParameter {
                category,
                name: "RecFormat2",
                value: Some(&value),
            };
            let _ = client.profiles().set_parameter(set).await;
        }
        if let Some(original) = self.original.take() {
            let _ = client.config().set_record_directory(&original).await;
        }
    }
}

async fn serve(
    client: &obws::Client,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    send: &impl Fn(Update),
    meters: &mut bool,
    folder: &mut RecordFolder,
    delay: &mut Delay,
) -> Result<(), String> {
    // Subscribe before reading the snapshot so no change falls in between.
    let mut events = client.events().map_err(|e| error_chain(&e))?;
    if *meters {
        subscribe_meters(client, true)
            .await
            .map_err(|e| error_chain(&e))?;
    }
    folder.apply(client, send).await;
    delay.apply(client, delay.ms, send).await;
    // A delayed scene switch waiting for its time; a newer switch replaces it.
    let mut pending_scene: Option<(tokio::time::Instant, String)> = None;
    // Levels arrive about 20 times a second; pass on at most 10.
    let mut last_levels = tokio::time::Instant::now();
    send(Update::Connected(
        snapshot(client).await.map_err(|e| error_chain(&e))?,
    ));
    loop {
        tokio::select! {
            event = events.next() => match event {
                Some(ObsEvent::ExitStarted) => return Err("OBS is shutting down".into()),
                // Something was added, removed or renamed: list everything again.
                Some(event) if structural(&event) => {
                    // A new camera or microphone gets the delay too.
                    if matches!(event, ObsEvent::InputCreated { .. }) {
                        delay.apply(client, delay.ms, send).await;
                    }
                    send(Update::Media(media(client).await.map_err(|e| error_chain(&e))?));
                }
                Some(ObsEvent::InputVolumeMeters { inputs }) => {
                    if last_levels.elapsed() >= Duration::from_millis(100) {
                        last_levels = tokio::time::Instant::now();
                        let levels = inputs
                            .into_iter()
                            .map(|i| {
                                let peak = i.levels.iter().map(|c| c[1]).fold(0.0, f32::max);
                                (i.name, ac_core::obs::db(peak))
                            })
                            .collect();
                        send(Update::Levels(levels));
                    }
                }
                Some(event) => if let Some(update) = translate(event) { send(update) },
                None => return Err("connection to OBS lost".into()),
            },
            () = async {
                if let Some((at, _)) = &pending_scene {
                    tokio::time::sleep_until(*at).await;
                }
            }, if pending_scene.is_some() => {
                let (_, name) = pending_scene.take().expect("guarded");
                if let Err(e) = client.scenes().set_current_program_scene(name.as_str()).await {
                    send(Update::Failed(format!("switching to scene {name:?}: {}", error_chain(&e))));
                }
            }
            req = requests.recv() => match req {
                Some(Request::SetScene(name))
                    if delay.ms > 0 && !delay.instant_scenes.contains(&name) =>
                {
                    let at = tokio::time::Instant::now() + Duration::from_millis(delay.ms.into());
                    pending_scene = Some((at, name));
                }
                Some(Request::SetScene(name)) => {
                    pending_scene = None;
                    if let Err(e) = client.scenes().set_current_program_scene(name.as_str()).await {
                        send(Update::Failed(format!("switching to scene {name:?}: {}", error_chain(&e))));
                    }
                }
                Some(Request::StartVirtualCam) => {
                    let cam = client.virtual_cam();
                    if !cam.status().await.unwrap_or(false)
                        && let Err(e) = cam.start().await
                    {
                        send(Update::Failed(format!("starting the virtual camera: {}", error_chain(&e))));
                    }
                }
                Some(Request::StopVirtualCam) => {
                    let cam = client.virtual_cam();
                    if cam.status().await.unwrap_or(false)
                        && let Err(e) = cam.stop().await
                    {
                        send(Update::Failed(format!("stopping the virtual camera: {}", error_chain(&e))));
                    }
                }
                Some(Request::Mute { input, muted }) => {
                    if let Err(e) = client.inputs().set_muted(input.as_str().into(), muted).await {
                        send(Update::Failed(format!("muting {input:?}: {}", error_chain(&e))));
                    }
                }
                Some(Request::Volume { input, db }) => {
                    let volume = obws::requests::inputs::Volume::Db(db as f32);
                    if let Err(e) = client.inputs().set_volume(input.as_str().into(), volume).await {
                        send(Update::Failed(format!("volume of {input:?}: {}", error_chain(&e))));
                    }
                }
                Some(Request::ItemEnabled { scene, id, enabled }) => {
                    let set = obws::requests::scene_items::SetEnabled {
                        scene: scene.as_str().into(),
                        item_id: id,
                        enabled,
                    };
                    if let Err(e) = client.scene_items().set_enabled(set).await {
                        send(Update::Failed(format!("showing a source in {scene:?}: {}", error_chain(&e))));
                    }
                }
                Some(Request::Items { scene }) => match scene_detail(client, &scene).await {
                    Ok(d) => send(Update::Items(d)),
                    Err(e) => send(Update::Failed(format!("listing {scene:?}: {}", error_chain(&e)))),
                },
                Some(Request::EditItem { scene, id, edit }) => {
                    if let Err(e) = edit_item(client, &scene, id, &edit).await {
                        send(Update::Failed(format!("editing a source in {scene:?}: {}", error_chain(&e))));
                    }
                    if let Ok(d) = scene_detail(client, &scene).await {
                        send(Update::Items(d));
                    }
                }
                Some(Request::Place { scene, id }) => {
                    let t = client.scene_items().transform(scene.as_str().into(), id).await;
                    let v = client.config().video_settings().await;
                    if let (Err(e), _) | (_, Err(e)) = (&t, &v) {
                        // Once per failure streak: the controller asks every second.
                        if !PLACE_FAILING.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!("facecam: OBS would not tell where {id} in {scene:?} is: {}", error_chain(e));
                        }
                    }
                    if let (Ok(t), Ok(v)) = (t, v) {
                        PLACE_FAILING.store(false, std::sync::atomic::Ordering::Relaxed);
                        let size = if t.bounds_type == obws::common::BoundsType::None {
                            (
                                (t.source_width - (t.crop_left + t.crop_right) as f32) * t.scale_x,
                                (t.source_height - (t.crop_top + t.crop_bottom) as f32) * t.scale_y,
                            )
                        } else {
                            (t.bounds_width, t.bounds_height)
                        };
                        let rect = ac_core::obs::placed(
                            (t.position_x, t.position_y),
                            size,
                            u8::from(t.alignment),
                        );
                        send(Update::Place {
                            scene,
                            id,
                            rect,
                            canvas: (v.base_width as f32, v.base_height as f32),
                        });
                    }
                }
                Some(Request::Screen { scene }) => {
                    let Ok(items) = client.scene_items().list(scene.as_str().into()).await else {
                        continue;
                    };
                    for item in items {
                        let name = item.source_name.as_str();
                        let Ok(got) = client
                            .inputs()
                            .settings::<serde_json::Value>(name.into())
                            .await
                        else {
                            continue;
                        };
                        if got.kind != "monitor_capture" {
                            continue;
                        }
                        let id = got.settings["monitor_id"].as_str().map(str::to_owned);
                        let index = got.settings["monitor"].as_u64().map(|i| i as usize);
                        send(Update::Screen { scene, id, index });
                        break;
                    }
                }
                Some(Request::AddBrowser { scene, source, url, existing, audio }) => {
                    let done = if existing {
                        let item = obws::requests::scene_items::CreateSceneItem {
                            scene: scene.as_str().into(),
                            source: source.as_str().into(),
                            enabled: Some(true),
                        };
                        client.scene_items().create(item).await.map(|_| ())
                    } else {
                        let (w, h) = client
                            .config()
                            .video_settings()
                            .await
                            .map_or((1920, 1080), |v| (v.base_width, v.base_height));
                        let create = obws::requests::inputs::Create {
                            scene: scene.as_str().into(),
                            input: source.as_str(),
                            kind: "browser_source",
                            settings: Some(serde_json::json!({
                                "url": url, "width": w, "height": h, "reroute_audio": audio
                            })),
                            enabled: Some(true),
                        };
                        client.inputs().create(create).await.map(|_| ())
                    };
                    if let Err(e) = done {
                        send(Update::Failed(format!("adding {source:?} to {scene:?}: {}", error_chain(&e))));
                    }
                }
                Some(Request::RecordDirectory(dir)) => {
                    folder.wanted = Some(dir);
                    folder.apply(client, send).await;
                }
                Some(Request::Delay(d)) => {
                    *delay = d;
                    delay.apply(client, delay.ms, send).await;
                }
                Some(Request::Meters(on)) => {
                    *meters = on;
                    if let Err(e) = subscribe_meters(client, on).await {
                        send(Update::Failed(format!("audio meters: {}", error_chain(&e))));
                    }
                }
                Some(Request::Record(action)) => {
                    use ac_core::command::OutputAction as A;
                    let rec = client.recording();
                    let result = match action {
                        A::Start => rec.start().await,
                        A::Stop => rec.stop().await.map(|_| ()),
                        A::Pause => rec.pause().await,
                        A::Resume => rec.resume().await,
                    };
                    if let Err(e) = result {
                        send(Update::Failed(format!("recording: {}", error_chain(&e))));
                    }
                }
                Some(Request::Stream(on)) => {
                    let result = if on {
                        client.streaming().start().await
                    } else {
                        client.streaming().stop().await
                    };
                    if let Err(e) = result {
                        send(Update::Failed(format!("streaming: {}", error_chain(&e))));
                    }
                }
                Some(Request::OverlayOrder { prefixes, order }) => {
                    if let Err(e) = overlay_order(client, &prefixes, &order).await {
                        send(Update::Failed(format!("stacking overlays: {}", error_chain(&e))));
                    }
                }
                Some(Request::RefreshBrowsers(prefixes)) => {
                    if let Err(e) = refresh_browsers(client, &prefixes).await {
                        send(Update::Failed(format!("refreshing overlays: {}", error_chain(&e))));
                    }
                }
                // aftercast is exiting: give OBS its own record folder back.
                None => {
                    folder.restore(client).await;
                    delay.apply(client, 0, send).await;
                    return Ok(());
                }
            },
        }
    }
}

impl Delay {
    /// Sets every source's delay to `ms` (0 removes it): a Render Delay filter on video
    /// sources, the audio sync offset on audio ones, minus what aftercast added before.
    async fn apply(&self, client: &obws::Client, ms: u32, send: &impl Fn(Update)) {
        if self.ledger.as_os_str().is_empty() {
            return; // never asked for
        }
        if let Err(e) = self.try_apply(client, ms.min(500)).await {
            send(Update::Failed(format!("stream delay: {}", error_chain(&e))));
        }
    }

    async fn try_apply(&self, client: &obws::Client, ms: u32) -> obws::error::Result<()> {
        let mut added: std::collections::BTreeMap<String, i64> =
            std::fs::read_to_string(&self.ledger)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        let want = i64::from(ms);
        for input in client.inputs().list(None).await? {
            let name = input.id.name.as_str();
            if input.kind == "browser_source" && self.is_overlay(client, name).await {
                continue;
            }
            // Inputs without audio refuse this.
            if let Ok(offset) = client.inputs().audio_sync_offset(name.into()).await {
                let had = added.get(name).copied().unwrap_or(0);
                if had != want {
                    let base = offset.whole_milliseconds() as i64 - had;
                    let to = time::Duration::milliseconds(base + want);
                    client
                        .inputs()
                        .set_audio_sync_offset(name.into(), to)
                        .await?;
                    if want == 0 {
                        added.remove(name);
                    } else {
                        added.insert(name.to_owned(), want);
                    }
                    let json = serde_json::to_string(&added).unwrap_or_default();
                    let _ = std::fs::write(&self.ledger, json);
                }
            }
            if !has_live_video(&input.kind) {
                continue;
            }
            let filters = client.filters();
            let settings = serde_json::json!({ "delay_ms": ms });
            match filters.get(name.into(), DELAY_FILTER).await.ok() {
                Some(_) if ms == 0 => filters.remove(name.into(), DELAY_FILTER).await?,
                Some(f) if f.settings.get("delay_ms") != Some(&settings["delay_ms"]) => {
                    filters
                        .set_settings(obws::requests::filters::SetSettings {
                            source: name.into(),
                            filter: DELAY_FILTER,
                            settings,
                            overlay: Some(true),
                        })
                        .await?;
                }
                None if ms > 0 => {
                    filters
                        .create(obws::requests::filters::Create {
                            source: name.into(),
                            filter: DELAY_FILTER,
                            kind: "gpu_delay",
                            settings: Some(settings),
                        })
                        .await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn is_overlay(&self, client: &obws::Client, name: &str) -> bool {
        #[derive(serde::Deserialize)]
        struct Browser {
            url: Option<String>,
        }
        client
            .inputs()
            .settings::<Browser>(name.into())
            .await
            .ok()
            .and_then(|s| s.settings.url)
            .is_some_and(|u| self.overlays.iter().any(|p| u.starts_with(p.as_str())))
    }
}

/// Sources whose picture moves on its own (cameras, captures, media, other pages) get the
/// Render Delay filter. Audio-only and still sources (images, colors, text) do not: each
/// filter holds up to 500 ms of frames in video memory.
fn has_live_video(kind: &str) -> bool {
    const STILL_OR_AUDIO: [&str; 10] = [
        "wasapi",
        "coreaudio",
        "pulse",
        "alsa",
        "jack",
        "sck_audio",
        "image_source",
        "color_source",
        "text_",
        "slideshow",
    ];
    !STILL_OR_AUDIO.iter().any(|p| kind.starts_with(p))
}

/// The sources of a scene with their places on OBS's canvas.
async fn scene_detail(
    client: &obws::Client,
    scene: &str,
) -> obws::error::Result<ac_core::obs::SceneDetail> {
    let video = client.config().video_settings().await?;
    let items = client.scene_items();
    let mut out = Vec::new();
    for it in items.list(scene.into()).await? {
        let t = items.transform(scene.into(), it.id).await?;
        let enabled = items.enabled(scene.into(), it.id).await?;
        let locked = items.locked(scene.into(), it.id).await?;
        let kind = match (it.is_group, it.input_kind) {
            (Some(true), _) => "group".to_owned(),
            (_, Some(kind)) => kind,
            _ => "scene".to_owned(),
        };
        out.push(ac_core::obs::ItemDetail {
            id: it.id,
            source: it.source_name,
            kind,
            enabled,
            locked,
            index: it.index,
            x: t.position_x,
            y: t.position_y,
            width: t.width,
            height: t.height,
            scale_x: t.scale_x,
            scale_y: t.scale_y,
            rotation: t.rotation,
            source_w: t.source_width,
            source_h: t.source_height,
            alignment: u8::from(t.alignment),
            bounds: t.bounds_type != obws::common::BoundsType::None,
            bounds_w: t.bounds_width,
            bounds_h: t.bounds_height,
            crop: [t.crop_left, t.crop_right, t.crop_top, t.crop_bottom],
        });
    }
    Ok(ac_core::obs::SceneDetail {
        scene: scene.to_owned(),
        canvas: [video.base_width as f32, video.base_height as f32],
        items: out,
    })
}

/// Applies an editor change to a source of a scene.
async fn edit_item(
    client: &obws::Client,
    scene: &str,
    id: i64,
    e: &ac_core::obs::ItemEdit,
) -> obws::error::Result<()> {
    use obws::requests::scene_items as r;
    let items = client.scene_items();
    if e.remove {
        return items.remove(scene.into(), id).await;
    }
    if e.duplicate {
        let dup = r::Duplicate {
            scene: scene.into(),
            item_id: id,
            destination: None,
        };
        items.duplicate(dup).await?;
        return Ok(());
    }
    if let Some(enabled) = e.enabled {
        items
            .set_enabled(r::SetEnabled {
                scene: scene.into(),
                item_id: id,
                enabled,
            })
            .await?;
    }
    if let Some(locked) = e.locked {
        items
            .set_locked(r::SetLocked {
                scene: scene.into(),
                item_id: id,
                locked,
            })
            .await?;
    }
    if let Some(index) = e.index {
        items
            .set_index(r::SetIndex {
                scene: scene.into(),
                item_id: id,
                index,
            })
            .await?;
    }
    let transform = r::SceneItemTransform {
        position: (e.x.is_some() || e.y.is_some()).then_some(r::Position { x: e.x, y: e.y }),
        rotation: e.rotation,
        scale: (e.scale_x.is_some() || e.scale_y.is_some()).then_some(r::Scale {
            x: e.scale_x,
            y: e.scale_y,
        }),
        alignment: None,
        bounds: (e.bounds_w.is_some() || e.bounds_h.is_some()).then_some(r::Bounds {
            width: e.bounds_w,
            height: e.bounds_h,
            ..Default::default()
        }),
        crop: e.crop.map(|c| r::Crop {
            left: Some(c[0]),
            right: Some(c[1]),
            top: Some(c[2]),
            bottom: Some(c[3]),
        }),
    };
    if e.x.is_some()
        || e.y.is_some()
        || e.rotation.is_some()
        || e.scale_x.is_some()
        || e.scale_y.is_some()
        || e.bounds_w.is_some()
        || e.bounds_h.is_some()
        || e.crop.is_some()
    {
        items
            .set_transform(r::SetTransform {
                scene: scene.into(),
                item_id: id,
                transform,
            })
            .await?;
    }
    if let Some(name) = &e.name {
        client
            .inputs()
            .set_name(e.source.as_str().into(), name)
            .await?;
    }
    Ok(())
}

/// Stacks aftercast's overlay sources (Browser sources whose URL starts with one of
/// `prefixes`) in every scene in `order` (overlay names, frontmost first), in the slots
/// they already hold: every other source keeps its place.
async fn overlay_order(
    client: &obws::Client,
    prefixes: &[String],
    order: &[String],
) -> obws::error::Result<()> {
    #[derive(serde::Deserialize)]
    struct Browser {
        url: Option<String>,
    }
    // Input name to rank (0 is frontmost).
    let mut rank = std::collections::HashMap::new();
    for input in client.inputs().list(Some("browser_source")).await? {
        let name = input.id.name;
        let url = client
            .inputs()
            .settings::<Browser>(name.as_str().into())
            .await?
            .settings
            .url;
        let overlay = url.and_then(|u| {
            let rest = prefixes.iter().find_map(|p| u.strip_prefix(p.as_str()))?;
            Some(
                rest.split(['?', '#', '/'])
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
            )
        });
        if let Some(r) = overlay.and_then(|o| order.iter().position(|n| *n == o)) {
            rank.insert(name, r);
        }
    }
    if rank.is_empty() {
        return Ok(());
    }
    for scene in client.scenes().list().await?.scenes {
        let id = scene.id.name.as_str();
        let mut items = client.scene_items().list(id.into()).await?;
        items.sort_by_key(|i| i.index); // bottom first
        let slots: Vec<usize> = (0..items.len())
            .filter(|&i| rank.contains_key(&items[i].source_name))
            .collect();
        if slots.len() < 2 {
            continue;
        }
        let mut overlays: Vec<_> = slots.iter().map(|&i| items[i].clone()).collect();
        overlays.sort_by_key(|i| std::cmp::Reverse(rank[&i.source_name])); // back first
        let mut want = items.clone();
        for (&slot, item) in slots.iter().zip(overlays) {
            want[slot] = item;
        }
        // Bottom up: moving an item down only shifts the ones not placed yet.
        let mut now: Vec<i64> = items.iter().map(|i| i.id).collect();
        for (index, item) in want.iter().enumerate() {
            if now[index] == item.id {
                continue;
            }
            let from = now.iter().position(|&i| i == item.id).expect("same items");
            let set = obws::requests::scene_items::SetIndex {
                scene: id.into(),
                item_id: item.id,
                index: index as u32,
            };
            client.scene_items().set_index(set).await?;
            let moved = now.remove(from);
            now.insert(index, moved);
        }
    }
    Ok(())
}

async fn refresh_browsers(client: &obws::Client, prefixes: &[String]) -> obws::error::Result<()> {
    #[derive(serde::Deserialize)]
    struct Browser {
        url: Option<String>,
    }
    for input in client.inputs().list(Some("browser_source")).await? {
        let name = input.id.name.as_str();
        let url = client
            .inputs()
            .settings::<Browser>(name.into())
            .await?
            .settings
            .url;
        if url.is_some_and(|u| prefixes.iter().any(|p| u.starts_with(p.as_str()))) {
            client
                .inputs()
                .press_properties_button(name.into(), "refreshnocache")
                .await?;
        }
    }
    Ok(())
}

/// Scenes in OBS's order, every audio input with its mute and volume, and the sources of
/// every scene with their visibility.
async fn media(client: &obws::Client) -> obws::error::Result<Media> {
    let mut scenes = client.scenes().list().await?.scenes;
    // OBS numbers scenes from the bottom of its list; show them top first, as OBS does.
    scenes.sort_by_key(|s| std::cmp::Reverse(s.index));
    let scenes: Vec<String> = scenes.into_iter().map(|s| s.id.name).collect();
    let mut items = Vec::new();
    // Which overlay each Browser source shows, asked once per source.
    let mut overlays: std::collections::HashMap<String, Option<String>> = Default::default();
    for scene in &scenes {
        let mut list = client.scene_items().list(scene.as_str().into()).await?;
        list.sort_by_key(|i| std::cmp::Reverse(i.index));
        for item in list {
            let enabled = client
                .scene_items()
                .enabled(scene.as_str().into(), item.id)
                .await?;
            let overlay = if item.input_kind.as_deref() == Some("browser_source") {
                if !overlays.contains_key(&item.source_name) {
                    let url = client
                        .inputs()
                        .settings::<serde_json::Value>(item.source_name.as_str().into())
                        .await
                        .ok()
                        .and_then(|s| {
                            s.settings["url"]
                                .as_str()
                                .and_then(ac_core::obs::overlay_name)
                        });
                    overlays.insert(item.source_name.clone(), url);
                }
                overlays[&item.source_name].clone()
            } else {
                None
            };
            items.push(Item {
                scene: scene.clone(),
                source: item.source_name,
                id: item.id,
                enabled,
                overlay,
            });
        }
    }
    let mut audio = Vec::new();
    for input in client.inputs().list(None).await? {
        let name = input.id.name;
        // Inputs without audio refuse GetInputMute.
        let Ok(muted) = client.inputs().muted(name.as_str().into()).await else {
            continue;
        };
        let volume_db = client
            .inputs()
            .volume(name.as_str().into())
            .await
            .map_or(0.0, |v| f64::from(v.db));
        audio.push(Audio {
            name,
            kind: input.kind,
            muted,
            volume_db,
        });
    }
    let mut devices = Vec::new();
    for input in client.inputs().list(None).await? {
        // The setting holding the device, per capture kind.
        let (video, property) = match input.kind.as_str() {
            "dshow_input" => (true, "video_device_id"),
            "av_capture_input" | "macos-avcapture" | "av_capture_input_v2" => (true, "device"),
            "v4l2_input" => (true, "device_id"),
            kind if kind.contains("input_capture") => (false, "device_id"),
            _ => continue,
        };
        let name = input.id.name;
        if let Some(device) = device_name(client, &name, property).await {
            devices.push(ac_core::obs::Device {
                input: name,
                video,
                name: device,
            });
        }
    }
    Ok(Media {
        scenes,
        audio,
        items,
        devices,
    })
}

/// The name OBS shows for the device `input` uses: the list item matching its setting.
async fn device_name(client: &obws::Client, input: &str, property: &str) -> Option<String> {
    let settings = client
        .inputs()
        .settings::<serde_json::Value>(input.into())
        .await
        .ok()?
        .settings;
    // Unset means OBS's default device.
    let value = settings
        .get(property)
        .cloned()
        .unwrap_or_else(|| serde_json::Value::String("default".into()));
    let items = client
        .inputs()
        .properties_list_property_items(input.into(), property)
        .await
        .ok()?;
    items
        .into_iter()
        .find(|i| i.value == value)
        .map(|i| i.name)
        .filter(|n| !n.is_empty())
}

/// Adds or removes the high-volume audio meters event from the default subscriptions.
async fn subscribe_meters(client: &obws::Client, on: bool) -> obws::error::Result<()> {
    use obws::requests::EventSubscription as E;
    let events = if on {
        E::ALL | E::INPUT_VOLUME_METERS
    } else {
        E::ALL
    };
    client.reidentify(events).await
}

/// Events after which the lists of scenes, inputs and sources must be read again.
fn structural(event: &ObsEvent) -> bool {
    matches!(
        event,
        ObsEvent::InputCreated { .. }
            | ObsEvent::InputRemoved { .. }
            | ObsEvent::InputNameChanged { .. }
            | ObsEvent::SceneItemCreated { .. }
            | ObsEvent::SceneItemRemoved { .. }
            | ObsEvent::SceneCreated { .. }
            | ObsEvent::SceneRemoved { .. }
            | ObsEvent::SceneNameChanged { .. }
            | ObsEvent::SceneListChanged { .. }
    )
}

async fn snapshot(client: &obws::Client) -> obws::error::Result<Snapshot> {
    let media = media(client).await?;
    let stream = client.streaming().status().await?;
    let record = client.recording().status().await?;
    Ok(Snapshot {
        scene: Some(client.scenes().current_program_scene().await?.id.name),
        scenes: media.scenes.clone(),
        media,
        stream: Output {
            active: stream.active,
            paused: false,
            reconnecting: stream.reconnecting,
            duration_s: stream.duration.as_seconds_f64(),
        },
        record: Output {
            active: record.active,
            paused: record.paused,
            reconnecting: false,
            duration_s: record.duration.as_seconds_f64(),
        },
    })
}

fn translate(event: ObsEvent) -> Option<Update> {
    Some(match event {
        ObsEvent::CurrentProgramSceneChanged { id } => Update::Scene(id.name),
        ObsEvent::InputMuteStateChanged { id, muted } => Update::Mute {
            input: id.name,
            muted,
        },
        ObsEvent::InputVolumeChanged { id, db, .. } => Update::Volume { input: id.name, db },
        ObsEvent::SceneItemEnableStateChanged {
            scene,
            item_id,
            enabled,
        } => Update::ItemEnabled {
            scene: scene.name,
            id: item_id as i64,
            enabled,
        },
        ObsEvent::StreamStateChanged { state, .. } => Update::Stream(output_state(state)?),
        ObsEvent::RecordStateChanged { state, path, .. } => Update::Record {
            state: output_state(state)?,
            path,
        },
        ObsEvent::RecordFileChanged { path } => Update::Record {
            state: OutputState::FileChanged,
            path: Some(path),
        },
        _ => return None,
    })
}

/// Final output states only; `starting`/`stopping` are transitions.
fn output_state(state: ObsOutput) -> Option<OutputState> {
    Some(match state {
        ObsOutput::Started | ObsOutput::Reconnected => OutputState::Started,
        ObsOutput::Stopped => OutputState::Stopped,
        ObsOutput::Reconnecting => OutputState::Reconnecting,
        ObsOutput::Paused => OutputState::Paused,
        ObsOutput::Resumed => OutputState::Resumed,
        _ => return None,
    })
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        // Some errors already include their source in their message.
        let s_text = s.to_string();
        if !text.contains(&s_text) {
            text = format!("{text}: {s_text}");
        }
        source = s.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let t = Target::parse("ws://127.0.0.1:4455", Some(String::new())).unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.password),
            ("127.0.0.1", 4455, None)
        );
        let t = Target::parse("localhost", Some("pw".into())).unwrap();
        assert_eq!((t.host.as_str(), t.port), ("localhost", 4455));
        assert!(Target::parse("ws://h:x", None).is_err());
        assert!(Target::parse("wss://h:1", None).is_err());
    }

    #[test]
    fn translates_obs_events() {
        let json = r#"{"outputActive":true,"outputState":"OBS_WEBSOCKET_OUTPUT_STARTED","outputPath":"D:/a.mkv"}"#;
        let fields: serde_json::Value = serde_json::from_str(json).unwrap();
        let event: ObsEvent = serde_json::from_value(serde_json::json!({
            "eventType": "RecordStateChanged",
            "eventData": fields,
        }))
        .unwrap();
        assert_eq!(
            translate(event),
            Some(Update::Record {
                state: OutputState::Started,
                path: Some("D:/a.mkv".into())
            })
        );
        assert_eq!(output_state(ObsOutput::Stopping), None);
        assert_eq!(
            output_state(ObsOutput::Reconnected),
            Some(OutputState::Started)
        );
    }
}
