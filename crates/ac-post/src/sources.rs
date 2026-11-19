//! Stage 7, git, and readers for the session's capture files used by later stages.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use ac_capture::keys::KeyRecord;
use ac_capture::shell::ShellLine;
use ac_core::chat::ChatMessage;
use ac_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::{Ctx, Result, tool};

/// One line of `git.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub t: Timestamp,
    pub repo: String,
    pub hash: String,
    pub subject: String,
    pub refs: String,
}

/// Session window in wall time: creation to the stream's end (or the last event).
pub fn window(ctx: &Ctx) -> (Timestamp, Timestamp) {
    let meta = &ctx.session.meta;
    let last = ctx.events.last().map_or(meta.created_at, |e| e.t);
    let end = meta.anchors.yt_actual_end.map_or(last, |e| e.max(last));
    (meta.created_at, end)
}

/// Stage 7: commits made during the session in repositories under `capture.git_roots`.
pub fn git(ctx: &mut Ctx) -> Result<()> {
    let (since, until) = window(ctx);
    let roots = ctx.config.capture.git_roots.clone();
    let depth = ctx.config.capture.git_max_depth;
    let mut lines = String::new();
    if !roots.is_empty() {
        tool::note_version(ctx, "git");
    }
    for repo in roots.iter().flat_map(|r| repos(&ctx.home.path(r), depth)) {
        let out = tool::run(
            ctx,
            "git",
            [
                "-C".into(),
                repo.as_os_str().to_owned(),
                "log".into(),
                "--all".into(),
                format!("--since={}", rfc3339(since)).into(),
                format!("--until={}", rfc3339(until)).into(),
                "--format=%H%x1f%aI%x1f%s%x1f%D".into(),
            ],
        )?;
        let name = repo.file_name().map_or_else(
            || repo.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        for line in out.lines() {
            let parts: Vec<&str> = line.split('\u{1f}').collect();
            let [hash, date, subject, refs] = parts[..] else {
                continue;
            };
            let Ok(t) = serde_json::from_value::<Timestamp>(serde_json::Value::String(date.into()))
            else {
                continue;
            };
            let commit = Commit {
                t,
                repo: name.clone(),
                hash: hash.into(),
                subject: subject.into(),
                refs: refs.into(),
            };
            lines.push_str(&serde_json::to_string(&commit).expect("commit serializes"));
            lines.push('\n');
        }
    }
    let path = ctx.dir.join("git.jsonl");
    fs::write(&path, lines).map_err(|e| format!("{}: {e}", path.display()))
}

fn rfc3339(t: Timestamp) -> String {
    serde_json::to_value(t)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Git repositories at most `depth` folders below `root` (not descending into one).
fn repos(root: &Path, depth: usize) -> Vec<PathBuf> {
    const SKIP: [&str; 5] = [
        "node_modules",
        "target",
        ".git",
        "$RECYCLE.BIN",
        "System Volume Information",
    ];
    let mut out = Vec::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), 0)]);
    while let Some((dir, level)) = queue.pop_front() {
        if dir.join(".git").exists() {
            out.push(dir);
            continue;
        }
        if level >= depth {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name();
            let skip = SKIP.iter().any(|s| name.eq_ignore_ascii_case(s));
            if !skip && e.file_type().is_ok_and(|t| t.is_dir()) {
                queue.push_back((e.path(), level + 1));
            }
        }
    }
    out
}

/// Every JSON line of `name` in the session folder that parses as `T`.
pub fn jsonl<T: for<'de> Deserialize<'de>>(ctx: &Ctx, name: &str) -> Vec<T> {
    fs::read_to_string(ctx.dir.join(name))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn keys(ctx: &Ctx) -> Vec<KeyRecord> {
    let path = ctx.dir.join("keys.jsonl.zst");
    if !path.exists() {
        return Vec::new();
    }
    ac_capture::keys::read(&path).unwrap_or_else(|e| {
        tracing::warn!("{}: {e}", path.display());
        Vec::new()
    })
}

pub fn shell(ctx: &Ctx) -> Vec<ShellLine> {
    jsonl(ctx, "shell.jsonl")
}

pub fn chat(ctx: &Ctx) -> Vec<ChatMessage> {
    jsonl(ctx, "chat.jsonl")
}

pub fn commits(ctx: &Ctx) -> Vec<Commit> {
    jsonl(ctx, "git.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_repos_within_depth_without_descending() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in [
            "a/.git",
            "a/inner/.git",
            "b/c/.git",
            "b/c/d/e/.git",
            "target/x/.git",
        ] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let mut found: Vec<String> = repos(root, 2)
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        found.sort();
        assert_eq!(found, ["a", "b/c"]);
    }
}
