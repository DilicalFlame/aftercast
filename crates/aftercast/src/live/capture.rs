//! Key and shell capture in the controller (specs/10-capture.md): filters each key press,
//! holds keys in the secret guard, writes `keys.jsonl.zst` and tails the shell spool
//! into `shell.jsonl`.

use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ac_capture::keys::{self, Filter, Guard, RawKey};
use ac_capture::shell::{self, ShellLine, Spool};
use ac_core::Home;
use ac_core::config::Config;
use ac_core::event::{CaptureKeys, CaptureSource, Event, EventKind};
use ac_core::session::Session;
use ac_core::time::Timestamp;
use ac_server::{Action, OverlayMsg};
use serde_json::json;

use super::Controller;

/// Held keys are written this often (spec: every 2 s).
const KEYS_WRITE_EVERY: Duration = Duration::from_secs(2);
/// Minutes shown in the keys/min sparkline.
pub const SPARK_MINUTES: usize = 10;

pub(super) struct Capture {
    filter: Filter,
    guard: Guard,
    keys_file: PathBuf,
    last_write: Instant,
    /// How the keyboard hook started; `None` until it reports (never in headless runs).
    pub hook: Option<Result<(), String>>,
    /// Stored keys per minute (unix minute, count), oldest first.
    minutes: VecDeque<(i64, u32)>,
    spool: Option<Spool>,
    shell_file: PathBuf,
    /// The last command in `shell.jsonl` (time, redacted text); older spool lines and
    /// this one are already copied.
    shell_last: (Timestamp, String),
}

impl Capture {
    pub fn new(home: &Home, config: &Config, session: &Session) -> Self {
        let hotkeys = &config.hotkeys;
        let shell_file = session.dir.join("shell.jsonl");
        let spool = config.capture.shell.then(|| {
            let path = home.path(&config.capture.shell_spool);
            if let Err(e) = shell::trim(&path) {
                tracing::warn!("trimming {}: {e}", path.display());
            }
            Spool::new(&path)
        });
        // Resuming: continue after the last command already copied.
        let shell_last = fs::read_to_string(&shell_file)
            .ok()
            .and_then(|s| {
                s.lines()
                    .rev()
                    .find_map(|l| serde_json::from_str::<ShellLine>(l).ok())
            })
            .map_or((session.meta.created_at, String::new()), |l| (l.t, l.cmd));
        Self {
            filter: Filter::new(
                &config.capture,
                &[
                    &hotkeys.mark,
                    &hotkeys.checkpoint,
                    &hotkeys.privacy,
                    &hotkeys.brb,
                ],
            ),
            guard: Guard::new(&config.capture.secret_patterns),
            keys_file: session.dir.join("keys.jsonl.zst"),
            last_write: Instant::now(),
            hook: None,
            minutes: VecDeque::new(),
            spool,
            shell_file,
            shell_last,
        }
    }
}

impl Controller {
    /// Reports the configured key capture state once at startup.
    pub(super) fn capture_start(&mut self) {
        if !self.config.capture.keys {
            self.emit(
                "capture",
                EventKind::CaptureState {
                    keys: CaptureKeys::Off,
                    reason: Some("capture.keys = false".into()),
                },
            );
        }
    }

    pub(super) fn keys_hook(&mut self, result: Result<(), String>) {
        let (keys, reason) = match &result {
            Ok(()) => (CaptureKeys::On, None),
            Err(e) => (CaptureKeys::Error, Some(e.clone())),
        };
        self.capture.hook = Some(result);
        self.emit("capture", EventKind::CaptureState { keys, reason });
    }

    pub(super) fn on_key(&mut self, raw: RawKey) {
        if !self.config.capture.keys {
            return;
        }
        let Some(rec) = self.capture.filter.check(&raw, self.state.privacy) else {
            return;
        };
        let minute = rec.t.unix_ms().div_euclid(60_000);
        match self.capture.minutes.back_mut() {
            Some((m, n)) if *m == minute => *n += 1,
            _ => {
                self.capture.minutes.push_back((minute, 1));
                if self.capture.minutes.len() > SPARK_MINUTES {
                    self.capture.minutes.pop_front();
                }
            }
        }
        if !self.overlays_off.contains("keys") {
            let data = match (rec.combo(), &rec.ch) {
                (Some(combo), _) => json!({ "combo": combo }),
                (None, Some(ch)) => json!({ "ch": ch }),
                (None, None) if rec.k == "BackSpace" => json!({ "back": true }),
                (None, None) => json!({ "key": rec.k }),
            };
            self.overlays.send(OverlayMsg {
                overlay: "keys".into(),
                action: Action::Update,
                data,
            });
        }
        if self.capture.guard.push(rec) {
            self.emit(
                "capture",
                EventKind::CaptureSecretDropped {
                    source: CaptureSource::Keys,
                },
            );
        }
    }

    /// Writes keys that can no longer be part of a secret (every 2 s), or all at the end.
    pub(super) fn write_keys(&mut self, all: bool) {
        let c = &mut self.capture;
        if !all && c.last_write.elapsed() < KEYS_WRITE_EVERY {
            return;
        }
        c.last_write = Instant::now();
        let records = if all {
            c.guard.take_all()
        } else {
            c.guard.take_cold(self.clock.now_ts())
        };
        if let Err(e) = keys::append(&c.keys_file, &records) {
            tracing::error!("writing keys.jsonl.zst: {e}");
        }
    }

    /// Copies new spool lines of this session to `shell.jsonl`, redacted; nothing while
    /// privacy is on.
    pub(super) fn shell_poll(&mut self) {
        let Some(spool) = &mut self.capture.spool else {
            return;
        };
        let lines = match spool.read_new() {
            Ok(lines) => lines,
            Err(e) => {
                tracing::warn!("reading the shell spool: {e}");
                return;
            }
        };
        for line in lines {
            let secrets = &self.config.capture.secret_patterns;
            let cmd = shell::redact(&line.cmd, secrets).into_owned();
            // Several commands can share a millisecond, so equal times are compared by text.
            let (last_t, last_cmd) = &self.capture.shell_last;
            if line.t < *last_t || (line.t == *last_t && cmd == *last_cmd) {
                continue;
            }
            self.capture.shell_last = (line.t, cmd.clone());
            if self.state.privacy {
                continue;
            }
            let cwd = shell::redact(&line.cwd, secrets).into_owned();
            if cmd != line.cmd || cwd != line.cwd {
                self.emit(
                    "capture",
                    EventKind::CaptureSecretDropped {
                        source: CaptureSource::Shell,
                    },
                );
            }
            let line = ShellLine { cmd, cwd, ..line };
            let written = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.capture.shell_file)
                .and_then(|mut f| {
                    let mut text = serde_json::to_string(&line)?;
                    text.push('\n');
                    f.write_all(text.as_bytes())
                });
            if let Err(e) = written {
                tracing::error!("writing shell.jsonl: {e}");
            }
            // Shown in the TUI log, but not stored in events.jsonl: shell.jsonl is the record.
            let event = Event::new(
                &self.clock,
                "shell",
                EventKind::ShellCmd {
                    cmd: line.cmd,
                    cwd: line.cwd,
                },
            );
            let _ = self.events.send(std::sync::Arc::new(event));
        }
    }

    /// Stored keys per minute for the last [`SPARK_MINUTES`] minutes, oldest first.
    pub(super) fn keys_per_min(&self) -> Vec<u32> {
        let now = self.clock.now_ts().unix_ms().div_euclid(60_000);
        (0..SPARK_MINUTES as i64)
            .rev()
            .map(|ago| {
                let minute = now - ago;
                self.capture
                    .minutes
                    .iter()
                    .find(|(m, _)| *m == minute)
                    .map_or(0, |(_, n)| *n)
            })
            .collect()
    }
}
