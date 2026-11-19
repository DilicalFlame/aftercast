//! Chat command parser, permissions, cooldowns and reply routing (specs/09-streambot.md).
//!
//! Pure logic with the time passed in: the session controller runs the commands, since
//! most of them need session state (quest, chapters, OBS).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use ac_core::chat::ChatMessage;
use ac_core::config::{self, ReplyMode};
use ac_core::event::Level;
use ac_core::time::Timestamp;

/// Built-in commands. Names and aliases are in [`Builtin::lookup`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Help,
    Q,
    Quest,
    Uptime,
    Chapters,
    Lurk,
    Brb,
    Back,
    Chapter,
    Mark,
    Lat,
    Title,
}

impl Builtin {
    fn lookup(name: &str) -> Option<Self> {
        Some(match name {
            "help" | "commands" => Self::Help,
            "q" => Self::Q,
            "quest" | "now" => Self::Quest,
            "uptime" => Self::Uptime,
            "chapters" => Self::Chapters,
            "lurk" => Self::Lurk,
            "brb" => Self::Brb,
            "back" => Self::Back,
            "chapter" => Self::Chapter,
            "mark" => Self::Mark,
            "lat" => Self::Lat,
            "title" => Self::Title,
            _ => return None,
        })
    }

    fn level(self) -> Level {
        match self {
            Self::Help | Self::Q | Self::Quest | Self::Uptime | Self::Chapters | Self::Lurk => {
                Level::Viewer
            }
            Self::Brb | Self::Back | Self::Chapter | Self::Mark => Level::Mod,
            Self::Lat | Self::Title => Level::Owner,
        }
    }
}

/// The viewer commands `!help` lists.
const VIEWER_HELP: &str = "!q <question> · !quest · !uptime · !chapters · !lurk";

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Builtin(Builtin),
    /// A `[bot.commands]` entry: its template text and reply mode.
    Custom {
        text: String,
        reply: ReplyMode,
    },
}

/// A command allowed to run now.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// Lowercase name as typed, without the prefix.
    pub name: String,
    pub cmd: Cmd,
    pub args: String,
    pub level: Level,
}

/// Where a reply actually goes after the chat cap is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub overlay: bool,
    pub chat: bool,
}

pub struct Bot {
    prefix: String,
    global_cooldown_s: f64,
    user_cooldown_s: f64,
    max_chat_per_hour: usize,
    owner_accounts: HashSet<String>,
    default_reply: ReplyMode,
    custom: BTreeMap<String, (String, Option<ReplyMode>)>,
    last_global: Option<Timestamp>,
    /// (author channel id, command name) -> last run.
    last_user: HashMap<(String, String), Timestamp>,
    /// Chat replies sent in the last hour.
    chat_sent: VecDeque<Timestamp>,
}

impl Bot {
    pub fn new(cfg: &config::Bot) -> Self {
        Self {
            prefix: cfg.prefix.clone(),
            global_cooldown_s: cfg.global_cooldown_s as f64,
            user_cooldown_s: cfg.user_cooldown_s as f64,
            max_chat_per_hour: cfg.max_chat_replies_per_hour as usize,
            owner_accounts: cfg.owner_accounts.iter().cloned().collect(),
            default_reply: cfg.default_reply,
            custom: cfg
                .commands
                .iter()
                .map(|(name, c)| (name.to_lowercase(), (c.text.clone(), c.reply)))
                .collect(),
            last_global: None,
            last_user: HashMap::new(),
            chat_sent: VecDeque::new(),
        }
    }

    pub fn default_reply(&self) -> ReplyMode {
        self.default_reply
    }

    pub fn help(&self) -> String {
        let custom: Vec<String> = self.custom.keys().map(|n| format!("!{n}")).collect();
        if custom.is_empty() {
            VIEWER_HELP.into()
        } else {
            format!("{VIEWER_HELP} · {}", custom.join(" · "))
        }
    }

    /// The command in `msg` if it may run now; `None` for plain chat, unknown commands,
    /// too low a level or a cooldown (all ignored silently). Owner and mods skip
    /// cooldowns. A command that runs starts its cooldowns.
    pub fn parse(&mut self, msg: &ChatMessage, now: Timestamp) -> Option<Parsed> {
        let rest = msg.text.trim().strip_prefix(self.prefix.as_str())?;
        let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let name = name.to_lowercase();
        let (cmd, needed) = match Builtin::lookup(&name) {
            Some(b) => (Cmd::Builtin(b), b.level()),
            None => {
                let (text, reply) = self.custom.get(&name)?;
                let reply = reply.unwrap_or(self.default_reply);
                let cmd = Cmd::Custom {
                    text: text.clone(),
                    reply,
                };
                (cmd, Level::Viewer)
            }
        };
        let level = if self.owner_accounts.contains(&msg.author_channel_id) {
            Level::Owner
        } else {
            msg.level()
        };
        if level < needed {
            return None;
        }
        let key = (msg.author_channel_id.clone(), name.clone());
        if level < Level::Mod {
            let cooling = |last: Option<&Timestamp>, secs: f64| {
                last.is_some_and(|t| now.secs_since(*t) < secs)
            };
            if cooling(self.last_global.as_ref(), self.global_cooldown_s)
                || cooling(self.last_user.get(&key), self.user_cooldown_s)
            {
                return None;
            }
        }
        self.last_global = Some(now);
        self.last_user.insert(key, now);
        Some(Parsed {
            name,
            cmd,
            args: args.trim().to_owned(),
            level,
        })
    }

    /// Applies the chat cap: a chat reply beyond `max_chat_replies_per_hour` goes to the
    /// overlay instead. Counts the chat replies it allows.
    pub fn route(&mut self, mode: ReplyMode, now: Timestamp) -> Route {
        while self
            .chat_sent
            .front()
            .is_some_and(|t| now.secs_since(*t) >= 3600.0)
        {
            self.chat_sent.pop_front();
        }
        let wants_chat = matches!(mode, ReplyMode::Chat | ReplyMode::Both);
        let chat = wants_chat && self.chat_sent.len() < self.max_chat_per_hour;
        if chat {
            self.chat_sent.push_back(now);
        }
        Route {
            chat,
            overlay: mode != ReplyMode::Chat || !chat,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::chat::ChatKind;
    use ac_core::config::BotCommand;

    fn t(secs: f64) -> Timestamp {
        Timestamp::from_unix_ms(1_800_000_000_000).add_secs(secs)
    }

    fn msg(author: &str, level: Level, text: &str) -> ChatMessage {
        ChatMessage {
            id: "m".into(),
            t_received: t(0.0),
            t_published: None,
            author_channel_id: format!("UC{author}"),
            author_name: author.into(),
            is_owner: level == Level::Owner,
            is_moderator: level == Level::Mod,
            is_member: level == Level::Member,
            kind: ChatKind::Text,
            text: text.into(),
            amount: None,
        }
    }

    fn bot() -> Bot {
        let mut cfg = config::Bot::default();
        cfg.commands.insert(
            "Links".into(),
            BotCommand {
                text: "github".into(),
                reply: Some(ReplyMode::Chat),
            },
        );
        cfg.max_chat_replies_per_hour = 2;
        Bot::new(&cfg)
    }

    #[test]
    fn parses_prefix_case_args_and_unknowns() {
        let mut b = bot();
        let p = b
            .parse(&msg("ana", Level::Viewer, "  !Q  why rust? "), t(0.0))
            .unwrap();
        assert_eq!(
            (p.name.as_str(), &p.cmd, p.args.as_str()),
            ("q", &Cmd::Builtin(Builtin::Q), "why rust?")
        );
        let p = b
            .parse(&msg("bob", Level::Viewer, "!LINKS"), t(10.0))
            .unwrap();
        assert_eq!(
            p.cmd,
            Cmd::Custom {
                text: "github".into(),
                reply: ReplyMode::Chat
            }
        );
        for text in ["hello", "!nope", "q why", "!"] {
            assert_eq!(b.parse(&msg("cy", Level::Viewer, text), t(100.0)), None);
        }
        assert!(b.help().ends_with("!links"));
    }

    #[test]
    fn permissions() {
        let mut b = bot();
        assert_eq!(
            b.parse(&msg("v", Level::Member, "!chapter X"), t(0.0)),
            None
        );
        assert!(
            b.parse(&msg("m", Level::Mod, "!chapter X"), t(0.0))
                .is_some()
        );
        assert_eq!(b.parse(&msg("m", Level::Mod, "!lat 42"), t(0.0)), None);
        assert!(
            b.parse(&msg("o", Level::Owner, "!lat 42"), t(0.0))
                .is_some()
        );
    }

    #[test]
    fn owner_accounts_get_owner_commands() {
        let mut b = Bot::new(&config::Bot {
            owner_accounts: vec!["UCphone".into()],
            ..Default::default()
        });
        let p = b.parse(&msg("phone", Level::Viewer, "!lat 42"), t(0.0));
        assert_eq!(p.map(|p| p.level), Some(Level::Owner));
        assert_eq!(
            b.parse(&msg("other", Level::Viewer, "!lat 42"), t(0.0)),
            None
        );
    }

    #[test]
    fn cooldowns_global_then_per_user_mods_bypass() {
        let mut b = bot(); // global 3 s, per user 20 s
        assert!(
            b.parse(&msg("a", Level::Viewer, "!uptime"), t(0.0))
                .is_some()
        );
        assert!(
            b.parse(&msg("b", Level::Viewer, "!uptime"), t(2.0))
                .is_none()
        );
        assert!(
            b.parse(&msg("b", Level::Viewer, "!uptime"), t(3.5))
                .is_some()
        );
        assert!(
            b.parse(&msg("a", Level::Viewer, "!uptime"), t(10.0))
                .is_none()
        );
        assert!(
            b.parse(&msg("a", Level::Viewer, "!lurk"), t(10.0))
                .is_some()
        );
        assert!(
            b.parse(&msg("a", Level::Viewer, "!uptime"), t(20.5))
                .is_some()
        );
        assert!(b.parse(&msg("m", Level::Mod, "!uptime"), t(20.6)).is_some());
        assert!(b.parse(&msg("m", Level::Mod, "!uptime"), t(20.7)).is_some());
    }

    #[test]
    fn chat_cap_downgrades_to_overlay() {
        let mut b = bot(); // cap 2 per hour
        let chat = Route {
            chat: true,
            overlay: false,
        };
        let overlay = Route {
            chat: false,
            overlay: true,
        };
        assert_eq!(b.route(ReplyMode::Chat, t(0.0)), chat);
        assert_eq!(b.route(ReplyMode::Overlay, t(1.0)), overlay);
        assert_eq!(b.route(ReplyMode::Chat, t(2.0)), chat);
        assert_eq!(b.route(ReplyMode::Chat, t(3.0)), overlay, "cap reached");
        assert_eq!(
            b.route(ReplyMode::Both, t(4.0)),
            overlay,
            "both keeps the overlay"
        );
        assert_eq!(b.route(ReplyMode::Chat, t(3600.5)), chat, "an hour later");
    }
}
