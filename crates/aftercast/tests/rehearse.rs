//! `aftercast rehearse`: simulated YouTube and chat in session episode 0 next to the real
//! ones, replaced by the next rehearsal, without using an episode number, the last-session
//! pointer or today's quota. (OBS points at a closed port here; with OBS running it
//! connects for real.)

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Runs a rehearsal: `script`, then `wait` of real time (the demo chat plays in real time,
/// script delays are virtual), then quit.
fn rehearse(bin: &str, home: &std::path::Path, script: &str, wait: Duration) -> String {
    let mut child = Command::new(bin)
        .arg("--home")
        .arg(home)
        .args(["--no-tui", "rehearse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(script.as_bytes()).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(wait);
    stdin.write_all(b"quit\n").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn rehearse_is_isolated_from_real_sessions_and_quota() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    assert!(
        Command::new(bin)
            .arg("--home")
            .arg(&home)
            .arg("init")
            .status()
            .unwrap()
            .success()
    );
    let config = home.join("aftercast.toml");
    let toml = fs::read_to_string(&config)
        .unwrap()
        .replace("port = 7878", &format!("port = {}", free_port()))
        .replace(
            "url = \"ws://127.0.0.1:4455\"",
            &format!("url = \"ws://127.0.0.1:{}\"", free_port()),
        )
        .replace("password = \"keyring:obs\"", "password = \"\"");
    fs::write(&config, toml).unwrap();

    // The demo chat's first lines arrive after 3 and 8 s.
    let out = rehearse(
        bin,
        &home,
        "checkpoint \"Overlay test\"\n",
        Duration::from_millis(9500),
    );
    assert!(out.contains(" session.start "), "{out}");

    // The rehearsal is episode 0 next to the real sessions.
    let rehearse_dir = home.join("rehearse");
    let session = only_session(&home);
    let name = session.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with("_ep000"), "{name}");
    let chat = fs::read_to_string(session.join("chat.jsonl")).unwrap();
    assert!(chat.contains("what are we building today"), "{chat}");
    assert!(chat.contains("why rust over go"), "{chat}");
    assert!(rehearse_dir.join("chat.jsonl").exists());
    assert!(rehearse_dir.join("quota.json").exists());

    // No episode number used, no last-session pointer, no quota spent.
    let state = fs::read_to_string(home.join("state/state.toml")).unwrap_or_default();
    assert!(!state.contains("last_session"), "{state}");
    assert!(
        !state.contains("next_episode") || state.contains("next_episode = 1"),
        "{state}"
    );
    assert!(!home.join("state/quota.json").exists());

    // Commands find it as `rehearsal` (render reads its suggestions, which do not exist yet).
    let out = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .args(["render", "rehearse", "--dry-run"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&name) && err.contains("suggestions.json"),
        "{err}"
    );

    // An edited demo chat is kept; a second rehearsal replaces the first.
    fs::write(
        rehearse_dir.join("chat.jsonl"),
        "{\"after_ms\": 500, \"author\": \"zed\", \"text\": \"custom line\"}\n",
    )
    .unwrap();
    rehearse(bin, &home, "", Duration::from_secs(2));
    let chat = fs::read_to_string(only_session(&home).join("chat.jsonl")).unwrap();
    assert!(
        chat.contains("custom line") && !chat.contains("building today"),
        "{chat}"
    );
}

/// The one session folder under `<home>/sessions/<year>/`.
fn only_session(home: &std::path::Path) -> std::path::PathBuf {
    let years: Vec<_> = fs::read_dir(home.join("sessions"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(years.len(), 1);
    let sessions: Vec<_> = fs::read_dir(years[0].path()).unwrap().flatten().collect();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    sessions[0].path()
}

/// Minimal HTTP/1.1 request to the local server; returns status and body.
fn http(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    use std::io::Read;
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out[9..12].parse().unwrap();
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b).to_owned();
    (status, body)
}

#[test]
fn rehearse_page_serves_the_feed_and_takes_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    assert!(
        Command::new(bin)
            .arg("--home")
            .arg(&home)
            .arg("init")
            .status()
            .unwrap()
            .success()
    );
    let port = free_port();
    let config = home.join("aftercast.toml");
    let toml = fs::read_to_string(&config)
        .unwrap()
        .replace("port = 7878", &format!("port = {port}"))
        .replace(
            "url = \"ws://127.0.0.1:4455\"",
            &format!("url = \"ws://127.0.0.1:{}\"", free_port()),
        )
        .replace("password = \"keyring:obs\"", "password = \"\"");
    fs::write(&config, toml).unwrap();
    // No demo chat: only what the page sends.
    fs::create_dir_all(home.join("rehearse")).unwrap();
    fs::write(home.join("rehearse/chat.jsonl"), "").unwrap();

    let mut child = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .args(["--no-tui", "rehearse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let ready = (0..100).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    assert!(ready, "server did not start");

    let (status, page) = http(port, "GET", "/rehearse", "");
    assert_eq!(status, 200);
    assert!(
        page.contains("REHEARSE") && page.contains("--accent"),
        "{page}"
    );

    let send = |role: &str, author: &str, text: &str| {
        let body = serde_json::json!({ "author": author, "role": role, "text": text }).to_string();
        http(port, "POST", "/rehearse/chat", &body)
    };
    // `!links` first: the bot's 3 s global cooldown would drop it after another command.
    assert_eq!(send("viewer", "dan", "!links").0, 204);
    assert_eq!(send("moderator", "mo", "!chapter From the page").0, 204);
    assert_eq!(send("guest", "x", "hi").0, 400);
    let sc = serde_json::json!({ "author": "eve", "role": "member", "text": "thanks", "amount": "₹50.00" });
    assert_eq!(http(port, "POST", "/rehearse/chat", &sc.to_string()).0, 204);

    // The bot's chat reply to !links comes back through the chat, from the channel.
    let feed = (0..50)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            let (_, body) = http(port, "GET", "/rehearse/feed", "");
            let feed: serde_json::Value = serde_json::from_str(&body).unwrap();
            let chat = feed["chat"].as_array().unwrap();
            chat.iter()
                .any(|m| m["owner"] == true && m["text"].as_str().unwrap().contains("GitHub"))
                .then_some(feed)
        })
        .unwrap_or_else(|| {
            panic!(
                "bot reply not echoed into chat: {}",
                http(port, "GET", "/rehearse/feed", "").1
            )
        });
    assert_eq!(feed["title"], "Rehearse (not live)");
    assert!(!feed["description"].as_str().unwrap().is_empty(), "{feed}");
    let chat = feed["chat"].as_array().unwrap();
    let mo = chat.iter().find(|m| m["author"] == "mo").unwrap();
    assert_eq!(mo["moderator"], true);
    let eve = chat.iter().find(|m| m["author"] == "eve").unwrap();
    assert_eq!(
        (eve["superchat"].clone(), eve["amount"].clone()),
        (true.into(), "₹50.00".into())
    );

    stdin.write_all(b"quit\n").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = fs::read_to_string(only_session(&home).join("events.jsonl")).unwrap();
    assert!(
        events.contains("\"title\":\"From the page\""),
        "mod chapter from the page"
    );
}
