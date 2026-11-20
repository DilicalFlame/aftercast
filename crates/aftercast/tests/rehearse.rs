//! `aftercast rehearse`: simulated YouTube and chat in a throwaway session under
//! `<home>/rehearsal/`, without touching real sessions, the last-session pointer or today's
//! quota. (OBS points at a closed port here; with OBS running it connects for real.)

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
fn rehearsal_is_isolated_from_real_sessions_and_quota() {
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

    let rehearsal = home.join("rehearsal");
    let year = fs::read_dir(rehearsal.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let session = fs::read_dir(year).unwrap().next().unwrap().unwrap().path();
    let chat = fs::read_to_string(session.join("chat.jsonl")).unwrap();
    assert!(chat.contains("what are we building today"), "{chat}");
    assert!(chat.contains("why rust over go"), "{chat}");
    assert!(rehearsal.join("chat.jsonl").exists());
    assert!(rehearsal.join("quota.json").exists());

    // Nothing real: no session, no last-session pointer, no quota spent.
    let real = fs::read_dir(home.join("sessions")).map_or(0, |d| d.count());
    assert_eq!(real, 0);
    let state = fs::read_to_string(home.join("state/state.toml")).unwrap_or_default();
    assert!(!state.contains("last_session"), "{state}");
    assert!(!home.join("state/quota.json").exists());

    // An edited demo chat is kept; a second rehearsal starts with a fresh session.
    fs::write(
        rehearsal.join("chat.jsonl"),
        "{\"after_ms\": 500, \"author\": \"zed\", \"text\": \"custom line\"}\n",
    )
    .unwrap();
    rehearse(bin, &home, "", Duration::from_secs(2));
    let year = fs::read_dir(rehearsal.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let sessions: Vec<_> = fs::read_dir(year).unwrap().flatten().collect();
    assert_eq!(sessions.len(), 1);
    let chat = fs::read_to_string(sessions[0].path().join("chat.jsonl")).unwrap();
    assert!(
        chat.contains("custom line") && !chat.contains("building today"),
        "{chat}"
    );
}
