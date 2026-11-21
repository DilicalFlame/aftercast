//! `--mock` backend: canned responses, recorded requests, no network.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ac_core::chat::{ChatKind, ChatMessage};
use ac_core::time::{Clock, Timestamp};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::chat::ChatUpdate;
use crate::{Error, Request, Result, api_error};

pub(crate) struct Mock {
    fixture: PathBuf,
    record_dir: PathBuf,
    /// Substituted for `"@now"` in fixtures, so broadcasts can start "now".
    now: Timestamp,
    counter: AtomicU32,
    /// Chat typed into a rehearse page (and echoed bot posts), merged into the stream.
    chat_tx: mpsc::UnboundedSender<ChatMessage>,
    chat_rx: Mutex<Option<mpsc::UnboundedReceiver<ChatMessage>>>,
    /// Echo `liveChatMessages.insert` into the chat stream, as YouTube does (rehearse).
    echo: AtomicBool,
}

impl Mock {
    pub(crate) fn new(fixture: &Path, record_dir: &Path, now: Timestamp) -> Self {
        let (chat_tx, chat_rx) = mpsc::unbounded_channel();
        Self {
            chat_tx,
            chat_rx: Mutex::new(Some(chat_rx)),
            echo: AtomicBool::new(false),
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
    /// A sender for chat messages to add to the stream; from now on the channel's own
    /// posts are echoed into it too.
    pub(crate) fn injector(&self) -> mpsc::UnboundedSender<ChatMessage> {
        self.echo.store(true, Ordering::Relaxed);
        self.chat_tx.clone()
    }

    pub(crate) fn send(&self, request: &Request) -> Result<Value> {
        let name = request.method.name();
        if name == "liveChatMessages.insert"
            && self.echo.load(Ordering::Relaxed)
            && let Some(text) = request.body.as_ref().and_then(|b| {
                b["snippet"]["textMessageDetails"]["messageText"]
                    .as_str()
                    .map(String::from)
            })
        {
            let n = self.counter.load(Ordering::Relaxed);
            let _ = self.chat_tx.send(ChatMessage {
                id: format!("echo-{n}"),
                t_received: self.now_or_start(),
                t_published: None,
                author_channel_id: "UC-channel".into(),
                author_name: "Rehearse".into(),
                is_owner: true,
                is_moderator: false,
                is_member: false,
                kind: ChatKind::Text,
                text,
                amount: None,
            });
        }
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
    /// Makes the line a Super Chat, e.g. `"₹100.00"`.
    #[serde(default)]
    amount: Option<String>,
}

impl Mock {
    /// Wall time for echoed messages (the mock has no clock of its own; the chat reader
    /// stamps `t_received` again on arrival).
    fn now_or_start(&self) -> Timestamp {
        Timestamp::now().max(self.now)
    }

    /// Plays `<fixture>/chat.jsonl` in real time (missing file: no scripted chat), merged
    /// with injected messages, which keep arriving after the script ends.
    pub(crate) async fn play_chat(&self, clock: &Clock, send: impl Fn(ChatUpdate)) {
        let path = self.fixture.join("chat.jsonl");
        let text = fs::read_to_string(&path).unwrap_or_default();
        let mut injected = self.chat_rx.lock().expect("not poisoned").take();
        let start = tokio::time::Instant::now();
        // Collected first: a lazy iterator with closures held across `.await` trips a
        // compiler limitation when the future is spawned.
        let mut script: Vec<(usize, ScriptedChat)> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ScriptedChat>(line) {
                Ok(line) => script.push((n, line)),
                Err(e) => tracing::warn!("{}:{}: {e}", path.display(), n + 1),
            }
        }
        let mut lines = script.into_iter().peekable();
        loop {
            let due = lines
                .peek()
                .map(|(_, l)| start + std::time::Duration::from_millis(l.after_ms));
            if due.is_none() && injected.is_none() {
                return;
            }
            let scripted = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let typed = async {
                match injected.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            };
            let line = tokio::select! {
                () = scripted => lines.next().expect("peeked"),
                msg = typed => {
                    match msg {
                        Some(mut msg) => {
                            msg.t_received = clock.now_ts();
                            send(ChatUpdate::Message(msg));
                        }
                        None => injected = None,
                    }
                    continue;
                }
            };
            let (n, line) = line;
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
                kind: if line.amount.is_some() {
                    ChatKind::Superchat
                } else {
                    ChatKind::Text
                },
                text: line.text,
                amount: line.amount,
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
