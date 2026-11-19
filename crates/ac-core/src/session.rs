//! Session folders and `session.toml` (specs/04-session-and-folders.md).

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Home;
use crate::config::Config;
use crate::error::{Error, Result, parse_toml};
use crate::event::{EventLog, read_events};
use crate::session_state::SessionState;
use crate::state::State;
use crate::time::{Recording, Timestamp};

/// Contents of `session.toml` (schemas/session.schema.json).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub schema_version: u32,
    pub id: String,
    pub episode: u32,
    pub platform: String,
    pub video_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_at_start: Option<String>,
    pub created_at: Timestamp,
    pub status: Status,
    pub anchors: Anchors,
}

/// Session lifecycle: created, live, ended, finalized, posted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Created,
    Live,
    Ended,
    Finalized,
    Posted,
}

/// Time anchors, all UTC (specs/05-time-sync.md). Unknown anchors are omitted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Anchors {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start_wall: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yt_actual_start: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yt_actual_end: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_stream_start: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_record_start: Option<Timestamp>,
    #[serde(default)]
    pub vod_offset_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recordings: Vec<Recording>,
}

/// What the platform reports about the broadcast when a session is created.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub video_id: String,
    pub live_chat_id: Option<String>,
    pub title: Option<String>,
}

/// An open session folder.
#[derive(Debug)]
pub struct Session {
    pub dir: PathBuf,
    pub meta: SessionMeta,
}

impl Session {
    /// Creates the folder for a new stream: allocates the next episode number, copies the
    /// seed README and writes `session.toml`.
    pub fn create(home: &Home, config: &Config, new: NewSession, now: Timestamp) -> Result<Self> {
        let state_path = home.state_file();
        let mut state = State::load(&state_path)?;
        let episode = state.next_episode;
        let local = now.datetime().with_timezone(&chrono::Local);
        let name = format!(
            "{}_{}{:0pad$}",
            local.format("%Y-%m-%d"),
            config.channel.episode_prefix,
            episode,
            pad = config.channel.episode_pad
        );
        let rel = PathBuf::from(local.format("%Y").to_string()).join(&name);
        let dir = sessions_dir(home, config).join(&rel);
        fs::create_dir_all(dir.join(".render")).map_err(Error::io(&dir))?;

        let seed = home
            .root()
            .join(&config.paths.templates)
            .join(&config.readme.seed_template);
        let readme = dir.join("README.md");
        if !readme.exists() {
            fs::copy(&seed, &readme).map_err(Error::io(&seed))?;
        }

        let session = Self {
            dir,
            meta: SessionMeta {
                schema_version: 1,
                id: ulid::Ulid::generate().to_string(),
                episode,
                platform: "youtube".into(),
                video_id: new.video_id,
                live_chat_id: new.live_chat_id,
                title_at_start: new.title,
                created_at: now,
                status: Status::Created,
                anchors: Anchors {
                    session_start_wall: Some(now),
                    ..Anchors::default()
                },
            },
        };
        session.save()?;

        state.next_episode = episode + 1;
        state.last_session = Some(config.paths.sessions.join(rel));
        state.save(&state_path)?;
        Ok(session)
    }

    /// Opens an existing session folder.
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("session.toml");
        let text = fs::read_to_string(&path).map_err(Error::io(&path))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            meta: parse_toml(&path, &text)?,
        })
    }

    /// Finds the session for `video_id` under the sessions folder.
    pub fn find(home: &Home, config: &Config, video_id: &str) -> Result<Option<Self>> {
        let root = sessions_dir(home, config);
        for year in read_dirs(&root)? {
            for dir in read_dirs(&year)? {
                if !dir.join("session.toml").is_file() {
                    continue;
                }
                match Self::open(&dir) {
                    Ok(s) if s.meta.video_id == video_id => return Ok(Some(s)),
                    Ok(_) => {}
                    Err(e) => tracing::warn!("skipping session: {e}"),
                }
            }
        }
        Ok(None)
    }

    /// Rewrites `session.toml` atomically.
    pub fn save(&self) -> Result<()> {
        let text = toml::to_string(&self.meta).expect("session serializes to TOML");
        crate::write_atomic(&self.dir.join("session.toml"), text)
    }

    pub fn name(&self) -> String {
        self.dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    pub fn readme(&self) -> PathBuf {
        self.dir.join("README.md")
    }

    pub fn events_file(&self) -> PathBuf {
        self.dir.join("events.jsonl")
    }

    pub fn render_dir(&self) -> PathBuf {
        self.dir.join(".render")
    }

    /// Replays `events.jsonl` into state and opens the log for appending (resume).
    pub fn replay(&self) -> Result<(SessionState, EventLog)> {
        let path = self.events_file();
        let state = SessionState::from_events(&read_events(&path)?);
        Ok((state, EventLog::open(&path)?))
    }
}

pub fn sessions_dir(home: &Home, config: &Config) -> PathBuf {
    home.root().join(&config.paths.sessions)
}

/// Subfolders of `dir`, sorted; a missing folder has none.
fn read_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(dir)(e)),
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    Ok(dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Home, Config) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        home.init().unwrap();
        (dir, home, Config::default())
    }

    fn new(id: &str) -> NewSession {
        NewSession {
            video_id: id.into(),
            live_chat_id: Some("chat".into()),
            title: Some("IDD #001".into()),
        }
    }

    #[test]
    fn create_allocates_episodes_and_find_locates_them() {
        let (_tmp, home, config) = setup();
        let now: Timestamp = "2026-11-19T09:30:02.114Z".parse().unwrap();
        let a = Session::create(&home, &config, new("aaaaaaaaaaa"), now).unwrap();
        let b = Session::create(&home, &config, new("bbbbbbbbbbb"), now).unwrap();
        assert_eq!((a.meta.episode, b.meta.episode), (1, 2));
        assert!(a.name().ends_with("_ep001"), "{}", a.name());
        assert!(a.readme().is_file());
        assert!(a.render_dir().is_dir());

        let state = State::load(&home.state_file()).unwrap();
        assert_eq!(state.next_episode, 3);
        assert!(state.last_session.unwrap().ends_with(b.name()));

        let found = Session::find(&home, &config, "bbbbbbbbbbb")
            .unwrap()
            .unwrap();
        assert_eq!(found.meta, b.meta);
        assert!(
            Session::find(&home, &config, "ccccccccccc")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn session_toml_validates_against_schema() {
        let (_tmp, home, config) = setup();
        let mut s = Session::create(
            &home,
            &config,
            new("abcdefghijk"),
            "2026-11-19T09:30:02.114Z".parse().unwrap(),
        )
        .unwrap();
        s.meta.anchors.yt_actual_start = Some("2026-11-19T09:28:55Z".parse().unwrap());
        s.meta.anchors.latency_s = Some(4.2);
        s.save().unwrap();

        let reopened = Session::open(&s.dir).unwrap();
        assert_eq!(reopened.meta, s.meta);

        let text = fs::read_to_string(s.dir.join("session.toml")).unwrap();
        let json = serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap();
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/session.schema.json"
        );
        let schema = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let v = jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .unwrap();
        let errors: Vec<String> = v.iter_errors(&json).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{json}: {errors:?}");
    }
}
