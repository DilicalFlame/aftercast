//! `--mock` backend: canned responses, recorded requests, no network.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use ac_core::chat::{ChatKind, ChatMessage};
use ac_core::time::{Clock, Timestamp};
use serde_json::{Value, json};

use crate::chat::ChatUpdate;
use crate::{Error, Request, Result, api_error};

pub(crate) struct Mock {
    fixture: PathBuf,
    record_dir: PathBuf,
    /// Substituted for `"@now"` in fixtures, so broadcasts can start "now".
    now: Timestamp,
    counter: AtomicU32,
}

impl Mock {
    pub(crate) fn new(fixture: &Path, record_dir: &Path, now: Timestamp) -> Self {
        Self {
            fixture: fixture.to_path_buf(),
            record_dir: record_dir.to_path_buf(),
            now,
            // Continue numbering after earlier runs (a resumed session) instead of overwriting.
            counter: AtomicU32::new(fs::read_dir(record_dir).map_or(0, |d| d.count() as u32)),
        }
    }

    /// Records the request, then answers from `<fixture>/youtube/<method>.json`. A fixture
    /// holding `{"error": {...}}` is returned as an API error; a write without a fixture
    /// echoes its body.
    pub(crate) fn send(&self, request: &Request) -> Result<Value> {
        let name = request.method.name();
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let query: serde_json::Map<String, Value> = request
            .query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Value::from(v.as_str())))
            .collect();
        let record = json!({ "method": name, "query": query, "body": request.body });
        fs::create_dir_all(&self.record_dir)
            .map_err(|e| Error::Mock(format!("{}: {e}", self.record_dir.display())))?;
        let record_path = self.record_dir.join(format!("{n:03}-{name}.json"));
        fs::write(
            &record_path,
            serde_json::to_string_pretty(&record).expect("record serializes"),
        )
        .map_err(|e| Error::Mock(format!("{}: {e}", record_path.display())))?;

        let path = self.fixture.join("youtube").join(format!("{name}.json"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text.replace("\"@now\"", &format!("\"{}\"", self.now)),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return request
                    .body
                    .clone()
                    .ok_or_else(|| Error::Mock(format!("{}: no fixture", path.display())));
            }
            Err(e) => return Err(Error::Mock(format!("{}: {e}", path.display()))),
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Mock(format!("{}: {e}", path.display())))?;
        match value["error"]["code"].as_u64() {
            Some(code) => Err(api_error(code as u16, &value)),
            None => Ok(value),
        }
    }
}

/// One line of `<fixture>/chat.jsonl`; `after_ms` counts from the chat start.
#[derive(serde::Deserialize)]
struct ScriptedChat {
    after_ms: u64,
    author: String,
    text: String,
    #[serde(default, rename = "mod")]
    is_mod: bool,
    #[serde(default)]
    owner: bool,
    #[serde(default)]
    member: bool,
}

impl Mock {
    /// Plays `<fixture>/chat.jsonl` in real time (missing file: no chat).
    pub(crate) async fn play_chat(&self, clock: &Clock, send: impl Fn(ChatUpdate)) {
        let path = self.fixture.join("chat.jsonl");
        let Ok(text) = fs::read_to_string(&path) else {
            return;
        };
        let start = tokio::time::Instant::now();
        for (n, line) in text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
        {
            let line: ScriptedChat = match serde_json::from_str(line) {
                Ok(line) => line,
                Err(e) => {
                    tracing::warn!("{}:{}: {e}", path.display(), n + 1);
                    continue;
                }
            };
            tokio::time::sleep_until(start + std::time::Duration::from_millis(line.after_ms)).await;
            let now = clock.now_ts();
            send(ChatUpdate::Message(ChatMessage {
                id: format!("mock-{n}"),
                t_received: now,
                t_published: Some(now),
                author_channel_id: format!("UC-{}", line.author),
                author_name: line.author,
                is_owner: line.owner,
                is_moderator: line.is_mod,
                is_member: line.member,
                kind: ChatKind::Text,
                text: line.text,
                amount: None,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use ac_core::time::Clock;

    use crate::quota::QuotaGuard;
    use crate::{Client, Error, LiveContent};

    fn client(dir: &std::path::Path) -> Client {
        let quota = QuotaGuard::open(&dir.join("quota.json"), 1500, BTreeMap::new()).unwrap();
        Client::mock(
            &dir.join("fixture"),
            &dir.join("requests"),
            quota,
            Arc::new(Clock::new()),
        )
    }

    fn fixture(dir: &std::path::Path, method: &str, body: &str) {
        let path = dir.join("fixture/youtube");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join(format!("{method}.json")), body).unwrap();
    }

    #[tokio::test]
    async fn push_sends_full_snippet_and_records_it() {
        let dir = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            "videos.list",
            r#"{ "items": [{ "id": "abcdefghijk",
                 "snippet": { "title": "IDD #042", "categoryId": "28", "tags": ["rust"], "description": "old",
                              "channelId": "UC1", "liveBroadcastContent": "live", "thumbnails": {} },
                 "liveStreamingDetails": { "actualStartTime": "@now" } }] }"#,
        );
        let mut c = client(dir.path());
        let spent = Arc::new(Mutex::new(Vec::new()));
        let sink = spent.clone();
        c.on_spent(move |s| sink.lock().unwrap().push(s));

        let b = c.video("abcdefghijk").await.unwrap();
        assert_eq!(b.content, LiveContent::Live);
        assert!(b.actual_start.is_some(), "@now substituted");

        c.update_description("abcdefghijk", "new text", false)
            .await
            .unwrap();
        let recorded: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("requests/003-videos.update.json")).unwrap(),
        )
        .unwrap();
        let snippet = &recorded["body"]["snippet"];
        assert_eq!(snippet["title"], "IDD #042");
        assert_eq!(snippet["categoryId"], "28");
        assert_eq!(snippet["tags"][0], "rust");
        assert_eq!(snippet["description"], "new text");
        assert!(
            snippet.get("thumbnails").is_none(),
            "read-only fields are not sent"
        );

        let units: Vec<u32> = spent.lock().unwrap().iter().map(|s| s.units).collect();
        assert_eq!(units, [1, 1, 50]);
        assert_eq!(c.quota().remaining(Clock::new().now_ts()), 10_000 - 52);
    }

    #[tokio::test]
    async fn api_errors_from_fixtures() {
        let dir = tempfile::tempdir().unwrap();
        fixture(
            dir.path(),
            "videos.list",
            r#"{ "error": { "code": 403, "message": "out", "errors": [{ "reason": "quotaExceeded" }] } }"#,
        );
        let c = client(dir.path());
        assert!(matches!(
            c.video("abcdefghijk").await,
            Err(Error::QuotaExhausted)
        ));
        assert!(matches!(
            c.update_description("abcdefghijk", "x", true).await,
            Err(Error::QuotaExhausted)
        ));
    }
}
