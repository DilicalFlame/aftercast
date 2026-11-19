//! Headless sessions (`--no-tui`): commands from a script file or stdin, events printed
//! to stdout. Lines look like `+5s checkpoint "Parser"`; the `+` delay advances the
//! session clock without waiting (specs/14-roadmap.md, "Mock mode").

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use ac_core::chat::{ChatKind, ChatMessage};
use ac_core::command::{Command, QuestionAction};
use ac_core::event::{By, Level};
use ac_core::session_state::QuestionState;
use ac_core::time::Clock;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, oneshot};

use ac_capture::keys::RawKey;

use crate::error::{AppError, Result};
use crate::live::{Handles, Msg};

/// One parsed script line.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub delay: Duration,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Checkpoint(String),
    Mark(Option<String>),
    Note(String),
    Quest(String),
    Render,
    Sync,
    Finalize,
    Quit,
    Brb(Option<bool>),
    Privacy(Option<bool>),
    Scene(String),
    /// Something OBS reports (`--mock` only), e.g. `obs record started D:/a.mkv`.
    Obs(ac_obs::Update),
    /// A chat message: `chat <author> [owner|mod|member] <text>`.
    Chat {
        author: String,
        level: Level,
        text: String,
    },
    /// `question show|answer|dismiss [n]`: the n-th question in the queue (default 1).
    Question(QuestionAction, usize),
    Say(String),
    Overlay(String, bool),
    Latency,
    Lat(u64),
    /// `title yes|no` answers a pending `!title`.
    Title(bool),
    /// `key <app> <combo>`, e.g. `key Code.exe Ctrl+S`: one key press in that app.
    Key {
        app: String,
        combo: String,
    },
    /// `type <app> <text>`: one key press per character.
    Type {
        app: String,
        text: String,
        /// `type-password`: typed into a password box.
        password: bool,
    },
    /// `shell`: read the shell spool now.
    Shell,
    /// Only advance the clock.
    Wait,
}

/// Parses `[+<n>ms|s|m] <command> [args]`. Blank lines and `#` comments yield `None`.
pub fn parse(line: &str) -> Result<Option<Line>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let (delay, rest) = match line.strip_prefix('+') {
        Some(rest) => {
            let (spec, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            (parse_delay(spec)?, rest.trim_start())
        }
        None => (Duration::ZERO, line),
    };
    let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let args = unquote(args.trim());
    let required = |what: &str| {
        if args.is_empty() {
            Err(format!("{name}: missing {what}"))
        } else {
            Ok(args.clone())
        }
    };
    let action = match name {
        "checkpoint" | "cp" => Action::Checkpoint(required("title")?),
        "mark" => Action::Mark((!args.is_empty()).then(|| args.clone())),
        "note" => Action::Note(required("text")?),
        "quest" => Action::Quest(required("title")?),
        "render" => Action::Render,
        "sync" => Action::Sync,
        "finalize" => Action::Finalize,
        "quit" => Action::Quit,
        "brb" => Action::Brb(Some(true)),
        "back" => Action::Brb(Some(false)),
        "privacy" => Action::Privacy(match args.as_str() {
            "on" => Some(true),
            "off" => Some(false),
            _ => None,
        }),
        "scene" => Action::Scene(required("name")?),
        "obs" => Action::Obs(obs_update(&args)?),
        "chat" => chat_line(&args)?,
        "question" => {
            let (what, n) = args.split_once(' ').unwrap_or((args.as_str(), "1"));
            let action = match what {
                "show" => QuestionAction::Show,
                "answer" => QuestionAction::Answer,
                "dismiss" => QuestionAction::Dismiss,
                _ => return Err("question show|answer|dismiss [n]".into()),
            };
            let n = n
                .trim()
                .parse()
                .map_err(|_| format!("bad question number {n:?}"))?;
            Action::Question(action, n)
        }
        "say" => Action::Say(required("text")?),
        "overlay" => match args.split_once(' ') {
            Some((name, "on")) => Action::Overlay(name.into(), true),
            Some((name, "off")) => Action::Overlay(name.into(), false),
            _ => return Err("overlay <name> on|off".into()),
        },
        "latency" => Action::Latency,
        "lat" => Action::Lat(
            args.parse()
                .map_err(|_| format!("lat: bad number {args:?}"))?,
        ),
        "title" => match args.as_str() {
            "yes" => Action::Title(true),
            "no" => Action::Title(false),
            _ => return Err("title yes|no".into()),
        },
        "key" | "type" | "type-password" => {
            let (app, rest) = args
                .split_once(' ')
                .ok_or_else(|| format!("{name} <app> <keys>"))?;
            let (app, rest) = (app.to_owned(), rest.to_owned());
            if name == "key" {
                Action::Key { app, combo: rest }
            } else {
                Action::Type {
                    app,
                    text: rest,
                    password: name == "type-password",
                }
            }
        }
        "shell" => Action::Shell,
        "wait" | "" => Action::Wait,
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(Some(Line { delay, action }))
}

/// `scene <name>`, `stream started|stopped|reconnecting`,
/// `record started|stopped|paused|resumed|file_changed [path]`, `down`.
fn obs_update(args: &str) -> Result<ac_obs::Update, String> {
    use ac_core::event::OutputState;
    use ac_obs::Update;
    let (what, rest) = args.split_once(' ').unwrap_or((args, ""));
    let rest = rest.trim();
    let state = || {
        let (state, path) = rest.split_once(' ').unwrap_or((rest, ""));
        let state: OutputState = serde_json::from_value(serde_json::Value::String(state.into()))
            .map_err(|_| format!("obs {what}: unknown state {state:?}"))?;
        Ok::<_, String>((
            state,
            Some(path.trim().to_owned()).filter(|p| !p.is_empty()),
        ))
    };
    Ok(match what {
        "scene" if !rest.is_empty() => Update::Scene(rest.to_owned()),
        "stream" => Update::Stream(state()?.0),
        "record" => {
            let (state, path) = state()?;
            Update::Record { state, path }
        }
        "down" => Update::Down {
            error: "connection to OBS lost".into(),
            retry_in: Duration::from_secs(1),
        },
        _ => return Err(format!("bad obs line {args:?}")),
    })
}

/// `<author> [owner|mod|member] <text>`.
fn chat_line(args: &str) -> Result<Action, String> {
    let (author, rest) = args
        .split_once(' ')
        .ok_or_else(|| "chat <author> [owner|mod|member] <text>".to_owned())?;
    let (level, text) = match rest.split_once(' ') {
        Some(("owner", text)) => (Level::Owner, text),
        Some(("mod", text)) => (Level::Mod, text),
        Some(("member", text)) => (Level::Member, text),
        _ => (Level::Viewer, rest),
    };
    Ok(Action::Chat {
        author: author.into(),
        level,
        text: unquote(text.trim()),
    })
}

/// A synthetic press typing `c`, as the hook reports it.
fn char_key(app: &str, c: char, t: ac_core::time::Timestamp) -> RawKey {
    let key = match c {
        'a'..='z' | 'A'..='Z' => format!("Key{}", c.to_ascii_uppercase()),
        '0'..='9' => format!("Num{c}"),
        ' ' => "Space".into(),
        _ => "Unknown".into(),
    };
    RawKey {
        t,
        key,
        ch: Some(c),
        mods: if c.is_ascii_uppercase() {
            vec!["Shift"]
        } else {
            vec![]
        },
        app: app.into(),
        title: String::new(),
        password: false,
    }
}

/// A synthetic press like `Ctrl+Shift+P` or `F5`.
fn combo_key(app: &str, combo: &str, t: ac_core::time::Timestamp) -> RawKey {
    let parts: Vec<&str> = combo.split('+').collect();
    let (key, held) = parts.split_last().unwrap_or((&"", &[]));
    let mods = ac_capture::keys::MODS
        .iter()
        .copied()
        .filter(|m| held.iter().any(|h| h.eq_ignore_ascii_case(m)))
        .collect();
    let key = match key.chars().collect::<Vec<_>>().as_slice() {
        [c] if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
        [c] if c.is_ascii_digit() => format!("Num{c}"),
        _ => key.to_string(),
    };
    RawKey {
        t,
        key,
        ch: None,
        mods,
        app: app.into(),
        title: String::new(),
        password: false,
    }
}

fn parse_delay(spec: &str) -> Result<Duration, String> {
    let split = spec
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(spec.len());
    let (num, unit) = spec.split_at(split);
    let value: f64 = num.parse().map_err(|_| format!("bad delay {spec:?}"))?;
    let secs = match unit {
        "ms" => value / 1000.0,
        "s" | "" => value,
        "m" => value * 60.0,
        _ => return Err(format!("bad delay unit in {spec:?} (use ms, s or m)")),
    };
    Ok(Duration::from_secs_f64(secs))
}

fn unquote(s: &str) -> String {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
        .to_owned()
}

impl Action {
    /// The controller command; `None` for `wait`.
    fn command(self, clock: &Clock) -> Option<Command> {
        let pressed_at = clock.now_ts();
        Some(match self {
            Self::Checkpoint(title) => Command::Checkpoint {
                title,
                pressed_at,
                by: By::Palette,
            },
            Self::Mark(label) => Command::Mark {
                label,
                pressed_at,
                by: By::Palette,
            },
            Self::Note(text) => Command::Note { text, pressed_at },
            Self::Quest(title) => Command::Quest { title },
            Self::Render => Command::Render,
            Self::Sync => Command::Sync,
            Self::Finalize => Command::Finalize,
            Self::Quit => Command::Quit,
            Self::Brb(on) => Command::Brb {
                on,
                by: By::Palette,
            },
            Self::Privacy(on) => Command::Privacy {
                on,
                by: By::Palette,
            },
            Self::Scene(name) => Command::Scene { name },
            Self::Say(text) => Command::Say { text },
            Self::Overlay(name, on) => Command::Overlay { name, on },
            Self::Latency => Command::Latency,
            Self::Lat(counter) => Command::LatencySample {
                counter,
                at: pressed_at,
            },
            Self::Title(ok) => Command::ConfirmTitle { ok },
            Self::Wait
            | Self::Obs(_)
            | Self::Chat { .. }
            | Self::Question(..)
            | Self::Key { .. }
            | Self::Type { .. }
            | Self::Shell => return None,
        })
    }
}

/// Runs the session headless until the script (or stdin) ends, then quits.
pub async fn run(handles: Handles, script: Option<&Path>) -> Result<()> {
    let printer = tokio::spawn(print_events(handles.events));
    let result = execute(&handles.clock, &handles.msgs, &handles.view, script).await;
    // Ends the session if the script did not; ignored when it already stopped.
    let (ack, _) = oneshot::channel();
    let _ = handles.msgs.send(Msg::Command(Command::Quit, ack));
    drop(handles.msgs);
    drop(handles.commands);
    handles
        .controller
        .await
        .map_err(|e| AppError::Usage(format!("session task failed: {e}")))?;
    let _ = printer.await;
    result
}

async fn execute(
    clock: &Clock,
    msgs: &tokio::sync::mpsc::UnboundedSender<Msg>,
    view: &tokio::sync::watch::Receiver<ac_tui::View>,
    script: Option<&Path>,
) -> Result<()> {
    // Lines run as they arrive, so stdin works interactively.
    let input: Box<dyn tokio::io::AsyncRead + Unpin + Send> = match script {
        Some(path) => Box::new(tokio::fs::File::open(path).await?),
        None => Box::new(tokio::io::stdin()),
    };
    let mut lines = BufReader::new(input).lines();
    let mut n = 0;
    while let Some(raw) = lines.next_line().await? {
        n += 1;
        let raw = raw.as_str();
        let line = parse(raw).map_err(|e| AppError::Usage(format!("script line {n}: {e}")))?;
        let Some(Line { delay, action }) = line else {
            continue;
        };
        if !delay.is_zero() {
            // Messages sent for earlier lines (OBS updates, chat) are timestamped when the
            // session handles them: let it catch up before the clock moves on.
            let (ack, done) = oneshot::channel();
            if msgs.send(Msg::Barrier(ack)).is_ok() {
                let _ = done.await;
            }
        }
        clock.advance(delay);
        let ends = matches!(action, Action::Quit | Action::Finalize);
        if let Action::Obs(update) = &action {
            // Same queue as commands, so it is handled before the next line.
            let _ = msgs.send(Msg::Obs(update.clone()));
        }
        let mut cmd = action.clone().command(clock);
        match action {
            Action::Key { app, combo } => {
                let _ = msgs.send(Msg::Key(combo_key(&app, &combo, clock.now_ts())));
            }
            Action::Type {
                app,
                text,
                password,
            } => {
                for c in text.chars() {
                    let key = RawKey {
                        password,
                        ..char_key(&app, c, clock.now_ts())
                    };
                    let _ = msgs.send(Msg::Key(key));
                    clock.advance(Duration::from_millis(100));
                }
            }
            Action::Shell => {
                let _ = msgs.send(Msg::ShellPoll);
            }
            Action::Chat {
                author,
                level,
                text,
            } => {
                let now = clock.now_ts();
                let msg = ChatMessage {
                    id: format!("script-{n}"),
                    t_received: now,
                    t_published: Some(now),
                    author_channel_id: format!("UC-{author}"),
                    author_name: author,
                    is_owner: level == Level::Owner,
                    is_moderator: level == Level::Mod,
                    is_member: level == Level::Member,
                    kind: ChatKind::Text,
                    text,
                    amount: None,
                };
                let _ = msgs.send(Msg::Chat(ac_youtube::chat::ChatUpdate::Message(msg)));
            }
            Action::Question(action, nth) => {
                // Wait until earlier lines (a chat message) are handled and published.
                let (ack, done) = oneshot::channel();
                if msgs.send(Msg::Barrier(ack)).is_ok() {
                    let _ = done.await;
                }
                let qid = view
                    .borrow()
                    .state
                    .questions
                    .iter()
                    .filter(|q| matches!(q.state, QuestionState::Open | QuestionState::Shown))
                    .nth(nth.saturating_sub(1))
                    .map(|q| q.qid.clone());
                match qid {
                    Some(qid) => cmd = Some(Command::Question { qid, action }),
                    None => eprintln!("script line {n}: no question {nth} in the queue"),
                }
            }
            _ => {}
        }
        if let Some(cmd) = cmd {
            let (ack, done) = oneshot::channel();
            if msgs.send(Msg::Command(cmd, ack)).is_err() {
                break;
            }
            let _ = done.await;
        }
        if ends {
            break;
        }
    }
    Ok(())
}

/// Prints `time type {data}` per event until the session ends.
async fn print_events(mut events: broadcast::Receiver<std::sync::Arc<ac_core::event::Event>>) {
    loop {
        match events.recv().await {
            Ok(event) => {
                let json = serde_json::to_value(&*event).unwrap_or_default();
                let kind = json["type"].as_str().unwrap_or("?");
                // A closed stdout (reader gone) ends printing instead of panicking.
                if writeln!(std::io::stdout(), "{} {kind} {}", event.t, json["data"]).is_err() {
                    return;
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => eprintln!("({n} events not shown)"),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Line {
        parse(s).unwrap().unwrap()
    }

    #[test]
    fn parses_delays_commands_and_quotes() {
        assert_eq!(
            line("+5s checkpoint \"Parser with nom\""),
            Line {
                delay: Duration::from_secs(5),
                action: Action::Checkpoint("Parser with nom".into())
            }
        );
        assert_eq!(line("+250ms mark").delay, Duration::from_millis(250));
        assert_eq!(line("+1.5m cp Setup").delay, Duration::from_secs(90));
        assert_eq!(
            line("mark good bit").action,
            Action::Mark(Some("good bit".into()))
        );
        assert_eq!(line("+10s").action, Action::Wait);
        assert_eq!(line("sync").action, Action::Sync);
        assert_eq!(line("privacy").action, Action::Privacy(None));
        assert_eq!(
            line("obs record file_changed D:/OBS/b.mkv").action,
            Action::Obs(ac_obs::Update::Record {
                state: ac_core::event::OutputState::FileChanged,
                path: Some("D:/OBS/b.mkv".into())
            })
        );
        assert_eq!(
            line("chat ana mod !chapter \"Parser\"").action,
            Action::Chat {
                author: "ana".into(),
                level: Level::Mod,
                text: "!chapter \"Parser\"".into()
            }
        );
        assert_eq!(
            line("chat bob !q why rust?").action,
            Action::Chat {
                author: "bob".into(),
                level: Level::Viewer,
                text: "!q why rust?".into()
            }
        );
        assert_eq!(
            line("question dismiss 2").action,
            Action::Question(QuestionAction::Dismiss, 2)
        );
        assert_eq!(line("lat 2400").action, Action::Lat(2400));
        assert_eq!(
            line("overlay toast off").action,
            Action::Overlay("toast".into(), false)
        );
        assert_eq!(parse("  # comment").unwrap(), None);
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(parse("checkpoint").is_err());
        assert!(parse("+5x sync").is_err());
        assert!(parse("explode").is_err());
        assert!(parse("obs record exploded").is_err());
        assert!(parse("obs scene").is_err());
    }
}
