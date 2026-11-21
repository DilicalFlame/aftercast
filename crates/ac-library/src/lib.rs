//! Cross-session index in `<home>/library/index.sqlite` (specs/11-post-pipeline.md, stage
//! 14): sessions, chapters, segment totals, the transcript (FTS5), suggestions and rendered
//! outputs. Everything comes from a session's files, so the index can always be rebuilt.

use std::fs;
use std::path::{Path, PathBuf};

use ac_core::Home;
use ac_core::config::Config;
use ac_core::session::Session;
use rusqlite::{Connection, params};
use serde::Deserialize;

pub type Result<T> = std::result::Result<T, String>;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions(
    name TEXT PRIMARY KEY, episode INTEGER, created_at TEXT, title TEXT, status TEXT,
    duration_s REAL, totals TEXT, stamp TEXT);
CREATE TABLE IF NOT EXISTS chapters(session TEXT, at TEXT, title TEXT);
CREATE TABLE IF NOT EXISTS suggestions(session TEXT, id TEXT, kind TEXT, title TEXT, json TEXT);
CREATE TABLE IF NOT EXISTS outputs(session TEXT, kind TEXT, dir TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS transcript USING fts5(
    text, context UNINDEXED, session UNINDEXED, start UNINDEXED, end UNINDEXED);
";

/// Every session folder (`sessions/<year>/<name>/session.toml`), oldest first.
pub fn session_dirs(home: &Home, config: &Config) -> Vec<PathBuf> {
    let root = ac_core::session::sessions_dir(home, config);
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|year| fs::read_dir(year.path()).into_iter().flatten().flatten())
        .map(|d| d.path())
        .filter(|d| d.join("session.toml").exists())
        .collect();
    dirs.sort_by_key(|d| d.file_name().map(|n| n.to_owned()));
    dirs
}

/// A session by folder name (`2026-11-19_ep042`), episode (`42`, `ep042`) or `rehearsal`
/// (episode 0). The newest match wins.
pub fn find_session(home: &Home, config: &Config, arg: &str) -> Option<PathBuf> {
    let dirs = session_dirs(home, config);
    if let Some(d) = dirs.iter().find(|d| name(d) == arg) {
        return Some(d.clone());
    }
    let episode = if arg.eq_ignore_ascii_case("rehearse") {
        ac_core::session::REHEARSE_EPISODE
    } else {
        arg.trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .parse()
            .ok()?
    };
    dirs.into_iter()
        .rev()
        .find(|d| Session::open(d).is_ok_and(|s| s.meta.episode == episode))
}

/// The newest real (not rehearsal) session, else the rehearsal.
pub fn latest_session(home: &Home, config: &Config) -> Option<PathBuf> {
    let dirs = session_dirs(home, config);
    dirs.iter()
        .rev()
        .find(|d| {
            Session::open(d).is_ok_and(|s| s.meta.episode != ac_core::session::REHEARSE_EPISODE)
        })
        .or(dirs.last())
        .cloned()
}

/// A transcript search hit; times are master seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub session: String,
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// The segments before and after.
    pub context: String,
}

pub struct Library {
    conn: Connection,
}

#[derive(Deserialize)]
struct Words {
    segments: Vec<Segment>,
}

#[derive(Deserialize)]
struct Segment {
    start: f64,
    end: f64,
    text: String,
}

#[derive(Deserialize)]
struct Suggestions {
    items: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Master {
    duration_s: f64,
}

fn db(e: rusqlite::Error) -> String {
    format!("library: {e}")
}

impl Library {
    pub fn open(home: &Home) -> Result<Self> {
        let dir = home.join("library");
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let conn = Connection::open(dir.join("index.sqlite")).map_err(db)?;
        conn.execute_batch(SCHEMA).map_err(db)?;
        Ok(Self { conn })
    }

    /// Re-indexes every session whose files changed since it was indexed; drops sessions
    /// that no longer exist. Returns how many were (re)indexed.
    pub fn sync(&mut self, home: &Home, config: &Config) -> Result<usize> {
        let dirs = session_dirs(home, config);
        let names: Vec<String> = dirs.iter().map(|d| name(d)).collect();
        let known: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT name, stamp FROM sessions")
                .map_err(db)?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(db)?
                .collect::<std::result::Result<_, _>>()
                .map_err(db)?
        };
        for (gone, _) in known.iter().filter(|(n, _)| !names.contains(n)) {
            self.remove(gone)?;
        }
        let mut count = 0;
        for dir in &dirs {
            let stamp = stamp(dir);
            if !known.iter().any(|(n, s)| *n == name(dir) && *s == stamp) {
                self.upsert(dir)?;
                count += 1;
            }
        }
        Ok(count)
    }

    /// Drops everything and indexes every session again.
    pub fn rebuild(&mut self, home: &Home, config: &Config) -> Result<usize> {
        self.conn
            .execute_batch(
                "DELETE FROM sessions; DELETE FROM chapters; DELETE FROM suggestions;
                 DELETE FROM outputs; DELETE FROM transcript;",
            )
            .map_err(db)?;
        self.sync(home, config)
    }

    fn remove(&mut self, session: &str) -> Result<()> {
        for table in [
            "sessions",
            "chapters",
            "suggestions",
            "outputs",
            "transcript",
        ] {
            let column = if table == "sessions" {
                "name"
            } else {
                "session"
            };
            self.conn
                .execute(
                    &format!("DELETE FROM {table} WHERE {column} = ?1"),
                    [session],
                )
                .map_err(db)?;
        }
        Ok(())
    }

    /// (Re)indexes one session folder.
    pub fn upsert(&mut self, dir: &Path) -> Result<()> {
        let session = Session::open(dir).map_err(|e| e.to_string())?;
        let name = name(dir);
        let read = |rel: &str| fs::read_to_string(dir.join(rel)).ok();
        let duration = read("media/master.json")
            .and_then(|t| serde_json::from_str::<Master>(&t).ok())
            .map(|m| m.duration_s);
        let segments = read("derived/segments.json")
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let totals = segments.as_ref().map(|v| v["totals"].to_string());
        // Nothing from redacted ranges, even if words.json predates the redaction.
        let redacted: Vec<(f64, f64)> = segments
            .as_ref()
            .and_then(|v| v["segments"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter(|s| s["kind"] == "redacted")
            .filter_map(|s| Some((s["start"].as_f64()?, s["end"].as_f64()?)))
            .collect();
        let words: Vec<Segment> = read("derived/words.json")
            .and_then(|t| serde_json::from_str::<Words>(&t).ok())
            .map_or_else(Vec::new, |w| w.segments)
            .into_iter()
            .filter(|s| !redacted.iter().any(|(a, b)| s.end > *a && s.start < *b))
            .collect();
        let suggestions = read("ai/suggestions.json")
            .and_then(|t| serde_json::from_str::<Suggestions>(&t).ok())
            .map_or_else(Vec::new, |s| s.items);
        let chapters = read("derived/index.md").map_or_else(Vec::new, |md| chapters(&md));

        self.remove(&name)?;
        let tx = self.conn.transaction().map_err(db)?;
        let meta = &session.meta;
        tx.execute(
            "INSERT INTO sessions VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                name,
                meta.episode,
                serde_json::to_value(meta.created_at)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from)),
                meta.title_at_start,
                serde_json::to_value(meta.status)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from)),
                duration,
                totals,
                stamp(dir),
            ],
        )
        .map_err(db)?;
        for (at, title) in chapters {
            tx.execute(
                "INSERT INTO chapters VALUES (?1, ?2, ?3)",
                params![name, at, title],
            )
            .map_err(db)?;
        }
        for (i, s) in words.iter().enumerate() {
            let near =
                |j: Option<usize>| j.and_then(|j| words.get(j)).map_or("", |s| s.text.as_str());
            let context = format!("{} / {}", near(i.checked_sub(1)), near(Some(i + 1)));
            tx.execute(
                "INSERT INTO transcript(text, context, session, start, end) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![s.text, context.trim_matches([' ', '/']), name, s.start, s.end],
            )
            .map_err(db)?;
        }
        for item in &suggestions {
            tx.execute(
                "INSERT INTO suggestions VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    name,
                    item["id"].as_str(),
                    item["kind"].as_str(),
                    item["title"].as_str(),
                    item.to_string()
                ],
            )
            .map_err(db)?;
        }
        for (kind, out) in outputs(dir) {
            tx.execute(
                "INSERT INTO outputs VALUES (?1, ?2, ?3)",
                params![name, kind, out],
            )
            .map_err(db)?;
        }
        tx.commit().map_err(db)
    }

    /// Transcript segments matching every word of `query`, best first.
    pub fn search(&self, query: &str, session: Option<&str>, limit: usize) -> Result<Vec<Hit>> {
        // Each word as a quoted FTS5 string, so punctuation in the query is not syntax.
        let q: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
            .collect();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session, start, end, text, context FROM transcript
                 WHERE transcript MATCH ?1 AND (?2 IS NULL OR session = ?2)
                 ORDER BY rank LIMIT ?3",
            )
            .map_err(db)?;
        stmt.query_map(params![q.join(" "), session, limit as i64], |r| {
            Ok(Hit {
                session: r.get(0)?,
                start: r.get(1)?,
                end: r.get(2)?,
                text: r.get(3)?,
                context: r.get(4)?,
            })
        })
        .map_err(db)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db)
    }
}

fn name(dir: &Path) -> String {
    dir.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Size and modification time of the files the index reads, plus the rendered outputs.
fn stamp(dir: &Path) -> String {
    let mut parts: Vec<String> = [
        "session.toml",
        "media/master.json",
        "derived/words.json",
        "derived/segments.json",
        "derived/index.md",
        "ai/suggestions.json",
    ]
    .iter()
    .map(|rel| {
        fs::metadata(dir.join(rel)).map_or_else(
            |_| "-".to_owned(),
            |m| format!("{}:{:?}", m.len(), m.modified().ok()),
        )
    })
    .collect();
    parts.extend(outputs(dir).into_iter().map(|(_, d)| d));
    parts.join("|")
}

/// Rendered items: (`kind` folder, path relative to the session) for each `out/*/*/meta.json`.
fn outputs(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for kind in fs::read_dir(dir.join("out"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for item in fs::read_dir(kind.path()).into_iter().flatten().flatten() {
            if item.path().join("meta.json").exists() {
                let k = kind.file_name().to_string_lossy().into_owned();
                let i = item.file_name().to_string_lossy().into_owned();
                out.push((k.clone(), format!("out/{k}/{i}")));
            }
        }
    }
    out.sort();
    out
}

/// `(HH:MM:SS, title)` pairs from the chapters line of `index.md`.
fn chapters(index: &str) -> Vec<(String, String)> {
    index
        .lines()
        .skip_while(|l| !l.starts_with("## Chapters"))
        .nth(1)
        .map_or_else(Vec::new, |line| {
            line.split(" · ")
                .filter_map(|c| c.split_once(' '))
                .map(|(at, title)| (at.to_owned(), title.to_owned()))
                .collect()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::session::NewSession;
    use ac_core::time::Timestamp;

    #[test]
    fn rehearse_is_episode_zero_and_never_latest() {
        let tmp = tempfile::tempdir().unwrap();
        let home = Home::new(tmp.path());
        home.init().unwrap();
        let config = Config::default();
        let new = |id: &str| NewSession {
            video_id: id.into(),
            ..NewSession::default()
        };
        let t: Timestamp = "2026-11-20T10:00:00Z".parse().unwrap();
        let real = Session::create(&home, &config, new("realvideo01"), t).unwrap();
        let rehearse = Session::create_rehearse(&home, &config, new("rehearse01"), t).unwrap();
        assert_eq!(rehearse.meta.episode, 0);
        assert!(name(&rehearse.dir).ends_with("_ep000"));

        assert_eq!(latest_session(&home, &config), Some(real.dir.clone()));
        for arg in ["rehearse", "0", "ep000"] {
            assert_eq!(
                find_session(&home, &config, arg),
                Some(rehearse.dir.clone()),
                "{arg}"
            );
        }
        assert_eq!(
            find_session(&home, &config, "ep001"),
            Some(real.dir.clone())
        );
        assert_eq!(find_session(&home, &config, "nope"), None);
        // The next real stream is still #2; wiping removes only the rehearsal.
        let state = ac_core::state::State::load(&home.state_file()).unwrap();
        assert_eq!(state.next_episode, 2);
        assert_eq!(Session::remove_rehearses(&home, &config).unwrap(), 1);
        assert_eq!(session_dirs(&home, &config), [real.dir]);
    }

    #[test]
    fn chapters_come_from_the_index_line() {
        let md = "# x\n\n## Chapters (as published)\n00:00:00 Start · 00:12:40 Project setup\n\n## Marks";
        assert_eq!(
            chapters(md),
            [
                ("00:00:00".into(), "Start".into()),
                ("00:12:40".into(), "Project setup".into())
            ]
        );
    }
}
