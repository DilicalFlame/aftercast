//! Phase 2 (OBS) behavior against the fake OBS of `--mock`: BRB returns to the previous
//! scene, manual scene switches in OBS toggle BRB/privacy, recording start/pause/split/stop
//! produce a valid `recordings.json`, and a lost connection shows as `down`.
//! The same checks against a real OBS are in docs/manual-tests.md.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn validator(name: &str) -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(name);
    let schema = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap()
}

fn assert_valid(v: &jsonschema::Validator, json: &Value) {
    let errors: Vec<String> = v.iter_errors(json).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{json}: {errors:?}");
}

/// Runs a headless mock session as a draft (as the TUI does until Enter on Home).
fn run_draft(home: &Path, script: &Path) -> std::process::Output {
    let fixture: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
    Command::new(env!("CARGO_BIN_EXE_aftercast"))
        .arg("--home")
        .arg(home)
        .arg("--mock")
        .arg(&fixture)
        .args(["abcdefghijk", "--no-tui", "--draft", "--script"])
        .arg(script)
        .output()
        .unwrap()
}

#[test]
fn quitting_a_draft_keeps_nothing_and_starting_it_keeps_everything() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    let init = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output();
    assert!(init.unwrap().status.success());
    let state = || fs::read_to_string(home.join("state/state.toml")).unwrap_or_default();
    let before = state();
    let sessions = || {
        fs::read_dir(home.join("sessions"))
            .map(|years| {
                years
                    .flatten()
                    .flat_map(|y| fs::read_dir(y.path()).unwrap())
                    .count()
            })
            .unwrap_or(0)
    };

    // Draft, then quit: the folder is gone and state.toml is as it was.
    let script = tmp.path().join("quit.txt");
    fs::write(
        &script,
        "+1s mute Mic/Aux
quit
",
    )
    .unwrap();
    let out = run_draft(&home, &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sessions(), 0, "the draft was removed");
    assert_eq!(state(), before, "no episode used, no last session");

    // Draft, begin, quit: the session stays and counts.
    fs::write(
        &script,
        "begin
quit
",
    )
    .unwrap();
    let out = run_draft(&home, &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(sessions(), 1);
    let after = state();
    assert!(after.contains("last_session"), "{after}");
    assert_ne!(after, before);
}

/// Runs a headless mock session with `script`; returns the session folder and its
/// events, each validated against the schema.
fn run(tmp: &Path, script: &str) -> (PathBuf, Vec<Value>) {
    let home = tmp.join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    let init = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output();
    assert!(init.unwrap().status.success());
    let script_path = tmp.join("script.txt");
    fs::write(&script_path, script).unwrap();
    let fixture: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
    let out = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("--mock")
        .arg(&fixture)
        .args(["abcdefghijk", "--no-tui", "--script"])
        .arg(&script_path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let only_child = |dir: PathBuf| fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
    let dir = only_child(only_child(home.join("sessions")));
    let events_schema = validator("events.schema.json");
    let events: Vec<Value> = fs::read_to_string(dir.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    events.iter().for_each(|e| assert_valid(&events_schema, e));
    (dir, events)
}

#[test]
fn mute_volume_and_source_visibility_reach_obs_and_the_log() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, events) = run(
        tmp.path(),
        "+1s mute Mic/Aux\n+1s mute Mic/Aux\n+1s mute desktop audio on\n\
         +1s volume Mic/Aux -6\n+1s hide Camera\n+1s show Camera\n+1s hide Nope\nquit\n",
    );
    let mutes: Vec<(String, bool)> = events
        .iter()
        .filter(|e| e["type"] == "obs.mute")
        .map(|e| {
            let d = &e["data"];
            (
                d["input"].as_str().unwrap().to_owned(),
                d["muted"].as_bool().unwrap(),
            )
        })
        .collect();
    // Toggled twice, then a case-insensitive name with an explicit on.
    assert_eq!(
        mutes,
        [
            ("Mic/Aux".into(), true),
            ("Mic/Aux".into(), false),
            ("Desktop Audio".into(), true)
        ]
    );
    let sources: Vec<bool> = events
        .iter()
        .filter(|e| e["type"] == "obs.source")
        .map(|e| {
            assert_eq!(e["data"]["source"], "Camera");
            e["data"]["visible"].as_bool().unwrap()
        })
        .collect();
    assert_eq!(sources, [false, true]);
}

#[test]
fn record_and_stream_controls_reach_obs_and_refuse_what_makes_no_sense() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, events) = run(
        tmp.path(),
        "+1s record start
+1s record start
+1s record pause
+1s record resume
         +1s record stop
+1s record stop
+1s stream start
+1s stream stop
quit
",
    );
    let states = |ty: &str| -> Vec<String> {
        events
            .iter()
            .filter(|e| e["type"] == ty)
            .map(|e| e["data"]["state"].as_str().unwrap().to_owned())
            .collect()
    };
    // The second start and the second stop are refused.
    assert_eq!(
        states("obs.record"),
        ["started", "paused", "resumed", "stopped"]
    );
    assert_eq!(states("obs.stream"), ["started", "stopped"]);
}

#[test]
fn end_shows_the_end_screen_then_stops_streaming_and_recording() {
    let tmp = tempfile::tempdir().unwrap();
    // The first end is cancelled during its end screen; the second runs its course (seen at
    // the next command, as the script moves the clock).
    let (_, events) = run(
        tmp.path(),
        "+1s record start\n+1s stream start\n+1s end 30\n+1s end cancel\n+1s end 1\n+3s mark\n+1s quit\n",
    );
    let of = |ty: &str| -> Vec<String> {
        events
            .iter()
            .filter(|e| e["type"] == ty)
            .map(|e| {
                let d = &e["data"];
                d["to"].as_str().or(d["state"].as_str()).unwrap().to_owned()
            })
            .collect()
    };
    let scenes = of("obs.scene");
    assert_eq!(
        scenes[scenes.len() - 3..],
        ["Ending", "Code", "Ending"],
        "end screen, cancel back to main, end screen again"
    );
    assert_eq!(of("obs.stream"), ["started", "stopped"]);
    assert_eq!(of("obs.record"), ["started", "stopped"]);
}

#[test]
fn go_live_opens_on_the_starting_scene_until_moving_on() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let init = Command::new(env!("CARGO_BIN_EXE_aftercast"))
        .arg("--home")
        .arg(&home)
        .arg("init")
        .output();
    assert!(init.unwrap().status.success());
    // Go Live is checked first: its own overlay server (any free port, not the 7878 another
    // aftercast may hold), and no recording demanded before Go Live starts one.
    let config = home.join("aftercast.toml");
    let text = fs::read_to_string(&config)
        .unwrap()
        .replace("port = 7878", "port = 0")
        .replace("require_recording = true", "require_recording = false");
    fs::write(&config, text).unwrap();
    // Go Live belongs to a draft (Home before the stream).
    let script = tmp.path().join("golive.txt");
    fs::write(&script, "+1s golive\n+1s scene Code\n+1s quit\n").unwrap();
    let out = run_draft(&home, &script);
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
    let scenes: Vec<&str> = events
        .iter()
        .filter(|e| e["type"] == "obs.scene")
        .map(|e| e["data"]["to"].as_str().unwrap())
        .collect();
    assert_eq!(scenes[scenes.len() - 2..], ["Starting Soon", "Code"]);
}

#[test]
fn obs_restart_after_stop_makes_the_session_live_again() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, events) = run(
        tmp.path(),
        "+1s obs stream started\n+5s obs stream stopped\n+30s obs stream started\nquit\n",
    );
    let kinds: Vec<String> = events
        .iter()
        .filter(|e| e["type"] == "stream.ended" || e["type"] == "stream.status")
        .map(|e| format!("{} {} {}", e["type"], e["data"]["status"], e["src"]))
        .collect();
    assert_eq!(
        kinds[kinds.len() - 2..],
        [
            r#""stream.ended" null "obs""#,
            r#""stream.status" "live" "obs""#
        ]
    );
    let toml = fs::read_to_string(dir.join("session.toml")).unwrap();
    assert!(toml.contains("status = \"live\""), "{toml}");
}

#[test]
fn mock_obs_brb_scenes_recordings_and_down() {
    let tmp = tempfile::tempdir().unwrap();
    let (dir, events) = run(
        tmp.path(),
        "\
+1s obs record started D:/OBS/a.mkv
+1s obs stream started
+10s brb
+60s back
+5s obs scene BRB
+5s obs scene Code
+5s obs record paused
+10s obs record resumed
+20s obs record file_changed D:/OBS/b.mkv
+5s privacy on
+5s privacy off
+5s scene Nope
+5s obs record stopped D:/OBS/b.mkv
+1s obs down
quit
",
    );

    let of = |ty: &str| -> Vec<&Value> { events.iter().filter(|e| e["type"] == ty).collect() };
    let scenes: Vec<&str> = of("obs.scene")
        .iter()
        .map(|e| e["data"]["to"].as_str().unwrap())
        .collect();
    assert_eq!(
        scenes,
        ["Code", "BRB", "Code", "BRB", "Code", "Privacy", "Code"]
    );
    // `b`/`back` and the manual switch in OBS both produce BRB start/end pairs.
    let brb: Vec<(&str, &str)> = of("brb.start")
        .iter()
        .map(|e| {
            (
                e["src"].as_str().unwrap(),
                e["data"]["return_scene"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(brb, [("palette", "Code"), ("obs", "Code")]);
    assert_eq!(of("brb.end").len(), 2);
    assert_eq!(of("privacy.start")[0]["data"]["by"], "palette");
    assert_eq!(of("privacy.end").len(), 1);
    // A missing scene is reported, not switched.
    assert!(!scenes.contains(&"Nope"));

    let states: Vec<&str> = of("obs.record")
        .iter()
        .map(|e| e["data"]["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["started", "paused", "resumed", "file_changed", "stopped"]
    );
    let down = of("service.state");
    assert_eq!(down.last().unwrap()["data"]["state"], "down");
    assert_eq!(down.last().unwrap()["data"]["service"], "obs");

    let recordings: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("media/recordings.json")).unwrap())
            .unwrap();
    assert_valid(&validator("recordings.schema.json"), &recordings);
    let files = recordings.as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["path"], "D:/OBS/a.mkv");
    assert_eq!(files[0]["pauses"].as_array().unwrap().len(), 1);
    assert_eq!(files[0]["stop"], files[1]["start"]);
    assert_eq!(files[1]["path"], "D:/OBS/b.mkv");
    assert!(files[1]["stop"].is_string());

    let toml = fs::read_to_string(dir.join("session.toml")).unwrap();
    assert!(toml.contains("obs_record_start"), "{toml}");
    assert!(toml.contains("obs_stream_start"), "{toml}");
    assert!(toml.contains("[[anchors.recordings]]"), "{toml}");
}
