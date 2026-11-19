//! Phase 4 acceptance (specs/14-roadmap.md) against `--mock`: typed keys are stored in every
//! app, a `ghp_` token leaves no trace in `keys.jsonl.zst`, password managers and password
//! boxes leave nothing, aftercast's own hotkeys are not recorded, privacy mode drops keys
//! and shell commands, and shell secrets are redacted in `shell.jsonl`.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Appends one command to the shell spool, as the PowerShell hook does (with a BOM first).
fn spool(home: &Path, cmd: &str) {
    let path = home.join("state/shell-spool.jsonl");
    let fresh = !path.exists();
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    if fresh {
        f.write_all("\u{feff}".as_bytes()).unwrap();
    }
    let t = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let line = json!({ "t": t, "cwd": "P:\\OSS\\aftercast", "cmd": cmd });
    writeln!(f, "{line}").unwrap();
}

#[test]
fn keys_and_shell_capture_with_secrets_and_privacy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    let init = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output();
    assert!(init.unwrap().status.success());
    let config = home.join("aftercast.toml");
    let toml = fs::read_to_string(&config)
        .unwrap()
        .replace("port = 7878", &format!("port = {}", free_port()));
    fs::write(&config, toml).unwrap();

    let fixture: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
    let mut child = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("--mock")
        .arg(&fixture)
        .args(["abcdefghijk", "--no-tui"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut say = |lines: &str| {
        stdin.write_all(lines.as_bytes()).unwrap();
        stdin.flush().unwrap();
    };
    // Events printed by the headless session, to wait for each step.
    let (tx, printed) = mpsc::channel();
    let stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let wait_for = |what: &str| loop {
        let line = printed
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("no {what} event"));
        if line.contains(what) {
            return;
        }
    };
    wait_for(" session.start ");

    let token = format!("ghp_{}", "Ab3dEf6hIj".repeat(4));
    spool(&home, "cargo test");
    spool(
        &home,
        &format!("git push https://{token}@github.com/me/repo"),
    );
    say(&format!(
        "shell\n\
         type Code.exe let t = {token};\n\
         key chrome.exe Ctrl+T\n\
         type chrome.exe rust docs\n\
         type-password chrome.exe hunter2\n\
         type Bitwarden.exe vault\n\
         key Code.exe Ctrl+Alt+M\n\
         key Code.exe Ctrl+S\n\
         privacy on\n"
    ));
    wait_for(" privacy.start ");
    spool(&home, "echo private");
    say("shell\ntype Code.exe secretword\nkey Code.exe Ctrl+C\nprivacy off\n");
    wait_for(" privacy.end ");
    spool(&home, "cargo build");
    say("shell\ntype Code.exe ok\nquit\n");
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let only_child = |dir: PathBuf| fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
    let dir = only_child(only_child(home.join("sessions")));

    // Keys: the token, privacy-time typing, other apps' text and our own hotkey are gone.
    let raw = zstd::decode_all(fs::File::open(dir.join("keys.jsonl.zst")).unwrap()).unwrap();
    let raw = String::from_utf8(raw).unwrap();
    assert!(!raw.contains("ghp_") && !raw.contains("Ab3dEf"), "{raw}");
    let keys: Vec<Value> = raw
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let typed: String = keys
        .iter()
        .filter(|k| k["app"] == "Code.exe")
        .filter_map(|k| k["ch"].as_str())
        .collect();
    assert_eq!(typed, "let t = ;ok");
    let combos: Vec<String> = keys
        .iter()
        .filter(|k| k["mods"].as_array().is_some_and(|m| !m.is_empty()))
        .map(|k| format!("{} {} {}", k["app"], k["mods"], k["k"]))
        .collect();
    assert_eq!(
        combos,
        [
            r#""chrome.exe" ["Ctrl"] "KeyT""#,
            r#""Code.exe" ["Ctrl"] "KeyS""#
        ]
    );
    let chrome: String = keys
        .iter()
        .filter(|k| k["app"] == "chrome.exe")
        .filter_map(|k| k["ch"].as_str())
        .collect();
    assert_eq!(chrome, "rust docs");
    assert!(keys.iter().all(|k| k["app"] != "Bitwarden.exe"));
    assert!(!raw.contains("hunter") && !raw.contains("vault"));
    assert!(keys.iter().all(|k| k.get("title").is_none()));

    // Shell: redacted, nothing from privacy mode, in order.
    let shell: Vec<String> = fs::read_to_string(dir.join("shell.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["cmd"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        shell,
        [
            "cargo test",
            "git push https://[REDACTED]@github.com/me/repo",
            "cargo build"
        ]
    );

    let events: Vec<Value> = fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let sources: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "capture.secret_dropped")
        .map(|e| e["data"]["source"].as_str().unwrap())
        .collect();
    assert_eq!(sources, ["shell", "keys"]);
    assert!(events.iter().all(|e| e["type"] != "shell.cmd"));
    let log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(!log.contains("ghp_") && !log.contains("secretword"));

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
