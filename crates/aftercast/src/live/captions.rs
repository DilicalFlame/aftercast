//! Live captions: `captions.py` turns the microphone into text (faster-whisper, streamed
//! the WhisperLive way); lines go to the `captions` overlay and `captions.jsonl`.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::Arc;

use ac_core::Home;
use ac_core::config::Config;
use ac_core::time::{Clock, Timestamp};
use ac_server::{Action, OverlayMsg};
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

use super::{Controller, Msg};

#[derive(Debug)]
pub struct Caption {
    pub text: String,
    pub done: bool,
    pub start: Timestamp,
    pub end: Timestamp,
}

/// A running helper. Dropping it closes the helper's stdin, which stops it.
pub(super) struct Running {
    args: Vec<String>,
    _stdin: ChildStdin,
}

/// What the helper runs with; when a setting changes these, it is restarted.
fn args(config: &Config) -> Vec<String> {
    let c = &config.captions;
    let mut prompt = format!("{}, a live coding stream.", config.channel.series);
    if !config.post.vocabulary.is_empty() {
        prompt += &format!(" Terms: {}.", config.post.vocabulary.join(", "));
    }
    let ffmpeg = config.paths.ffmpeg.to_string_lossy().into_owned();
    [
        ("--device", c.device.as_str()),
        ("--model", &c.model),
        ("--compute", &config.post.whisper_compute_type),
        ("--language", &config.post.language),
        ("--prompt", &prompt),
        ("--ffmpeg", &ffmpeg),
    ]
    .iter()
    .flat_map(|(k, v)| [(*k).to_owned(), (*v).to_owned()])
    .collect()
}

/// Starts the helper when `[captions] enabled`; its lines are read on their own thread.
pub(super) fn spawn(
    home: &Home,
    config: &Config,
    clock: Arc<Clock>,
    tx: UnboundedSender<Msg>,
) -> Option<Running> {
    if !config.captions.enabled {
        return None;
    }
    if config.captions.device.is_empty() {
        tracing::warn!("captions: set [captions].device to your microphone's name");
        return None;
    }
    let script = ac_post::tool::write_helpers(home)
        .inspect_err(|e| tracing::warn!("captions: {e}"))
        .ok()?
        .join("captions.py");
    let log_path = home.join("logs/captions.log");
    let log = fs::create_dir_all(home.join("logs")).and_then(|()| File::create(&log_path));
    let args = args(config);
    let mut cmd = Command::new(home.python(&config.paths.python));
    cmd.arg(script)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log.map_or_else(|_| Stdio::null(), Stdio::from));
    let models = home.models_dir();
    if models.exists() {
        cmd.arg("--models").arg(models);
    }
    let mut child = cmd
        .spawn()
        .inspect_err(|e| tracing::warn!("captions: {e}"))
        .ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    std::thread::spawn(move || {
        let mut t0 = clock.now_ts();
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(l) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if l["ready"] == true {
                t0 = clock.now_ts();
                continue;
            }
            let secs = |k: &str| l[k].as_f64().unwrap_or(0.0);
            let caption = Caption {
                text: l["text"].as_str().unwrap_or("").to_owned(),
                done: l["final"] == true,
                start: t0.add_secs(secs("start")),
                end: t0.add_secs(secs("end")),
            };
            if tx.send(Msg::Caption(caption)).is_err() {
                return;
            }
        }
        let status = child.wait().map(|s| s.to_string()).unwrap_or_default();
        tracing::info!("captions stopped ({status}), see {}", log_path.display());
    });
    Some(Running {
        args,
        _stdin: stdin,
    })
}

impl Controller {
    /// After a settings change: starts, stops or restarts the helper to match.
    pub(super) fn captions_reload(&mut self) {
        let want = self.config.captions.enabled.then(|| args(&self.config));
        if want.as_ref() == self.captions.as_ref().map(|r| &r.args) {
            return;
        }
        // Dropping the old one stops it before the new one loads its model.
        self.captions = None;
        self.captions = spawn(
            &self.home,
            &self.config,
            self.clock.clone(),
            self.msg_tx.clone(),
        );
    }

    /// Like typed keys: nothing before the session starts, nothing in privacy mode.
    pub(super) fn on_caption(&mut self, c: Caption) {
        if !self.begun || self.state.privacy {
            return;
        }
        if !self.overlays_off.contains("captions") {
            self.overlays.send(OverlayMsg {
                overlay: "captions".into(),
                action: Action::Update,
                data: json!({ "text": c.text, "final": c.done }),
            });
        }
        if c.done {
            let line = json!({ "t": c.start, "end": c.end, "text": c.text });
            let path = self.session.dir.join("captions.jsonl");
            let written = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                tracing::error!("{}: {e}", path.display());
            }
        }
    }
}
