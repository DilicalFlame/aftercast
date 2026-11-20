//! The `:` command palette's knowledge: every command with its usage and what it does, and
//! suggestions for what is being typed, from command names to their arguments (OBS
//! scenes, audio inputs and sources, overlays, setting keys and values).

use ac_core::config::{Config, Value};
use ac_core::obs::Media;

use crate::settings::{self, Kind};

/// What an argument completes from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Arg {
    None,
    /// Free text (a title, a note): nothing to suggest.
    Text,
    Scene,
    /// An audio input, then `on`/`off`.
    Mute,
    /// An audio input, then a number of dB.
    Volume,
    /// An audio input only.
    Audio,
    /// A source in the program scene.
    Source,
    /// An overlay name, then `on`/`off`.
    Overlay,
    OnOff,
    /// A setting key, then a value.
    Setting,
    /// A setting key only.
    SettingKey,
}

pub struct Spec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub usage: &'static str,
    pub about: &'static str,
    pub arg: Arg,
}

pub const OVERLAYS: [&str; 7] = [
    "now", "question", "toast", "brb", "latency", "chapters", "keys",
];

const fn spec(
    name: &'static str,
    aliases: &'static [&'static str],
    usage: &'static str,
    about: &'static str,
    arg: Arg,
) -> Spec {
    Spec {
        name,
        aliases,
        usage,
        about,
        arg,
    }
}

/// Every command, in the order suggestions list them.
pub const SPECS: &[Spec] = &[
    spec(
        "checkpoint",
        &["cp"],
        "<title>",
        "chapter at the moment Enter is pressed",
        Arg::Text,
    ),
    spec("mark", &[], "[label]", "mark a good moment", Arg::Text),
    spec(
        "note",
        &[],
        "<text>",
        "private note, never published",
        Arg::Text,
    ),
    spec(
        "scene",
        &[],
        "<scene>",
        "switch the OBS program scene",
        Arg::Scene,
    ),
    spec(
        "mute",
        &[],
        "<input> [on|off]",
        "mute an OBS audio input (again: unmute)",
        Arg::Mute,
    ),
    spec(
        "unmute",
        &[],
        "<input>",
        "unmute an OBS audio input",
        Arg::Audio,
    ),
    spec(
        "volume",
        &["vol"],
        "<input> <dB>",
        "set an audio input's volume",
        Arg::Volume,
    ),
    spec(
        "hide",
        &[],
        "<source>",
        "hide a source in the program scene",
        Arg::Source,
    ),
    spec(
        "show",
        &[],
        "<source>",
        "show a source in the program scene",
        Arg::Source,
    ),
    spec("brb", &["break", "b"], "", "BRB scene on", Arg::None),
    spec(
        "back",
        &[],
        "",
        "BRB off, back to the scene before",
        Arg::None,
    ),
    spec(
        "privacy",
        &[],
        "[on|off]",
        "privacy scene, key capture paused",
        Arg::OnOff,
    ),
    spec(
        "set",
        &[],
        "<setting> [value]",
        "change a setting (no value flips a switch)",
        Arg::Setting,
    ),
    spec(
        "reset",
        &[],
        "<setting>",
        "a setting back to its default",
        Arg::SettingKey,
    ),
    spec("settings", &[], "", "open the settings page", Arg::None),
    spec(
        "sync",
        &[],
        "",
        "push the description to YouTube now",
        Arg::None,
    ),
    spec("quest", &[], "<title>", "set the current quest", Arg::Text),
    spec(
        "say",
        &[],
        "<text>",
        "chat as the channel (20 quota units)",
        Arg::Text,
    ),
    spec(
        "overlay",
        &[],
        "<overlay> on|off",
        "turn an overlay on or off",
        Arg::Overlay,
    ),
    spec(
        "latency",
        &[],
        "",
        "start or stop measuring the stream delay",
        Arg::None,
    ),
    spec(
        "lat",
        &[],
        "<n>",
        "the counter number seen on stream",
        Arg::Text,
    ),
    spec("preflight", &[], "", "the pre-flight checklist", Arg::None),
    spec("reload", &[], "", "re-read config and templates", Arg::None),
    spec(
        "finalize",
        &[],
        "",
        "final description push, end the session",
        Arg::None,
    ),
    spec("help", &["?"], "", "keys and commands", Arg::None),
    spec(
        "quit",
        &["q"],
        "",
        "quit (the session can be resumed)",
        Arg::None,
    ),
];

/// The command typed as `name` (or one of its aliases).
pub fn find(name: &str) -> Option<&'static Spec> {
    SPECS
        .iter()
        .find(|s| s.name == name || s.aliases.contains(&name))
}

/// One suggestion: picking it replaces the whole line with `line`.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub line: String,
    pub label: String,
    pub detail: String,
}

/// What the palette knows about the outside world when suggesting.
pub struct World<'a> {
    pub obs: &'a Media,
    pub scene: Option<&'a str>,
    pub config: &'a Config,
}

/// The command being typed (if its name is complete) and suggestions for the line.
pub fn suggest(line: &str, world: &World) -> (Option<&'static Spec>, Vec<Suggestion>) {
    let Some((name, args)) = line.split_once(' ') else {
        return (find(line), command_names(line));
    };
    let Some(spec) = find(name) else {
        return (None, Vec::new());
    };
    let head = format!("{name} ");
    let out = match spec.arg {
        Arg::None | Arg::Text => Vec::new(),
        Arg::OnOff => words(&head, args, &["on", "off"]),
        Arg::Scene => pick(
            &head,
            args,
            world.obs.scenes.iter().map(|s| {
                let now = world.scene == Some(s.as_str());
                (
                    s.clone(),
                    if now {
                        "● on air now".into()
                    } else {
                        String::new()
                    },
                )
            }),
        ),
        Arg::Audio | Arg::Mute | Arg::Volume => {
            let audio = || {
                world.obs.audio.iter().map(|a| {
                    let state = if a.muted { "muted" } else { "live" };
                    (a.name.clone(), format!("{state} · {:.1} dB", a.volume_db))
                })
            };
            match (
                spec.arg,
                then(args, world.obs.audio.iter().map(|a| a.name.as_str())),
            ) {
                (Arg::Mute, Some((input, rest))) => {
                    words(&format!("{head}{input} "), rest, &["on", "off"])
                }
                (Arg::Volume, Some((input, rest))) => words(
                    &format!("{head}{input} "),
                    rest,
                    &["-12", "-6", "-3", "0", "3"],
                ),
                _ => pick(&head, args, audio()),
            }
        }
        Arg::Source => {
            let scene = world.scene.unwrap_or_default();
            pick(
                &head,
                args,
                world.obs.sources_in(scene).map(|i| {
                    let state = if i.enabled { "shown" } else { "hidden" };
                    (i.source.clone(), format!("{state} in {scene}"))
                }),
            )
        }
        Arg::Overlay => match then(args, OVERLAYS.iter().copied()) {
            Some((overlay, rest)) => words(&format!("{head}{overlay} "), rest, &["on", "off"]),
            None => pick(
                &head,
                args,
                OVERLAYS.iter().map(|o| ((*o).to_owned(), String::new())),
            ),
        },
        Arg::Setting | Arg::SettingKey => {
            let all = settings::all();
            let keys = all.iter().map(|s| s.key);
            match (spec.arg, then(args, keys)) {
                (Arg::Setting, Some((key, rest))) => {
                    let s = all
                        .iter()
                        .find(|s| s.key == key)
                        .expect("key came from the list");
                    match s.kind {
                        Kind::Bool => words(&format!("{head}{key} "), rest, &["on", "off"]),
                        Kind::Choice(options) => words(&format!("{head}{key} "), rest, options),
                        _ => Vec::new(),
                    }
                }
                _ => pick(
                    &head,
                    args,
                    all.iter().map(|s| {
                        let value = settings::show(&(s.get)(world.config));
                        (s.key.to_owned(), format!("{value} · {}", s.label))
                    }),
                ),
            }
        }
    };
    (Some(spec), out)
}

/// Command names matching what is typed: prefix matches first, then names containing it.
fn command_names(typed: &str) -> Vec<Suggestion> {
    let typed = typed.to_lowercase();
    let mut first = Vec::new();
    let mut rest = Vec::new();
    for s in SPECS {
        let names = std::iter::once(s.name).chain(s.aliases.iter().copied());
        let prefix = names.clone().any(|n| n.starts_with(&typed));
        if !prefix && !s.name.contains(&typed) {
            continue;
        }
        let suggestion = Suggestion {
            line: if s.arg == Arg::None {
                s.name.to_owned()
            } else {
                format!("{} ", s.name)
            },
            label: format!("{} {}", s.name, s.usage).trim_end().to_owned(),
            detail: s.about.to_owned(),
        };
        if prefix {
            first.push(suggestion)
        } else {
            rest.push(suggestion)
        }
    }
    first.extend(rest);
    first
}

/// `args` starts with one of `names` (case-insensitive) and a space: that name and the rest.
fn then<'a>(args: &'a str, names: impl Iterator<Item = &'a str>) -> Option<(&'a str, &'a str)> {
    // The longest match wins, so "Mic" does not shadow "Mic/Aux".
    names
        .filter(|n| {
            args.len() > n.len()
                && args.is_char_boundary(n.len())
                && args[..n.len()].eq_ignore_ascii_case(n)
                && args[n.len()..].starts_with(' ')
        })
        .max_by_key(|n| n.len())
        .map(|n| (n, &args[n.len() + 1..]))
}

/// Candidates containing what is typed (case-insensitive), those starting with it first.
fn pick(
    head: &str,
    typed: &str,
    candidates: impl Iterator<Item = (String, String)>,
) -> Vec<Suggestion> {
    let typed = typed.to_lowercase();
    let mut first = Vec::new();
    let mut rest = Vec::new();
    for (name, detail) in candidates {
        let lower = name.to_lowercase();
        let s = Suggestion {
            line: format!("{head}{name}"),
            label: name,
            detail,
        };
        if lower.starts_with(&typed) {
            first.push(s);
        } else if lower.contains(&typed) {
            rest.push(s);
        }
    }
    first.extend(rest);
    first
}

fn words(head: &str, typed: &str, options: &[&str]) -> Vec<Suggestion> {
    pick(
        head,
        typed,
        options.iter().map(|o| ((*o).to_owned(), String::new())),
    )
}

/// Splits a trailing `on` / `off` (or `toggle`) off `args`: the name and the switch.
pub fn trailing_switch(args: &str) -> (String, Option<bool>) {
    match args.rsplit_once(' ') {
        Some((name, "on")) => (name.trim().to_owned(), Some(true)),
        Some((name, "off")) => (name.trim().to_owned(), Some(false)),
        Some((name, "toggle")) => (name.trim().to_owned(), None),
        _ => (args.trim().to_owned(), None),
    }
}

/// A typed value for a setting: `on`/`off` (also yes/no, true/false), a number in range,
/// one of the choices, or text.
pub fn parse_value(s: &settings::Setting, text: &str) -> Result<Value, String> {
    let text = text.trim();
    match s.kind {
        Kind::Bool => match text.to_lowercase().as_str() {
            "on" | "yes" | "true" | "1" => Ok(Value::Bool(true)),
            "off" | "no" | "false" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("{} takes on or off", s.key)),
        },
        Kind::Int { min, max, .. } => text
            .parse::<i64>()
            .map(|n| Value::Int(n.clamp(min, max)))
            .map_err(|_| format!("{} takes a whole number ({min} to {max})", s.key)),
        Kind::Float { min, max, .. } => match text.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Value::Float(n.clamp(min, max))),
            _ => Err(format!("{} takes a number ({min} to {max})", s.key)),
        },
        Kind::Choice(options) => options
            .iter()
            .find(|o| o.eq_ignore_ascii_case(text))
            .map(|o| Value::Text((*o).to_owned()))
            .ok_or_else(|| format!("{} takes one of: {}", s.key, options.join(", "))),
        Kind::Text => Ok(Value::Text(text.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::obs::{Audio, Item};

    fn world_media() -> Media {
        Media {
            scenes: vec!["Code".into(), "BRB".into(), "Chatting".into()],
            audio: vec![
                Audio {
                    name: "Mic".into(),
                    kind: "wasapi_input_capture".into(),
                    muted: false,
                    volume_db: 0.0,
                },
                Audio {
                    name: "Mic/Aux".into(),
                    kind: "wasapi_input_capture".into(),
                    muted: true,
                    volume_db: -3.0,
                },
                Audio {
                    name: "Desktop Audio".into(),
                    kind: "wasapi_output_capture".into(),
                    muted: false,
                    volume_db: 0.0,
                },
            ],
            items: vec![Item {
                scene: "Code".into(),
                source: "Camera".into(),
                id: 1,
                enabled: true,
            }],
        }
    }

    fn lines(line: &str) -> Vec<String> {
        let obs = world_media();
        let config = Config::default();
        let world = World {
            obs: &obs,
            scene: Some("Code"),
            config: &config,
        };
        suggest(line, &world)
            .1
            .into_iter()
            .map(|s| s.line)
            .collect()
    }

    #[test]
    fn suggests_commands_then_their_arguments() {
        assert_eq!(lines("mu")[0], "mute ");
        assert!(lines("s").starts_with(&["scene ".into(), "show ".into(), "set ".into()]));
        // Contains-matches come after prefix matches.
        assert!(lines("ute").contains(&"mute ".to_owned()));
        assert_eq!(lines("scene ch"), ["scene Chatting"]);
        assert_eq!(lines("mute de"), ["mute Desktop Audio"]);
        // After a complete input name: on/off; the longest name wins.
        assert_eq!(
            lines("mute Mic/Aux o"),
            ["mute Mic/Aux on", "mute Mic/Aux off"]
        );
        assert_eq!(lines("hide c"), ["hide Camera"]);
        assert_eq!(
            lines("overlay toast "),
            ["overlay toast on", "overlay toast off"]
        );
        assert_eq!(lines("set tui.wr")[0], "set tui.wrap_lines");
        assert_eq!(
            lines("set bot.default_reply b"),
            ["set bot.default_reply both"]
        );
        assert!(lines("say hel").is_empty());
    }

    #[test]
    fn parses_switches_and_values() {
        assert_eq!(
            trailing_switch("Desktop Audio off"),
            ("Desktop Audio".into(), Some(false))
        );
        assert_eq!(trailing_switch("Mic/Aux"), ("Mic/Aux".into(), None));
        let all = settings::all();
        let wrap = all.iter().find(|s| s.key == "tui.wrap_lines").unwrap();
        assert_eq!(parse_value(wrap, "ON"), Ok(Value::Bool(true)));
        assert!(parse_value(wrap, "maybe").is_err());
        let reply = all.iter().find(|s| s.key == "bot.default_reply").unwrap();
        assert_eq!(parse_value(reply, "Chat"), Ok(Value::Text("chat".into())));
        assert!(
            parse_value(reply, "fax")
                .unwrap_err()
                .contains("overlay, chat, both")
        );
    }
}
