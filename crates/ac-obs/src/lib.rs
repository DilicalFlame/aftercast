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
    /// Starts OBS Virtual Camera unless it runs (a rehearsal page shows it as the video).
    StartVirtualCam,
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
    Record(ac_core::command::OutputAction),
    Stream(bool),
    /// Receive audio levels (a high-volume event OBS only sends on request).
    Meters(bool),
    /// Record into this folder from now on, and after every reconnect. The folder OBS had
    /// before is put back when the request channel closes (aftercast exits).
    RecordDirectory(String),
}

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
    loop {
        let error = match connect(&target).await {
            Ok(client) => {
                backoff = BACKOFF_MIN;
                match serve(&client, &mut requests, &send, &mut meters, &mut folder).await {
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
        match client.config().set_record_directory(&dir).await {
            Ok(()) => send(Update::RecordDirectory(dir)),
            Err(e) => send(Update::Failed(format!(
                "recording into {dir}: {} (needs OBS 30.2 or newer)",
                error_chain(&e)
            ))),
        }
    }

    /// Puts OBS's own folder back.
    async fn restore(&mut self, client: &obws::Client) {
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
) -> Result<(), String> {
    // Subscribe before reading the snapshot so no change falls in between.
    let mut events = client.events().map_err(|e| error_chain(&e))?;
    if *meters {
        subscribe_meters(client, true)
            .await
            .map_err(|e| error_chain(&e))?;
    }
    folder.apply(client, send).await;
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
            req = requests.recv() => match req {
                Some(Request::SetScene(name)) => {
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
                Some(Request::RecordDirectory(dir)) => {
                    folder.wanted = Some(dir);
                    folder.apply(client, send).await;
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
                Some(Request::RefreshBrowsers(prefixes)) => {
                    if let Err(e) = refresh_browsers(client, &prefixes).await {
                        send(Update::Failed(format!("refreshing overlays: {}", error_chain(&e))));
                    }
                }
                // aftercast is exiting: give OBS its own record folder back.
                None => {
                    folder.restore(client).await;
                    return Ok(());
                }
            },
        }
    }
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
    for scene in &scenes {
        let mut list = client.scene_items().list(scene.as_str().into()).await?;
        list.sort_by_key(|i| std::cmp::Reverse(i.index));
        for item in list {
            let enabled = client
                .scene_items()
                .enabled(scene.as_str().into(), item.id)
                .await?;
            items.push(Item {
                scene: scene.clone(),
                source: item.source_name,
                id: item.id,
                enabled,
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
