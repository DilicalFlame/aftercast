//! Keystroke filtering, the secret guard and `keys.jsonl.zst` (specs/10-capture.md).
//! Pure logic: the hook feeds [`RawKey`]s, the controller decides with [`Filter`] and
//! holds records in a [`Guard`] until they can no longer be part of a secret.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use ac_core::config;
use ac_core::time::Timestamp;
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Modifier names, in the order they are stored.
pub const MODS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Win"];

/// Printable characters kept per app for the secret check.
pub const TYPED_WINDOW: usize = 128;

/// A typed character older than this can no longer start a secret (a token typed with a
/// pause longer than this in the middle is not caught).
pub const COOL_AFTER_S: f64 = 30.0;

/// One key press as the hook saw it, before any filtering.
#[derive(Debug, Clone, PartialEq)]
pub struct RawKey {
    pub t: Timestamp,
    /// rdev `Key` debug name, e.g. `KeyA`, `Return`, `F5`.
    pub key: String,
    /// The character the press typed, if any.
    pub ch: Option<char>,
    /// Held modifiers, a subset of [`MODS`] in that order.
    pub mods: Vec<&'static str>,
    /// Foreground process name, e.g. `Code.exe`.
    pub app: String,
    /// Foreground window title; only used for the deny rule, never stored.
    pub title: String,
}

/// One line of `keys.jsonl.zst`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyRecord {
    pub t: Timestamp,
    pub k: String,
    pub mods: Vec<String>,
    pub app: String,
    pub ch: Option<String>,
}

impl KeyRecord {
    /// `Ctrl+Shift+P` style label; `None` for plain typing.
    pub fn combo(&self) -> Option<String> {
        let shortcut = self.mods.iter().any(|m| m != "Shift");
        shortcut.then(|| {
            let key = self.k.strip_prefix("Key").unwrap_or(&self.k);
            let key = key
                .strip_prefix("Num")
                .filter(|d| d.len() == 1)
                .unwrap_or(key);
            let mut parts = self.mods.clone();
            parts.push(key.to_owned());
            parts.join("+")
        })
    }
}

/// The filtering rules, applied before anything is stored.
pub struct Filter {
    allow: Vec<String>,
    deny: Vec<Regex>,
    /// aftercast's own global hotkeys: `(mods, key)`.
    hotkeys: Vec<(Vec<String>, String)>,
}

impl Filter {
    pub fn new(cfg: &config::Capture, hotkeys: &[&str]) -> Self {
        Self {
            allow: cfg
                .keys_allow_apps
                .iter()
                .map(|a| a.to_lowercase())
                .collect(),
            deny: cfg
                .keys_deny_title_patterns
                .iter()
                .map(|p| p.0.clone())
                .collect(),
            hotkeys: hotkeys.iter().filter_map(|h| parse_combo(h)).collect(),
        }
    }

    /// What may be stored of `raw`, if anything.
    pub fn check(&self, raw: &RawKey, privacy: bool) -> Option<KeyRecord> {
        if privacy || self.deny.iter().any(|re| re.is_match(&raw.title)) || is_modifier(&raw.key) {
            return None;
        }
        let mods: Vec<String> = raw.mods.iter().map(|m| m.to_string()).collect();
        if self
            .hotkeys
            .iter()
            .any(|(m, k)| *m == mods && *k == raw.key)
        {
            return None;
        }
        let allowed = self.allow.contains(&raw.app.to_lowercase());
        let shortcut = raw.mods.iter().any(|m| *m != "Shift");
        if !allowed && !shortcut {
            return None;
        }
        let ch = raw
            .ch
            .filter(|c| allowed && !shortcut && (!c.is_control() || *c == '\n' || *c == '\t'))
            .map(String::from);
        Some(KeyRecord {
            t: raw.t,
            k: raw.key.clone(),
            mods,
            app: raw.app.clone(),
            ch,
        })
    }
}

fn is_modifier(key: &str) -> bool {
    matches!(
        key,
        "ControlLeft"
            | "ControlRight"
            | "Alt"
            | "AltGr"
            | "ShiftLeft"
            | "ShiftRight"
            | "MetaLeft"
            | "MetaRight"
    )
}

/// `Ctrl+Alt+M` into stored form: (`["Ctrl", "Alt"]`, `KeyM`).
fn parse_combo(s: &str) -> Option<(Vec<String>, String)> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last()?;
    let mut held = Vec::new();
    for m in mods {
        let name = match m.to_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl",
            "alt" | "option" => "Alt",
            "shift" => "Shift",
            "win" | "super" | "meta" | "cmd" => "Win",
            _ => return None,
        };
        held.push(name);
    }
    let held = MODS
        .iter()
        .filter(|m| held.contains(m))
        .map(|m| m.to_string())
        .collect();
    let key = match key.chars().collect::<Vec<_>>().as_slice() {
        [c] if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
        [c] if c.is_ascii_digit() => format!("Num{c}"),
        _ => key.to_string(),
    };
    Some((held, key))
}

/// Holds records until they can no longer be part of a secret, and drops the keys of any
/// secret typed in the meantime.
pub struct Guard {
    secrets: Vec<Regex>,
    next: u64,
    pending: VecDeque<(u64, KeyRecord)>,
    /// Per app: the last typed characters with their record ids and times.
    typed: HashMap<String, VecDeque<(u64, char, Timestamp)>>,
    /// Ids of records still in some `typed` window.
    hot: HashSet<u64>,
    /// Ids in a `typed` window whose records were dropped as part of a secret.
    dropped: HashSet<u64>,
}

impl Guard {
    pub fn new(secrets: &[config::Pattern]) -> Self {
        Self {
            secrets: secrets.iter().map(|p| p.0.clone()).collect(),
            next: 0,
            pending: VecDeque::new(),
            typed: HashMap::new(),
            hot: HashSet::new(),
            dropped: HashSet::new(),
        }
    }

    /// Adds a record; `true` if this completed a new secret, whose keys are then dropped.
    pub fn push(&mut self, rec: KeyRecord) -> bool {
        let id = self.next;
        self.next += 1;
        let typed = self.typed.entry(rec.app.clone()).or_default();
        let ch = rec.ch.as_deref().and_then(|s| s.chars().next());
        if let Some(c) = ch {
            typed.push_back((id, c, rec.t));
            self.hot.insert(id);
            if typed.len() > TYPED_WINDOW
                && let Some((old, _, _)) = typed.pop_front()
            {
                self.hot.remove(&old);
                self.dropped.remove(&old);
            }
        } else if rec.k == "BackSpace"
            && rec.mods.is_empty()
            && let Some((gone, _, _)) = typed.pop_back()
        {
            self.hot.remove(&gone);
            self.dropped.remove(&gone);
        }
        self.pending.push_back((id, rec));
        self.drop_secrets()
    }

    /// Drops the records of every secret match. Matched characters stay in the typed text
    /// (marked dropped), so the rest of a token typed after the pattern first matches
    /// extends the match and is dropped too. `true` when a new secret started.
    fn drop_secrets(&mut self) -> bool {
        let mut fresh = false;
        let mut newly = HashSet::new();
        for typed in self.typed.values() {
            let text: String = typed.iter().map(|(_, c, _)| *c).collect();
            // Byte offset of each character, to map matches back to records.
            let starts: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
            for re in &self.secrets {
                for m in re.find_iter(&text) {
                    let ids: Vec<u64> = starts
                        .iter()
                        .zip(typed)
                        .filter(|(s, _)| **s >= m.start() && **s < m.end())
                        .map(|(_, (id, _, _))| *id)
                        .collect();
                    if ids.iter().all(|id| !self.dropped.contains(id)) {
                        fresh = true;
                    }
                    newly.extend(ids.into_iter().filter(|id| !self.dropped.contains(id)));
                }
            }
        }
        if !newly.is_empty() {
            self.pending.retain(|(id, _)| !newly.contains(id));
            self.dropped.extend(newly);
        }
        fresh
    }

    /// Records that can be written now: everything before the oldest one still in a typed
    /// window. Characters typed before `now - COOL_AFTER_S` leave their window.
    pub fn take_cold(&mut self, now: Timestamp) -> Vec<KeyRecord> {
        for typed in self.typed.values_mut() {
            while let Some((id, _, t)) = typed.front()
                && now.secs_since(*t) > COOL_AFTER_S
            {
                self.hot.remove(id);
                self.dropped.remove(id);
                typed.pop_front();
            }
        }
        let mut out = Vec::new();
        while let Some((id, _)) = self.pending.front()
            && !self.hot.contains(id)
        {
            out.extend(self.pending.pop_front().map(|(_, r)| r));
        }
        out
    }

    /// Everything still held (session end).
    pub fn take_all(&mut self) -> Vec<KeyRecord> {
        self.typed.clear();
        self.hot.clear();
        self.dropped.clear();
        self.pending.drain(..).map(|(_, r)| r).collect()
    }
}

/// Appends `records` as one zstd frame; concatenated frames form a valid `.zst` file, so a
/// crash loses at most the frame being written.
pub fn append(path: &Path, records: &[KeyRecord]) -> io::Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    let mut lines = Vec::new();
    for r in records {
        serde_json::to_writer(&mut lines, r)?;
        lines.push(b'\n');
    }
    let frame = zstd::encode_all(&lines[..], 3)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(&frame)?;
    file.sync_data()
}

/// Reads every record of a `keys.jsonl.zst`.
pub fn read(path: &Path) -> io::Result<Vec<KeyRecord>> {
    let bytes = zstd::decode_all(fs::File::open(path)?)?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .map(|l| serde_json::from_str(l).map_err(io::Error::other))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: f64) -> Timestamp {
        Timestamp::from_unix_ms(1_800_000_000_000).add_secs(secs)
    }

    fn raw(app: &str, key: &str, ch: Option<char>, mods: &[&'static str]) -> RawKey {
        RawKey {
            t: t(0.0),
            key: key.into(),
            ch,
            mods: mods.to_vec(),
            app: app.into(),
            title: "main.rs - aftercast".into(),
        }
    }

    fn filter() -> Filter {
        Filter::new(&config::Capture::default(), &["Ctrl+Alt+M", "Ctrl+Alt+C"])
    }

    #[test]
    fn privacy_drops_everything() {
        let f = filter();
        assert!(
            f.check(&raw("Code.exe", "KeyA", Some('a'), &[]), true)
                .is_none()
        );
        assert!(
            f.check(&raw("Code.exe", "KeyS", None, &["Ctrl"]), true)
                .is_none()
        );
    }

    #[test]
    fn denied_titles_drop_everything() {
        let f = filter();
        for title in [
            "Bitwarden",
            "Enter password",
            ".env - aftercast",
            "Sign in to GitHub",
        ] {
            let mut k = raw("Code.exe", "KeyS", None, &["Ctrl"]);
            k.title = title.into();
            assert!(f.check(&k, false).is_none(), "{title}");
        }
    }

    #[test]
    fn allowed_apps_keep_characters_case_insensitively() {
        let f = filter();
        let r = f
            .check(&raw("code.EXE", "KeyA", Some('a'), &[]), false)
            .unwrap();
        assert_eq!((r.k.as_str(), r.ch.as_deref()), ("KeyA", Some("a")));
        let r = f
            .check(&raw("Code.exe", "Return", Some('\n'), &[]), false)
            .unwrap();
        assert_eq!(r.ch.as_deref(), Some("\n"));
        // Shift+letter is typing.
        let r = f
            .check(&raw("Code.exe", "KeyA", Some('A'), &["Shift"]), false)
            .unwrap();
        assert_eq!(r.ch.as_deref(), Some("A"));
    }

    #[test]
    fn other_apps_keep_only_shortcuts_without_characters() {
        let f = filter();
        assert!(
            f.check(&raw("chrome.exe", "KeyA", Some('a'), &[]), false)
                .is_none()
        );
        assert!(
            f.check(&raw("chrome.exe", "KeyA", Some('A'), &["Shift"]), false)
                .is_none()
        );
        let r = f
            .check(&raw("chrome.exe", "KeyT", Some('\u{14}'), &["Ctrl"]), false)
            .unwrap();
        assert_eq!(
            (r.mods.clone(), r.ch.clone()),
            (vec!["Ctrl".to_string()], None)
        );
        assert_eq!(r.combo().as_deref(), Some("Ctrl+T"));
        let r = f
            .check(&raw("chrome.exe", "Tab", None, &["Alt"]), false)
            .unwrap();
        assert_eq!(r.combo().as_deref(), Some("Alt+Tab"));
    }

    #[test]
    fn shortcuts_in_allowed_apps_have_no_character() {
        let f = filter();
        let r = f
            .check(&raw("Code.exe", "KeyS", Some('\u{13}'), &["Ctrl"]), false)
            .unwrap();
        assert_eq!(r.ch, None);
        // AltGr-style characters with Ctrl+Alt are not stored as text either.
        let r = f
            .check(&raw("Code.exe", "KeyQ", Some('@'), &["Ctrl", "Alt"]), false)
            .unwrap();
        assert_eq!(r.ch, None);
    }

    #[test]
    fn modifiers_alone_and_own_hotkeys_are_dropped() {
        let f = filter();
        assert!(
            f.check(&raw("Code.exe", "ControlLeft", None, &["Ctrl"]), false)
                .is_none()
        );
        assert!(
            f.check(&raw("Code.exe", "ShiftLeft", None, &[]), false)
                .is_none()
        );
        assert!(
            f.check(&raw("Code.exe", "KeyM", None, &["Ctrl", "Alt"]), false)
                .is_none()
        );
        assert!(
            f.check(&raw("Code.exe", "KeyM", None, &["Ctrl"]), false)
                .is_some()
        );
    }

    fn typed(app: &str, text: &str, at: f64) -> Vec<KeyRecord> {
        text.chars()
            .enumerate()
            .map(|(i, c)| KeyRecord {
                t: t(at + i as f64 * 0.1),
                k: format!("Key{}", c.to_ascii_uppercase()),
                mods: vec![],
                app: app.into(),
                ch: Some(c.to_string()),
            })
            .collect()
    }

    #[test]
    fn typed_token_never_leaves_the_guard() {
        let mut g = Guard::new(&config::Capture::default().secret_patterns);
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let mut secrets = 0;
        for r in typed("Code.exe", &format!("let t = {token};"), 0.0) {
            secrets += usize::from(g.push(r));
            // Nothing of the token may be released while it is still being typed.
            assert!(g.take_cold(t(1.0)).is_empty());
        }
        assert_eq!(secrets, 1, "one event per token, not per key");
        let out: String = g.take_all().iter().filter_map(|r| r.ch.clone()).collect();
        assert_eq!(out, "let t = ;");
    }

    #[test]
    fn records_cool_after_the_window_or_time() {
        let mut g = Guard::new(&[]);
        for r in typed("Code.exe", &"x".repeat(TYPED_WINDOW + 3), 0.0) {
            g.push(r);
        }
        assert_eq!(g.take_cold(t(0.0)).len(), 3);
        assert_eq!(g.take_cold(t(100.0)).len(), TYPED_WINDOW);
    }

    #[test]
    fn backspace_edits_the_typed_text() {
        let mut g = Guard::new(&config::Capture::default().secret_patterns);
        let mut dropped = false;
        let mut recs = typed("Code.exe", "AKIAX", 0.0);
        recs.push(KeyRecord {
            k: "BackSpace".into(),
            ch: None,
            ..recs[0].clone()
        });
        recs.extend(typed("Code.exe", "ABCDEFGHIJKLMNOP", 1.0));
        for r in recs {
            dropped |= g.push(r);
        }
        assert!(dropped, "AKIA + 16 after deleting the X");
    }

    #[test]
    fn frames_append_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.jsonl.zst");
        let recs = typed("Code.exe", "ab", 0.0);
        append(&path, &recs[..1]).unwrap();
        append(&path, &recs[1..]).unwrap();
        append(&path, &[]).unwrap();
        assert_eq!(read(&path).unwrap(), recs);
    }
}
