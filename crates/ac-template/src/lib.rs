//! Session README rendering: minijinja template, markdown to plain text and chapters
//! (specs/08-readme-and-description.md).

pub mod chapters;
pub mod plain;

use std::fs;
use std::path::{Path, PathBuf};

use ac_core::config::{Config, Readme};
use ac_core::session::SessionMeta;
use ac_core::session_state::{QuestionState, SessionState};
use ac_core::time::{Timestamp, chapter_ts, vod_t};
use chrono::Local;
use minijinja::{Environment, UndefinedBehavior, path_loader};
use serde_json::{Value, json};

use chapters::Chapter;

/// Name under which the session README is rendered (shown in error locations).
pub const README_NAME: &str = "README.md";

/// A rendered description.
#[derive(Debug, Clone, PartialEq)]
pub struct Description {
    /// Rendered markdown (`.render/README.rendered.md`).
    pub markdown: String,
    /// Plain-text YouTube description (`.render/description.txt`).
    pub text: String,
}

impl Description {
    pub fn chars(&self) -> usize {
        self.text.chars().count()
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RenderError {
    #[error("{name}:{line}:{col}: {message}")]
    Template {
        name: String,
        line: usize,
        col: usize,
        message: String,
    },
    #[error("description is {chars} characters, {} over the {max} limit", chars - max)]
    TooLong { chars: usize, max: usize },
}

/// Template environment rooted at the templates folder (for `{% include %}`).
pub struct Renderer {
    env: Environment<'static>,
    templates: PathBuf,
}

impl Renderer {
    pub fn new(templates: &Path) -> Self {
        let mut env = Environment::new();
        env.set_loader(path_loader(templates));
        env.set_undefined_behavior(UndefinedBehavior::Strict);
        env.add_filter("ts", |secs: f64| chapter_ts(secs, secs >= 3600.0));
        env.add_filter("truncate", truncate);
        env.add_filter("slug", slug);
        Self {
            env,
            templates: templates.to_path_buf(),
        }
    }

    /// Renders README source with `ctx` into markdown and a validated plain description.
    pub fn render(
        &self,
        readme: &str,
        ctx: &Value,
        opts: &Readme,
    ) -> Result<Description, RenderError> {
        let markdown = self
            .env
            .render_named_str(README_NAME, readme, minijinja::value::Serde(ctx))
            .map_err(|e| self.template_error(readme, &e))?;
        let text = plain::to_plain(&markdown, opts.link_style);
        let chars = text.chars().count();
        if chars > opts.max_chars {
            return Err(RenderError::TooLong {
                chars,
                max: opts.max_chars,
            });
        }
        Ok(Description { markdown, text })
    }

    /// Renders a short template (a bot reply) with `ctx`; no description rules apply.
    pub fn render_str(&self, name: &str, src: &str, ctx: &Value) -> Result<String, RenderError> {
        self.env
            .render_named_str(name, src, minijinja::value::Serde(ctx))
            .map_err(|e| RenderError::Template {
                name: name.into(),
                line: e.line().unwrap_or(1),
                col: 1,
                message: e.to_string(),
            })
    }

    fn template_error(&self, readme: &str, mut e: &minijinja::Error) -> RenderError {
        // Errors inside an include are wrapped; report the innermost location.
        while let Some(inner) = std::error::Error::source(e).and_then(|s| s.downcast_ref()) {
            e = inner;
        }
        let name = e.name().unwrap_or(README_NAME).to_owned();
        let source = if name == README_NAME {
            readme.to_owned()
        } else {
            fs::read_to_string(self.templates.join(&name)).unwrap_or_default()
        };
        let line = e.line().unwrap_or(1);
        let col = e
            .range()
            .and_then(|r| source.get(..r.start))
            .map_or(1, |before| {
                before.rsplit('\n').next().unwrap_or("").chars().count() + 1
            });
        let message = match e.detail() {
            Some(detail) => format!("{}: {detail}", e.kind()),
            None => e.kind().to_string(),
        };
        RenderError::Template {
            name,
            line,
            col,
            message,
        }
    }
}

/// `truncate(length=255, end="...")`, counted in characters.
fn truncate(s: &str, length: Option<usize>, end: Option<&str>) -> String {
    let length = length.unwrap_or(255);
    if s.chars().count() <= length {
        return s.to_owned();
    }
    let end = end.unwrap_or("...");
    let keep = length.saturating_sub(end.chars().count());
    s.chars().take(keep).collect::<String>() + end
}

/// Lowercase ASCII words joined by `-`.
pub fn slug(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

/// Everything the template context is built from.
pub struct ContextInput<'a> {
    pub config: &'a Config,
    pub meta: &'a SessionMeta,
    pub state: &'a SessionState,
    pub now: Timestamp,
}

/// Builds the chapters and the template context (specs/08, "Template context").
pub fn context(input: &ContextInput<'_>) -> (Value, Vec<Chapter>) {
    let ContextInput {
        config,
        meta,
        state,
        now,
    } = *input;
    let anchors = &meta.anchors;
    let to_vod = |t: Timestamp| {
        anchors
            .yt_actual_start
            .map(|start| vod_t(t, start, anchors.vod_offset_ms))
    };
    let vod_len = to_vod(anchors.yt_actual_end.unwrap_or(now)).unwrap_or(0.0);
    let long = vod_len >= 3600.0;
    let ts = |t: Timestamp| to_vod(t).map(|v| chapter_ts(v, long)).unwrap_or_default();

    let entries = chapters::candidates(state, &config.stream)
        .into_iter()
        .filter_map(|(t, title, kind)| Some((to_vod(t)?, title, kind)))
        .collect();
    let chapters = chapters::build(entries, vod_len, &config.stream.first_chapter_title);

    let local = |t: Timestamp| t.datetime().with_timezone(&Local);
    let started = anchors
        .yt_actual_start
        .or(anchors.session_start_wall)
        .unwrap_or(meta.created_at);
    let env: serde_json::Map<String, Value> = config
        .readme
        .expose_env
        .iter()
        .filter_map(|name| Some((name.clone(), Value::from(std::env::var(name).ok()?))))
        .collect();

    let mut ctx = json!({
        "channel": { "series": config.channel.series, "short": config.channel.short },
        "stream": {
            "title": meta.title_at_start.clone().unwrap_or_default(),
            "url": format!("https://www.youtube.com/watch?v={}", meta.video_id),
            "video_id": meta.video_id,
            "episode": meta.episode,
            "episode_padded": format!("{:0pad$}", meta.episode, pad = config.channel.episode_pad),
            "date": local(meta.created_at).format("%Y-%m-%d").to_string(),
            "started_at": local(started).format("%H:%M").to_string(),
            "status": serde_json::to_value(meta.status).unwrap_or_default(),
        },
        "vars": config.vars,
        "env": env,
        "quests": state.quests.iter().map(|q| json!({
            "title": q.title, "vod_t": to_vod(q.t), "ts": ts(q.t),
        })).collect::<Vec<_>>(),
        "chapters": chapters,
        "chapters_block": chapters::block(&chapters),
        "questions_answered": state.questions.iter()
            .filter(|q| q.state == QuestionState::Answered)
            .map(|q| json!({
                "author": q.author, "text": q.text, "ts": q.shown_at.map(ts).unwrap_or_default(),
            }))
            .collect::<Vec<_>>(),
        "now": { "local": local(now).format("%Y-%m-%d %H:%M").to_string(), "utc": now.to_string() },
    });
    // `quest` stays undefined until one is set, so templates can test `quest is defined`.
    if let Some(q) = state.quests.last() {
        ctx["quest"] = json!({ "title": q.title, "id": q.id, "source": q.source });
    }
    (ctx, chapters)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::event::{By, Event, EventKind, QuestSource};
    use ac_core::session::{Anchors, Status};
    use ac_core::time::Clock;

    fn renderer() -> (tempfile::TempDir, Renderer) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("partials")).unwrap();
        fs::write(dir.path().join("partials/bad.md"), "ok\n{{ nope }}").unwrap();
        let r = Renderer::new(dir.path());
        (dir, r)
    }

    #[test]
    fn filters() {
        let (_d, r) = renderer();
        let out = r
            .render(
                "{{ 3915 | ts }} {{ 45 | ts }} {{ 'Hello, World!' | slug }} {{ 'abcdef' | truncate(4) }} {{ 'ab' | truncate(4) }} {{ x | default('d') }}",
                &json!({}),
                &Readme::default(),
            )
            .unwrap();
        assert_eq!(out.text, "1:05:15 0:45 hello-world a... ab d");
    }

    #[test]
    fn undefined_is_an_error_with_location() {
        let (_d, r) = renderer();
        let err = r
            .render("line one\nhi {{ missing }}", &json!({}), &Readme::default())
            .unwrap_err();
        let RenderError::Template {
            name, line, col, ..
        } = &err
        else {
            panic!("{err}")
        };
        assert_eq!((name.as_str(), *line, *col), (README_NAME, 2, 7), "{err}");

        let err = r
            .render(
                "{% include 'partials/bad.md' %}",
                &json!({}),
                &Readme::default(),
            )
            .unwrap_err();
        assert!(
            err.to_string().starts_with("partials/bad.md:2:4: "),
            "{err}"
        );

        let err = r
            .render("{% if %}", &json!({}), &Readme::default())
            .unwrap_err();
        assert!(err.to_string().starts_with("README.md:1:"), "{err}");
    }

    #[test]
    fn over_length_reports_exact_overflow() {
        let (_d, r) = renderer();
        let opts = Readme {
            max_chars: 10,
            ..Readme::default()
        };
        let err = r.render("12345678901234", &json!({}), &opts).unwrap_err();
        assert_eq!(err, RenderError::TooLong { chars: 14, max: 10 });
        assert_eq!(
            err.to_string(),
            "description is 14 characters, 4 over the 10 limit"
        );
    }

    #[test]
    fn context_maps_state_to_vod_time() {
        let clock = Clock::starting_at("2026-11-19T10:00:00Z".parse().unwrap());
        let start: Timestamp = "2026-11-19T09:00:00Z".parse().unwrap();
        let config = Config::default();
        let meta = SessionMeta {
            schema_version: 1,
            id: "x".into(),
            episode: 42,
            platform: "youtube".into(),
            video_id: "abcdefghijk".into(),
            live_chat_id: None,
            title_at_start: Some("IDD #042".into()),
            created_at: start,
            status: Status::Live,
            anchors: Anchors {
                yt_actual_start: Some(start),
                ..Anchors::default()
            },
        };
        let at = |s: &str| -> Timestamp { s.parse().unwrap() };
        let events = [
            EventKind::QuestSet {
                id: Some("rust/1".into()),
                title: "TUI todo".into(),
                source: QuestSource::Manual,
                extra: None,
            },
            EventKind::Checkpoint {
                cid: "1".into(),
                title: "Setup".into(),
                pressed_at: at("2026-11-19T09:12:40Z"),
                by: By::Tui,
                latency_corrected_s: None,
            },
        ];
        let mut state = SessionState::default();
        for kind in events {
            state.apply(&Event::new(&clock, "tui", kind));
        }
        let (ctx, chapters) = context(&ContextInput {
            config: &config,
            meta: &meta,
            state: &state,
            now: clock.now_ts(),
        });
        // VOD is 1 h long, so long timestamps; the quest change at 1:00:00 is a chapter too.
        let lines: Vec<_> = chapters
            .iter()
            .map(|c| format!("{} {}", c.ts, c.title))
            .collect();
        assert_eq!(
            lines,
            ["0:00:00 Start", "0:12:40 Setup", "1:00:00 TUI todo"]
        );
        assert_eq!(
            ctx["chapters_block"],
            "0:00:00 Start\n0:12:40 Setup\n1:00:00 TUI todo"
        );
        assert_eq!(ctx["stream"]["episode_padded"], "042");
        assert_eq!(ctx["quest"]["title"], "TUI todo");
        assert_eq!(ctx["quests"][0]["ts"], "1:00:00");
        assert_eq!(ctx["stream"]["status"], "live");
    }
}
