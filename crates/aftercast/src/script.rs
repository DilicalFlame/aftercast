//! Headless sessions (`--no-tui`): commands from a script file or stdin, events printed
//! to stdout. Lines look like `+5s checkpoint "Parser"`; the `+` delay advances the
//! session clock without waiting (specs/14-roadmap.md, "Mock mode").

use std::path::Path;
use std::time::Duration;

use ac_core::command::Command;
use ac_core::event::By;
use ac_core::time::Clock;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{broadcast, oneshot};

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
        "wait" | "" => Action::Wait,
        other => return Err(format!("unknown command {other:?}")),
    };
    Ok(Some(Line { delay, action }))
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
            Self::Wait => return None,
        })
    }
}

/// Runs the session headless until the script (or stdin) ends, then quits.
pub async fn run(handles: Handles, script: Option<&Path>) -> Result<()> {
    let printer = tokio::spawn(print_events(handles.events));
    let result = execute(&handles.clock, &handles.msgs, script).await;
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
        clock.advance(delay);
        let ends = matches!(action, Action::Quit | Action::Finalize);
        if let Some(cmd) = action.command(clock) {
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
                println!(
                    "{} {} {}",
                    event.t,
                    json["type"].as_str().unwrap_or("?"),
                    json["data"]
                );
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
        assert_eq!(parse("  # comment").unwrap(), None);
        assert_eq!(parse("").unwrap(), None);
    }

    #[test]
    fn rejects_bad_lines() {
        assert!(parse("checkpoint").is_err());
        assert!(parse("+5x sync").is_err());
        assert!(parse("explode").is_err());
    }
}
