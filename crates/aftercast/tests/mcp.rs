//! Phase 7 acceptance (specs/13-mcp-server.md, specs/14-roadmap.md): `aftercast mcp` over
//! stdio against a home with two processed sessions: list_sessions, read_index pages,
//! get_frame returns a JPEG, redacted ranges come back as placeholders, write_suggestions
//! refuses a range overlapping a redaction, and search_transcript finds hits in both
//! sessions. Also `aftercast library search`. Needs ffmpeg on PATH (for a frame image).

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};

use base64::Engine;
use serde_json::{Value, json};

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
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

fn write_json(path: &Path, v: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

/// A ten-minute session from the mock, then hand-written pipeline outputs: speech at 30 s
/// and 300 s, a redacted segment 120 to 180 s (with words that must never come back), one
/// frame, a long index.md and chat.
fn session(bin: &str, home: &Path, tmp: &Path) -> PathBuf {
    let fixture: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
    let rec = tmp.join("rec.mkv");
    let script = format!(
        "obs record started {r}\n+20s chat bob hello from chat\n+580s obs record stopped {r}\n+1s quit\n",
        r = rec.display()
    );
    let mut child = Command::new(bin)
        .arg("--home")
        .arg(home)
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
    let year = fs::read_dir(home.join("sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let dir = fs::read_dir(year).unwrap().next().unwrap().unwrap().path();

    write_json(
        &dir.join("media/master.json"),
        &json!({ "path": "media/master.mp4", "duration_s": 6000.0, "fps": 30.0, "width": 1280,
                 "height": 720, "files": [] }),
    );
    let seg = |s: f64, text: &str| {
        let words: Vec<Value> = text
            .split(' ')
            .enumerate()
            .map(|(i, w)| json!({ "w": w, "start": s + i as f64 * 0.5, "end": s + i as f64 * 0.5 + 0.4, "p": 0.9 }))
            .collect();
        json!({ "start": s, "end": s + 3.0, "text": text, "words": words })
    };
    write_json(
        &dir.join("derived/words.json"),
        &json!({ "language": "en", "model": "fixture", "segments": [
            seg(30.0, "today we write the parser"),
            seg(150.0, "my secret plan"),
            seg(300.0, "the borrow checker is upset"),
        ]}),
    );
    write_json(
        &dir.join("derived/segments.json"),
        &json!({ "totals": { "speech": 60.0, "dead": 480.0, "redacted": 60.0 }, "segments": [
            { "start": 0.0, "end": 120.0, "kind": "speech" },
            { "start": 120.0, "end": 180.0, "kind": "redacted", "reason": "privacy" },
            { "start": 180.0, "end": 600.0, "kind": "dead" },
        ]}),
    );
    write_json(
        &dir.join("derived/frames.json"),
        &json!({ "frames": [ { "t": 30.0, "file": "00h00m30s.jpg", "reason": "interval", "ocr": "fn main() {}" } ] }),
    );
    fs::create_dir_all(dir.join("derived/frames")).unwrap();
    ok(Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", "testsrc2=s=1280x720", "-frames:v", "1"])
        .arg(dir.join("derived/frames/00h00m30s.jpg")));
    write_json(
        &dir.join("derived/manifest.json"),
        &json!({ "session": "x" }),
    );
    fs::write(
        dir.join("derived/activity.csv"),
        "t,words,keys,scene\n0,4,10,Code\n10,0,2,Code\n60,1,0,BRB\n",
    )
    .unwrap();
    // One block per minute, padded so the index needs more than one page.
    let mut index = String::from(
        "# IDD #001 · test\n\n## Summary stats\nspeech 1m\n\n## Chapters (as published)\n00:00:00 Start · 00:05:00 Parser\n\n## Timeline\n",
    );
    for m in 0..100 {
        let pad = "words ".repeat(60);
        index.push_str(&format!(
            "### {:02}:{:02}:00 to {:02}:{:02}:00  [speech]\n[{:02}:{:02}:05] {pad}\n",
            m / 60,
            m % 60,
            (m + 1) / 60,
            (m + 1) % 60,
            m / 60,
            m % 60
        ));
    }
    fs::write(dir.join("derived/index.md"), index).unwrap();
    dir
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Mcp {
    child: Child,
    out: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        let msg = json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params });
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(reply["id"], self.id);
        reply["result"].clone()
    }

    /// A tool's JSON result; panics on a tool error unless `expect_error`.
    fn call(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }));
        assert_ne!(r["isError"], true, "{name}: {r}");
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    fn call_error(&mut self, name: &str, args: Value) -> String {
        let r = self.request("tools/call", json!({ "name": name, "arguments": args }));
        assert_eq!(r["isError"], true, "{name}: {r}");
        r["content"][0]["text"].as_str().unwrap().to_owned()
    }
}

#[test]
fn mcp_tools_over_stdio() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("Streams");
    let bin = env!("CARGO_BIN_EXE_aftercast");
    ok(Command::new(bin).arg("--home").arg(&home).arg("init"));
    let config = home.join("aftercast.toml");
    let toml = fs::read_to_string(&config)
        .unwrap()
        .replace("port = 7878", &format!("port = {}", free_port()));
    fs::write(&config, toml).unwrap();

    // Session 1, and a copy as episode 2 with a different transcript.
    let one = session(bin, &home, tmp.path());
    let name_one = one.file_name().unwrap().to_string_lossy().into_owned();
    let name_two = name_one.replace("_ep001", "_ep002");
    let two = one.with_file_name(&name_two);
    copy_dir(&one, &two);
    let toml = fs::read_to_string(two.join("session.toml")).unwrap();
    fs::write(
        two.join("session.toml"),
        toml.replace("episode = 1", "episode = 2"),
    )
    .unwrap();
    write_json(
        &two.join("derived/words.json"),
        &json!({ "language": "en", "model": "fixture", "segments": [
            { "start": 40.0, "end": 43.0, "text": "fixing the parser again", "words": [] } ]}),
    );

    let mut child = Command::new(bin)
        .arg("--home")
        .arg(&home)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut mcp = Mcp { child, out, id: 0 };

    let init = mcp.request(
        "initialize",
        json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }),
    );
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "aftercast");
    let tools = mcp.request("tools/list", json!({}));
    assert_eq!(tools["tools"].as_array().unwrap().len(), 14);

    let sessions = mcp.call("list_sessions", json!({}));
    let names: Vec<&str> = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [name_two.as_str(), name_one.as_str()]);

    // read_index pages: the whole index comes back across pages, each block once.
    let first = mcp.call("read_index", json!({ "session": "ep001" }));
    let mut text = first["text"].as_str().unwrap().to_owned();
    assert!(text.starts_with("# IDD #001") && text.len() <= 25_000);
    let mut next = first["next"].clone();
    let mut pages = 1;
    while !next.is_null() {
        let page = mcp.call(
            "read_index",
            json!({ "session": "1", "start": next["start_hms"] }),
        );
        text.push_str(page["text"].as_str().unwrap());
        next = page["next"].clone();
        pages += 1;
    }
    assert!(pages >= 2, "{pages}");
    assert_eq!(
        text.matches("\n### ").count() + usize::from(text.starts_with("### ")),
        100
    );
    assert_eq!(text.matches("### 00:42:00").count(), 1);

    // get_frame: a JPEG scaled to 960 wide.
    let r = mcp.request(
        "tools/call",
        json!({ "name": "get_frame", "arguments": { "session": "latest", "t": "00:00:31" } }),
    );
    assert_eq!(r["content"][0]["mimeType"], "image/jpeg");
    let jpeg = base64::engine::general_purpose::STANDARD
        .decode(r["content"][0]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(&jpeg[..2], [0xFF, 0xD8]);
    let meta: Value = serde_json::from_str(r["content"][1]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["width"], 960);

    // Redacted: a placeholder, never the words.
    let t = mcp.call(
        "get_transcript",
        json!({ "session": name_one, "start": 100, "end": 200, "granularity": "word" }),
    );
    assert_eq!(
        t["items"],
        json!([{ "redacted": true, "start": 120.0, "end": 180.0,
        "start_hms": "00:02:00.000", "end_hms": "00:03:00.000" }])
    );
    let t = mcp.call(
        "get_transcript",
        json!({ "session": name_one, "start": 0, "end": 600 }),
    );
    assert!(!t.to_string().contains("secret"), "{t}");
    let frames = mcp.call(
        "list_frames",
        json!({ "session": name_one, "start": 0, "end": 600 }),
    );
    assert_eq!(frames["items"][0]["ocr"], "fn main() {}");

    let chat = mcp.call(
        "get_chat",
        json!({ "session": name_one, "start": 0, "end": 600 }),
    );
    let chat = chat.to_string();
    assert!(
        chat.contains("hello from chat") && !chat.contains("author_channel_id"),
        "{chat}"
    );
    let activity = mcp.call("get_activity", json!({ "session": name_one, "bin_s": 60 }));
    assert_eq!(activity["items"][0]["keys"], 12.0);
    assert_eq!(activity["items"][1]["scene"], "BRB");

    // Suggestions: a redacted overlap is refused and nothing is written.
    let bad = json!({ "schema_version": 1, "session": name_one, "items": [
        { "id": "001", "kind": "short", "title": "Secret", "slug": "secret",
          "ranges": [ { "start": 110.0, "end": 130.0 } ] } ]});
    let err = mcp.call_error(
        "write_suggestions",
        json!({ "session": name_one, "suggestions": bad }),
    );
    assert!(err.contains("overlaps a redacted segment"), "{err}");
    assert!(!one.join("ai/suggestions.json").exists());
    let good = json!({ "schema_version": 1, "session": name_one, "items": [
        { "id": "001", "kind": "short", "title": "The parser", "slug": "the-parser",
          "ranges": [ { "start": "00:00:29", "end": 34.0 } ] } ]});
    let err = mcp.call_error(
        "validate_suggestions",
        json!({ "session": name_one, "suggestions": good }),
    );
    assert!(
        err.contains("schema"),
        "times in suggestions are numbers: {err}"
    );
    let good = json!({ "schema_version": 1, "session": name_one, "items": [
        { "id": "001", "kind": "short", "title": "The parser", "slug": "the-parser",
          "ranges": [ { "start": 29.0, "end": 34.0 } ] } ]});
    let v = mcp.call(
        "validate_suggestions",
        json!({ "session": name_one, "suggestions": good }),
    );
    assert_eq!(v["valid"], true);
    // No word edge within 300 ms of either end, so only the padding applies.
    assert_eq!(v["items"][0]["ranges"][0]["start"], 28.75);
    assert_eq!(v["items"][0]["ranges"][0]["end"], 34.4);
    let w = mcp.call(
        "write_suggestions",
        json!({ "session": name_one, "suggestions": good, "mode": "replace" }),
    );
    assert_eq!(w["items"], 1);
    assert!(one.join("ai/suggestions.json").exists());
    let err = mcp.call_error("render", json!({ "session": name_one }));
    assert!(err.contains("confirm"), "{err}");

    // Search across both sessions.
    let hits = mcp.call("search_transcript", json!({ "query": "parser" }));
    let mut found: Vec<&str> = hits["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["session"].as_str().unwrap())
        .collect();
    found.sort();
    assert_eq!(found, [name_one.as_str(), name_two.as_str()]);
    let hits = mcp.call(
        "search_transcript",
        json!({ "query": "parser", "session": "ep002" }),
    );
    assert_eq!(hits["hits"].as_array().unwrap().len(), 1);
    assert_eq!(hits["hits"][0]["t_hms"], "00:00:40.000");
    let none = mcp.call("search_transcript", json!({ "query": "secret" }));
    assert_eq!(none["hits"], json!([]));

    drop(mcp.child.stdin.take());
    assert!(mcp.child.wait().unwrap().success());

    let out = ok(Command::new(bin).arg("--home").arg(&home).args([
        "library",
        "search",
        "borrow checker",
    ]));
    assert!(
        out.contains(&format!(
            "{name_one}  00:05:00  the borrow checker is upset"
        )),
        "{out}"
    );
    let out = ok(Command::new(bin)
        .arg("--home")
        .arg(&home)
        .args(["library", "rebuild"]));
    assert!(out.contains("indexed 2 sessions"), "{out}");
}
