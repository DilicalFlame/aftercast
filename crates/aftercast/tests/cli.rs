//! End-to-end tests of the `aftercast` binary.

use std::path::Path;
use std::process::{Command, Output};

fn aftercast(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aftercast"))
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .expect("binary runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn stubbed_subcommand_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = aftercast(dir.path(), &["export", "silences"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not yet implemented"));
}

#[test]
fn sessions_lists_nothing_then_json() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("Streams");
    assert_eq!(aftercast(&home, &["init"]).status.code(), Some(0));
    let out = aftercast(&home, &["sessions"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("no sessions yet"));
    let out = aftercast(&home, &["sessions", "--json"]);
    assert_eq!(stdout(&out).trim(), "[]");
}

#[test]
fn init_creates_tree_then_skips_existing() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("Streams");

    let out = aftercast(&home, &["init"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!stdout(&out).contains("skipped"));
    for path in [
        "aftercast.toml",
        "templates/session-readme.md",
        "templates/partials/links.md",
        "templates/partials/support.md",
        "state/state.toml",
    ] {
        assert!(home.join(path).is_file(), "{path}");
    }
    for path in ["overlays", "logs", "library", "sessions"] {
        assert!(home.join(path).is_dir(), "{path}");
    }

    let out = aftercast(&home, &["init"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out).matches("exists, skipped").count(), 5);
}

#[test]
fn doctor_with_invalid_toml_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("aftercast.toml");
    std::fs::write(
        &config,
        "[channel]
episode_pad = \"3\"
",
    )
    .unwrap();
    let out = aftercast(dir.path(), &["doctor"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stdout(&out).contains("aftercast.toml:2:15: "),
        "{}",
        stdout(&out)
    );
}
