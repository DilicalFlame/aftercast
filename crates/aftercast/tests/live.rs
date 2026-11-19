//! Phase 1 acceptance (specs/14-roadmap.md) with `--mock fixtures/basic`: start a session,
//! create 3 checkpoints, edit the README and see it re-rendered, sync (the mock records the
//! request body), quit, restart and find the state (including chapters) restored.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

const VIDEO: &str = "abcdefghijk";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic")
}

fn session(home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aftercast"));
    cmd.arg("--home")
        .arg(home)
        .arg("--mock")
        .arg(fixture())
        .arg(VIDEO)
        .arg("--no-tui")
        .args(args);
    cmd
}

/// Runs a session interactively, handing out stdout lines as they appear.
struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
}

impl Running {
    fn start(home: &Path) -> Self {
        let mut child = session(home, &[])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("binary runs");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    /// Waits for an event line of `kind` whose data matches `pred`.
    fn wait_for(&self, kind: &str, pred: impl Fn(&Value) -> bool) -> Value {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(15))
                .unwrap_or_else(|_| panic!("timed out waiting for {kind}"));
            let mut parts = line.splitn(3, ' ');
            let (_, ty, data) = (parts.next(), parts.next(), parts.next().unwrap_or("null"));
            let data: Value = serde_json::from_str(data).unwrap_or(Value::Null);
            if ty == Some(kind) && pred(&data) {
                return data;
            }
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin.as_mut().unwrap(), "{line}").unwrap();
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}

fn last_update(home: &Path) -> Value {
    let dir = home.join("mock/requests");
    let mut names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with("videos.update.json"))
        .collect();
    names.sort();
    let text = fs::read_to_string(dir.join(names.last().expect("a push was recorded"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn mock_session_checkpoints_sync_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let init = Command::new(env!("CARGO_BIN_EXE_aftercast"))
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output()
        .unwrap();
    assert!(init.status.success());

    // Run 1: three checkpoints, spaced in virtual time, then quit.
    let script = tmp.path().join("run1.txt");
    fs::write(
        &script,
        "+15s checkpoint \"Project setup\"\n+30s cp Parser\n+40s mark nice bit\n+20s checkpoint Tests\nquit\n",
    )
    .unwrap();
    let out = session(&home, &["--script"]).arg(&script).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.matches(" checkpoint ").count(), 3, "{stdout}");
    assert!(stdout.contains("\"chapters\":4"), "{stdout}");

    let only_child = |dir: PathBuf| fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
    let session_dir = only_child(only_child(home.join("sessions")));
    let readme = session_dir.join("README.md");

    // Run 2: edit the README while the session runs, see it re-rendered, then sync.
    let mut run = Running::start(&home);
    run.wait_for("session.start", |d| d["resumed"] == true);
    let before = run.wait_for("desc.rendered", |d| d["ok"] == true)["chars"]
        .as_u64()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let mut text = fs::read_to_string(&readme).unwrap();
    text.push_str("\nEdited during the stream\n");
    fs::write(&readme, text).unwrap();
    run.wait_for("desc.rendered", |d| d["chars"].as_u64() > Some(before));
    run.send("sync");
    run.wait_for("desc.pushed", |d| d["trigger"] == "manual");
    run.send("quit");
    run.finish();

    let push = last_update(&home);
    let snippet = &push["body"]["snippet"];
    assert_eq!(push["body"]["id"], VIDEO);
    assert_eq!(snippet["title"], "IDD #001 · mock stream");
    assert_eq!(snippet["categoryId"], "28");
    let description = snippet["description"].as_str().unwrap();
    assert!(
        description.contains("Edited during the stream"),
        "{description}"
    );
    assert!(
        description.contains("0:00 Start\n0:15 Project setup\n0:45 Parser\n1:45 Tests"),
        "{description}"
    );

    // Run 3: restart; replayed state still has the checkpoints and chapters.
    let out = session(&home, &[]).stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"resumed\":true"), "{stdout}");
    assert!(stdout.contains("\"chapters\":4"), "{stdout}");
    let rendered = fs::read_to_string(session_dir.join(".render/description.txt")).unwrap();
    assert!(rendered.contains("1:45 Tests"), "{rendered}");

    let toml = fs::read_to_string(session_dir.join("session.toml")).unwrap();
    assert!(toml.contains("status = \"live\""), "{toml}");
}
