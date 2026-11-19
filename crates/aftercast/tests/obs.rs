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
