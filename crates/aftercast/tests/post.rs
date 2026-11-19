//! Phase 5 acceptance (specs/14-roadmap.md): a 10-minute recording in two files plus a scripted
//! session (chapters, chat, a question, typing, privacy, BRB, a mark), a shell secret and a
//! commit. `aftercast post` must finish every stage with schema-valid outputs, a rerun must
//! skip everything, `--from frames` must rerun only frames onward, `index.md` must follow the
//! documented structure, and nothing from redacted ranges may appear in the outputs.
//! Needs ffmpeg and git on PATH; the transcript is a fixture (no Whisper in tests).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ac_core::time::Timestamp;
use serde_json::{Value, json};

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn rfc3339(t: Timestamp) -> String {
    serde_json::to_value(t)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

fn ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{cmd:?}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn validate(schema: &str, value: &Value) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(schema);
    let schema: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let v = jsonschema::options()
        .should_validate_formats(true)
        .build(&schema)
        .unwrap();
    let errors: Vec<String> = v.iter_errors(value).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{schema}: {errors:?}");
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

/// Stage name to outcome from `aftercast post` output.
fn outcomes(stdout: &str) -> Vec<(String, String)> {
    stdout
        .lines()
        .filter(|l| l.starts_with("  "))
        .filter_map(|l| {
            let (stage, what) = l.trim().split_once(char::is_whitespace)?;
            Some((stage.to_owned(), what.trim().to_owned()))
        })
        .collect()
}

#[test]
fn post_pipeline_on_a_ten_minute_session() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    ok(Command::new(bin).arg("--home").arg(&home).arg("init"));

    // A repository with one commit, dated inside the session later on.
    let code = tmp.path().join("code");
    let repo = code.join("todo-tui");
    fs::create_dir_all(&repo).unwrap();
    ok(Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["init", "-q"]));

    let config = home.join("aftercast.toml");
    let roots = format!("git_roots = [{:?}]", code.to_string_lossy());
    let toml = fs::read_to_string(&config).unwrap();
    let toml = toml
        .replace("port = 7878", &format!("port = {}", free_port()))
        .lines()
        .map(|l| {
            if l.starts_with("git_roots") {
                roots.clone()
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&config, toml).unwrap();

    // The live part: recording split into two files, privacy 120 to 180 s, BRB 210 to 270 s.
    let a = tmp.path().join("rec-a.mkv");
    let b = tmp.path().join("rec-b.mkv");
    let script = format!(
        "obs record started {a}\n\
         +20s checkpoint \"Project setup\"\n\
         +10s chat bob !q why rust over go?\n\
         +10s mark \"good bit\"\n\
         +80s privacy on\n\
         +60s privacy off\n\
         +30s brb\n\
         +60s back\n\
         +10s question show\n\
         +5s question answer\n\
         +15s obs record file_changed {b}\n\
         +20s checkpoint \"Parser\"\n\
         +250s type Code.exe fn main() {{}}\n\
         +28s obs record stopped {b}\n\
         +5s quit\n",
        a = a.display(),
        b = b.display()
    );
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let only_child = |dir: PathBuf| fs::read_dir(dir).unwrap().next().unwrap().unwrap().path();
    let dir = only_child(only_child(home.join("sessions")));
    let recs = read(&dir.join("media/recordings.json"));
    let recs = recs.as_array().unwrap();
    assert_eq!(recs.len(), 2);
    let ts = |v: &Value| serde_json::from_value::<Timestamp>(v.clone()).unwrap();
    let start = ts(&recs[0]["start"]);
    let lens: Vec<f64> = recs
        .iter()
        .map(|r| ts(&r["stop"]).secs_since(ts(&r["start"])))
        .collect();
    let at = |secs: f64| start.add_secs(secs);

    // The recordings, as long as the session says, with different pictures.
    for (path, len, src) in [(&a, lens[0], "testsrc2"), (&b, lens[1], "smptebars")] {
        ok(Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                &format!("{src}=s=320x180:r=10:d={len:.3}"),
                "-f",
                "lavfi",
                "-i",
                &format!("sine=f=440:d={len:.3}"),
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
            ])
            .arg(path));
    }

    // A commit at 250 s, shell commands at 60 s and 420 s (the second leaked a token).
    ok(Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(["commit", "-q", "--allow-empty", "-m", "feat: list view"])
        .env("GIT_AUTHOR_DATE", rfc3339(at(250.0)))
        .env("GIT_COMMITTER_DATE", rfc3339(at(250.0))));
    let shell = [
        json!({ "t": rfc3339(at(60.0)), "cwd": "P:\\code", "cmd": "cargo new todo-tui" }),
        json!({ "t": rfc3339(at(420.0)), "cwd": "P:\\code", "cmd": "export GH=[REDACTED]" }),
    ];
    let shell: String = shell.iter().map(|l| format!("{l}\n")).collect();
    fs::write(dir.join("shell.jsonl"), shell).unwrap();

    // Transcript: normal speech, speech inside privacy (150 s) and inside the secret's
    // range (425 s).
    let seg = |s: f64, e: f64, text: &str| {
        let words: Vec<Value> = text
            .split(' ')
            .enumerate()
            .map(|(i, w)| json!({ "w": w, "start": s + i as f64 * 0.4, "end": s + i as f64 * 0.4 + 0.3, "p": 0.9 }))
            .collect();
        json!({ "start": s, "end": e, "text": text, "words": words })
    };
    let words = json!({ "language": "en", "model": "fixture", "segments": [
        seg(5.0, 9.0, "hello everyone welcome back"),
        seg(30.0, 34.0, "today we build a todo app"),
        seg(150.0, 153.0, "this is my private stuff"),
        seg(330.0, 334.0, "now the parser part"),
        seg(425.0, 428.0, "oops that token leaked"),
    ]});
    let words_path = tmp.path().join("words.json");
    fs::write(&words_path, words.to_string()).unwrap();

    let post = |args: &[&str]| -> String {
        ok(Command::new(bin)
            .arg("--home")
            .arg(&home)
            .arg("post")
            .args(args)
            .env("AFTERCAST_TRANSCRIBE_FIXTURE", &words_path))
    };

    // First run: every stage runs (VOD, calibration and OCR are off by default).
    let first = outcomes(&post(&[]));
    for (stage, what) in &first {
        let expected = if ["vod", "calibrate", "ocr"].contains(&stage.as_str()) {
            what.starts_with("skipped")
        } else {
            what == "done"
        };
        assert!(expected, "{stage}: {what}");
    }
    assert_eq!(first.len(), 13);

    let derived = dir.join("derived");
    let master = read(&dir.join("media/master.json"));
    let duration = master["duration_s"].as_f64().unwrap();
    assert!(
        (duration - 600.0).abs() < 2.0,
        "{duration} {master} {recs:?}"
    );
    assert_eq!(master["files"].as_array().unwrap().len(), 2);

    let words = read(&derived.join("words.json"));
    let frames = read(&derived.join("frames.json"));
    let segments = read(&derived.join("segments.json"));
    let manifest = read(&derived.join("manifest.json"));
    validate("words.schema.json", &words);
    validate("frames.schema.json", &frames);
    validate("segments.schema.json", &segments);
    validate("manifest.schema.json", &manifest);

    // Redacted: privacy (about 120 to 180 s) and the shell secret (415 to 450 s).
    let redacted: Vec<(f64, f64)> = segments["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == "redacted")
        .map(|s| (s["start"].as_f64().unwrap(), s["end"].as_f64().unwrap()))
        .collect();
    let covered = |t: f64| redacted.iter().any(|(a, b)| t >= *a && t < *b);
    assert!(covered(121.0) && covered(179.0), "{redacted:?}");
    assert!(covered(416.0) && covered(449.0), "{redacted:?}");
    assert!(!covered(100.0) && !covered(300.0), "{redacted:?}");
    let reasons: Vec<&str> = segments["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["reason"].as_str())
        .collect();
    assert!(
        reasons.iter().any(|r| r.contains("privacy"))
            && reasons.iter().any(|r| r.contains("secret (shell)"))
    );
    assert!(
        segments["segments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["kind"] == "brb")
    );

    // Nothing from redacted ranges in the transcript or frames.
    let text: Vec<&str> = words["segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        text,
        [
            "hello everyone welcome back",
            "today we build a todo app",
            "now the parser part"
        ]
    );
    let transcript = fs::read_to_string(derived.join("transcript.txt")).unwrap();
    assert!(!transcript.contains("private") && !transcript.contains("token"));
    let frame_list = frames["frames"].as_array().unwrap();
    assert!(!frame_list.is_empty());
    for f in frame_list {
        let t = f["t"].as_f64().unwrap();
        assert!(!covered(t), "frame at {t} in a redacted range");
        assert!(
            derived
                .join("frames")
                .join(f["file"].as_str().unwrap())
                .exists()
        );
    }
    assert_eq!(
        fs::read_dir(derived.join("frames")).unwrap().count(),
        frame_list.len()
    );

    let secrets = read(&derived.join("secrets.json"));
    assert_eq!(secrets[0]["source"], "shell");
    assert!(
        !fs::read_to_string(derived.join("secrets.json"))
            .unwrap()
            .contains("GH=")
    );

    let commits = fs::read_to_string(dir.join("git.jsonl")).unwrap();
    assert_eq!(commits.lines().count(), 1);
    assert!(commits.contains("feat: list view"));

    let activity = fs::read_to_string(derived.join("activity.csv")).unwrap();
    assert!(activity.starts_with("t,words,speech_s,keys,keys_chars,shortcuts,shell_cmds,commits,chat_msgs,chat_unique,questions,marks,scene,brb,privacy,frame_change\n"));
    let bins = activity.lines().count() - 1;
    assert!((60..=61).contains(&bins), "{bins} bins");

    // index.md: documented sections, redacted minutes empty.
    let index = fs::read_to_string(derived.join("index.md")).unwrap();
    assert!(index.starts_with("# IDD #001 · "), "{index}");
    let mut last = 0;
    for heading in [
        "## Summary stats",
        "## Chapters (as published)",
        "## Marks and notes (streamer)",
        "## Questions answered",
        "## Commits",
        "## Timeline",
    ] {
        let at = index
            .find(heading)
            .unwrap_or_else(|| panic!("{heading} missing:\n{index}"));
        assert!(at > last, "{heading} out of order");
        last = at;
    }
    assert!(
        index.contains("00:00:00 Start · 00:00:20 Project setup"),
        "{index}"
    );
    assert!(index.contains("mark \"good bit\""));
    assert!(index.contains("bob: why rust over go?"));
    assert!(index.contains("todo-tui"));
    assert!(index.contains("[00:00:05] hello everyone welcome back"));
    assert!(index.contains("> $ cargo new todo-tui"));
    assert!(index.contains("[redacted]"));
    assert!(index.contains("[brb]"));
    for leaked in ["private", "token", "[REDACTED]"] {
        assert!(!index.contains(leaked), "{leaked} in index.md");
    }

    // Rerun: nothing to do.
    let stages = read(&derived.join(".stages.json"));
    for (stage, what) in outcomes(&post(&[])) {
        assert!(
            what == "up to date" || what.starts_with("skipped"),
            "{stage}: {what}"
        );
    }
    assert_eq!(read(&derived.join(".stages.json")), stages);

    // --from frames: earlier stages untouched, frames onward rerun.
    for (stage, what) in outcomes(&post(&["--from", "frames"])) {
        let rerun =
            ["frames", "activity", "segments", "secrets", "index"].contains(&stage.as_str());
        if rerun {
            assert_eq!(what, "done", "{stage}");
        } else {
            assert!(
                what == "up to date" || what.starts_with("skipped"),
                "{stage}: {what}"
            );
        }
    }
    let after = read(&derived.join(".stages.json"));
    assert_eq!(after["ingest"], stages["ingest"]);
    assert_eq!(after["transcribe"], stages["transcribe"]);
    assert_ne!(after["frames"]["finished"], stages["frames"]["finished"]);
}
