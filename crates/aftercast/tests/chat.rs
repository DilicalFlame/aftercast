//! Phase 3 acceptance (specs/14-roadmap.md) against `--mock`: chat drives `!q` into the
//! queue, showing it reaches the `question` overlay over the WebSocket, a mod's
//! `!chapter` is latency-corrected, the chat reply cap downgrades to the overlay, and
//! `!lat` measures latency. The session runs headless with commands on stdin.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::Value;

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_questions_overlay_chapter_cap_and_latency() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    let init = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output();
    assert!(init.unwrap().status.success());
    let port = free_port();
    let config = home.join("aftercast.toml");
    let toml = fs::read_to_string(&config)
        .unwrap()
        .replace("port = 7878", &format!("port = {port}"))
        .replace(
            "max_chat_replies_per_hour = 30",
            "max_chat_replies_per_hour = 1",
        )
        .replace(
            "current_quest_file = \"\"",
            "current_quest_file = \"quest.json\"",
        );
    fs::write(&config, toml).unwrap();
    fs::write(
        home.join("quest.json"),
        r#"{ "id": "rust/R-C-07", "title": "Build a TUI todo app", "rank": "C" }"#,
    )
    .unwrap();

    let fixture: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
    let mut child = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("--mock")
        .arg(&fixture)
        .args(["abcdefghijk", "--no-tui"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut say = |lines: &str| {
        stdin.write_all(lines.as_bytes()).unwrap();
        stdin.flush().unwrap();
    };

    // The server is up once the session started; retry until it accepts.
    let url = format!("ws://127.0.0.1:{port}/ws");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut ws = loop {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => break ws,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(e) => panic!("overlay server not reachable: {e}"),
        }
    };

    // Spaced past the 3 s global and 20 s per-viewer cooldowns.
    say("+1s chat bob !q why rust over go?\n\
         +25s chat bob !q and another one?\n\
         +5s chat cy !q Why Rust over Go\n");
    let sent = Instant::now();
    say("question show\n");
    let shown = loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("overlay message")
            .unwrap()
            .unwrap();
        let msg: Value = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
        if msg["overlay"] == "question" && msg["action"] == "show" {
            break msg;
        }
    };
    let latency = sent.elapsed();
    assert_eq!(shown["data"]["author"], "bob");
    assert_eq!(shown["data"]["text"], "why rust over go?");
    assert!(latency < Duration::from_millis(100), "{latency:?}");

    say("+5s chat ana mod !chapter Parser with nom\n\
         +5s chat dan !links\n\
         +5s chat eve !links\n\
         +1s chat fay super:₹100.00 keep it up!\n\
         +1s chat gus joined Welcome to the channel!\n\
         +1s question answer 1\n\
         +1s latency\n\
         +240s chat me owner !lat 2350\n\
         +20s latency\n\
         +1s question show 2\n\
         quit\n");
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let only_child = |dir: PathBuf| fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
    let dir = only_child(only_child(home.join("sessions")));
    let events: Vec<Value> = fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let of = |ty: &str| -> Vec<&Value> { events.iter().filter(|e| e["type"] == ty).collect() };

    // One question per viewer, near-duplicates dropped; the Super Chat and the membership
    // join the inbox, pinned first.
    let added = of("question.add");
    assert_eq!(added.len(), 3, "{added:?}");
    assert_eq!(added[1]["data"]["support"]["label"], "₹100.00");
    assert_eq!(added[1]["data"]["support"]["kind"], "superchat");
    assert_eq!(added[1]["data"]["text"], "keep it up!");
    assert_eq!(added[2]["data"]["support"]["kind"], "membership");
    assert!(added[0]["data"].get("support").is_none());
    // `t` on a supporter thanks them quietly: answered, never on stream.
    let fay = added[1]["data"]["qid"].as_str().unwrap();
    let answered: Vec<&str> = of("question.answered")
        .iter()
        .map(|e| e["data"]["qid"].as_str().unwrap())
        .collect();
    assert!(answered.contains(&fay), "{answered:?}");
    assert!(of("question.show").iter().all(|e| e["data"]["qid"] != fay));
    let toasts: Vec<&str> = of("bot.cmd")
        .iter()
        .map(|e| e["data"]["cmd"].as_str().unwrap())
        .collect();
    assert_eq!(toasts.iter().filter(|c| **c == "q").count(), 3);
    // Shown, then answered by the second `question show` (bob is 2nd in the Inbox, after
    // the membership).
    assert_eq!(of("question.show").len(), 1);
    assert_eq!(of("question.answered").len(), 2);

    // The mod's chapter points `latency` back from when YouTube says the message was sent
    // (default latency 5 s; script chat is sent and received at the same moment).
    let cp = of("checkpoint")[0];
    assert_eq!(cp["data"]["title"], "Parser with nom");
    assert_eq!(cp["data"]["by"], "mod");
    assert_eq!(cp["data"]["latency_corrected_s"], 5.0);
    // `latency` wraps the measurement in its own chapter and returns to the previous one.
    let titles: Vec<&str> = of("checkpoint")
        .iter()
        .map(|e| e["data"]["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["Parser with nom", "Latency check", "Parser with nom"]
    );

    // Cap of one chat reply per hour: the second `!links` goes to the overlay.
    let modes: Vec<&str> = of("bot.reply")
        .iter()
        .filter(|e| e["data"]["cmd"] == "links")
        .map(|e| e["data"]["mode"].as_str().unwrap())
        .collect();
    assert_eq!(modes, ["chat", "overlay"]);

    // LAT 2350 is on screen 235 s after the start; the `!lat` is sent 240 s in.
    let lat = of("latency.measured");
    assert_eq!(lat[0]["data"]["latency_s"], 5.0);
    assert!(
        fs::read_to_string(dir.join("session.toml"))
            .unwrap()
            .contains("latency_s = 5.0")
    );

    let quest = of("quest.set");
    assert_eq!(quest[0]["data"]["source"], "external");
    assert_eq!(quest[0]["data"]["id"], "rust/R-C-07");
    assert_eq!(quest[0]["data"]["extra"]["rank"], "C");

    let chat: Vec<Value> = fs::read_to_string(dir.join("chat.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(chat.len(), 9);
    let ts = |v: &Value| serde_json::from_value::<ac_core::time::Timestamp>(v.clone()).unwrap();
    let received = chat
        .iter()
        .find(|m| m["text"] == "!chapter Parser with nom")
        .unwrap();
    let back = ts(&received["t_received"]).secs_since(ts(&cp["data"]["pressed_at"]));
    assert!((back - 5.0).abs() < 0.001, "{back}");

    let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/events.schema.json");
    let schema = serde_json::from_str(&fs::read_to_string(schema).unwrap()).unwrap();
    let v = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    for e in &events {
        let errors: Vec<String> = v.iter_errors(e).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{e}: {errors:?}");
    }
}
