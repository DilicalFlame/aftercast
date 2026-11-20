//! The MCP tools (specs/13-mcp-server.md). Results are JSON text, at most [`MAX_CHARS`];
//! long lists come in pages with a `next` start time. Content from redacted ranges is never
//! returned, only `{ "redacted": true, "start", "end" }` placeholders, and raw keystrokes
//! never at all.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use ac_core::Home;
use ac_core::chat::ChatMessage;
use ac_core::config::Config;
use ac_core::session::Session;
use ac_post::{Ctx, SegmentKind, Segments, Words, read_json};
use base64::Engine;
use serde_json::{Value, json};

type R<T> = Result<T, String>;

const MAX_CHARS: usize = 25_000;
const OCR_SNIPPET: usize = 200;

/// Tool definitions for `tools/list`.
pub fn list() -> Value {
    let session = json!({ "type": "string", "description": "folder name (2026-11-19_ep042), episode (42, ep042) or latest" });
    let time = |what: &str| json!({ "type": ["number", "string"], "description": format!("{what}: seconds on the master timeline or HH:MM:SS(.mmm)") });
    let tool = |name: &str, description: &str, props: Value, required: &[&str]| {
        json!({ "name": name, "description": description,
                "inputSchema": { "type": "object", "properties": props, "required": required } })
    };
    let suggestions = json!({ "type": ["object", "string"], "description": "ai/suggestions.json content (schemas/suggestions.schema.json)" });
    json!([
        tool(
            "list_sessions",
            "Sessions, newest first: name, episode, date, duration, status, title, whether suggestions exist.",
            json!({ "limit": { "type": "integer" }, "query": { "type": "string", "description": "filter by name or title" } }),
            &[]
        ),
        tool(
            "session_overview",
            "Manifest, stats, chapters, marks and notes, questions answered, commits and segment totals of a session.",
            json!({ "session": session }),
            &["session"]
        ),
        tool(
            "read_index",
            "A slice of derived/index.md (one block per minute). Pages with `next`.",
            json!({ "session": session, "start": time("from"), "end": time("to") }),
            &["session"]
        ),
        tool(
            "get_transcript",
            "Transcript segments or words in a range.",
            json!({ "session": session, "start": time("from"), "end": time("to"),
                     "granularity": { "enum": ["segment", "word"], "default": "segment" } }),
            &["session", "start", "end"]
        ),
        tool(
            "search_transcript",
            "Full-text search of transcripts, in one session or all of them.",
            json!({ "query": { "type": "string" }, "session": session, "limit": { "type": "integer", "default": 20 } }),
            &["query"]
        ),
        tool(
            "get_events",
            "Session events (chapters, marks, notes, scenes, BRB, questions, ...) in a range.",
            json!({ "session": session, "start": time("from"), "end": time("to"),
                     "types": { "type": "array", "items": { "type": "string" }, "description": "e.g. [\"mark\", \"question\"]; a prefix matches its subtypes" } }),
            &["session"]
        ),
        tool(
            "get_activity",
            "Activity per bin: words, speech, keys, shortcuts, shell commands, commits, chat, questions, marks, scene.",
            json!({ "session": session, "start": time("from"), "end": time("to"),
                     "bin_s": { "enum": [10, 60, 300], "default": 60 } }),
            &["session"]
        ),
        tool(
            "get_segments",
            "Segments (speech, focus, dead, brb, redacted) in a range.",
            json!({ "session": session, "start": time("from"), "end": time("to") }),
            &["session"]
        ),
        tool(
            "list_frames",
            "Extracted frames in a range: time, file, reason, OCR snippet.",
            json!({ "session": session, "start": time("from"), "end": time("to") }),
            &["session", "start", "end"]
        ),
        tool(
            "get_frame",
            "One frame as a JPEG image, by file name or nearest time.",
            json!({ "session": session, "file": { "type": "string" }, "t": time("time"),
                     "max_width": { "type": "integer", "default": 960 } }),
            &["session"]
        ),
        tool(
            "get_chat",
            "Chat messages in a range (author names kept, ids dropped). Times are when aftercast received them; viewers saw the stream about latency_s earlier.",
            json!({ "session": session, "start": time("from"), "end": time("to"), "limit": { "type": "integer", "default": 200 } }),
            &["session", "start", "end"]
        ),
        tool(
            "validate_suggestions",
            "Validate suggestions: errors, warnings and the snapped, padded ranges. Writes nothing.",
            json!({ "session": session, "suggestions": suggestions }),
            &["session", "suggestions"]
        ),
        tool(
            "write_suggestions",
            "Validate and write ai/suggestions.json; refuses when any item has errors. merge replaces items with the same id and keeps the rest.",
            json!({ "session": session, "suggestions": suggestions,
                     "mode": { "enum": ["replace", "merge"], "default": "merge" } }),
            &["session", "suggestions"]
        ),
        tool(
            "render",
            "Render items of ai/suggestions.json with ffmpeg (can take minutes). Needs confirm: true unless dry_run.",
            json!({ "session": session, "ids": { "type": "array", "items": { "type": "string" } },
                     "dry_run": { "type": "boolean" }, "confirm": { "type": "boolean" } }),
            &["session"]
        ),
    ])
}

/// Runs a tool; `Err` becomes an `isError` result.
pub fn call(home: &Home, config: &Path, name: &str, args: &Value) -> R<Vec<Value>> {
    let config = Config::load(config).map_err(|e| e.to_string())?;
    let result = match name {
        "list_sessions" => list_sessions(home, &config, args),
        "session_overview" => session_overview(open(home, config, args)?),
        "read_index" => read_index(open(home, config, args)?, args),
        "get_transcript" => get_transcript(open(home, config, args)?, args),
        "search_transcript" => search_transcript(home, &config, args),
        "get_events" => get_events(open(home, config, args)?, args),
        "get_activity" => get_activity(open(home, config, args)?, args),
        "get_segments" => get_segments(open(home, config, args)?, args),
        "list_frames" => list_frames(open(home, config, args)?, args),
        "get_frame" => return get_frame(open(home, config, args)?, args),
        "get_chat" => get_chat(open(home, config, args)?, args),
        "validate_suggestions" => validate(open(home, config, args)?, args),
        "write_suggestions" => write_suggestions(open(home, config, args)?, args),
        "render" => render(open(home, config, args)?, args),
        _ => Err(format!("unknown tool {name}")),
    }?;
    Ok(vec![json!({ "type": "text", "text": result.to_string() })])
}

// Sessions and times.

fn name(dir: &Path) -> String {
    dir.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A session by folder name, episode (`42`, `ep042`) or `latest`.
fn session_dir(home: &Home, config: &Config, arg: &str) -> R<PathBuf> {
    let dirs = ac_library::session_dirs(home, config);
    if arg.is_empty() || arg == "latest" {
        return dirs.last().cloned().ok_or_else(|| "no sessions yet".into());
    }
    if let Some(d) = dirs.iter().find(|d| name(d) == arg) {
        return Ok(d.clone());
    }
    if let Ok(n) = arg
        .trim_start_matches(|c: char| c.is_ascii_alphabetic())
        .parse::<u32>()
        && let Some(d) = dirs
            .iter()
            .rev()
            .find(|d| Session::open(d).is_ok_and(|s| s.meta.episode == n))
    {
        return Ok(d.clone());
    }
    Err(format!("no session {arg:?}; list_sessions shows them"))
}

/// A session's pipeline context plus what every tool needs.
struct Open {
    ctx: Ctx,
    duration: f64,
    redacted: Vec<(f64, f64)>,
}

fn open(home: &Home, config: Config, args: &Value) -> R<Open> {
    let dir = session_dir(home, &config, args["session"].as_str().unwrap_or("latest"))?;
    let mut ctx = Ctx::open(home.clone(), config, &dir)?;
    ctx.progress = false;
    let duration = ctx.master().map(|m| m.duration_s).unwrap_or(f64::INFINITY);
    let redacted = read_json::<Segments>(&ctx.derived("segments.json"))
        .map(|s| {
            s.segments
                .iter()
                .filter(|s| s.kind == SegmentKind::Redacted)
                .map(|s| (s.start, s.end))
                .collect()
        })
        .unwrap_or_default();
    Ok(Open {
        ctx,
        duration,
        redacted,
    })
}

impl Open {
    fn need(&self, rel: &str) -> R<PathBuf> {
        let path = self.ctx.derived(rel);
        if path.exists() {
            Ok(path)
        } else {
            Err(format!(
                "{} has no derived/{rel} yet; run aftercast post",
                self.ctx.session.name()
            ))
        }
    }

    /// `start`/`end` arguments, defaulting to the whole session.
    fn window(&self, args: &Value) -> R<(f64, f64)> {
        let start = time(args, "start")?.unwrap_or(0.0);
        let end = time(args, "end")?.unwrap_or(self.duration);
        if end <= start {
            return Err(format!("end ({end}) must be after start ({start})"));
        }
        Ok((start, end))
    }

    fn is_redacted(&self, t: f64) -> bool {
        self.redacted.iter().any(|(a, b)| t >= *a && t < *b)
    }

    /// Placeholders for the redacted ranges overlapping `[a, b)`.
    fn placeholders(&self, a: f64, b: f64) -> Vec<(f64, Value)> {
        self.redacted
            .iter()
            .filter(|(s, e)| *e > a && *s < b)
            .map(|&(s, e)| {
                (
                    s,
                    json!({ "redacted": true, "start": s, "end": e, "start_hms": hms(s), "end_hms": hms(e) }),
                )
            })
            .collect()
    }
}

fn time(args: &Value, key: &str) -> R<Option<f64>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(n.as_f64()),
        Value::String(s) => parse_time(s).map(Some),
        other => Err(format!("{key}: expected seconds or HH:MM:SS, got {other}")),
    }
}

/// `4925.2`, `01:22:05`, `01:22:05.200` or `22:05` to seconds.
fn parse_time(s: &str) -> R<f64> {
    let bad = || format!("bad time {s:?}: use seconds or HH:MM:SS(.mmm)");
    s.trim()
        .split(':')
        .try_fold(0.0, |acc, part| {
            part.parse::<f64>()
                .ok()
                .filter(|p| *p >= 0.0)
                .map(|p| acc * 60.0 + p)
        })
        .ok_or_else(bad)
}

fn hms(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// Timed items in order, cut at [`MAX_CHARS`] or `limit`; `next` says where to continue.
fn page(mut items: Vec<(f64, Value)>, limit: Option<usize>) -> Value {
    items.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    let mut size = 0;
    let mut next = Value::Null;
    for (i, (t, v)) in items.into_iter().enumerate() {
        let len = v.to_string().len();
        if limit.is_some_and(|l| i >= l) || (size + len > MAX_CHARS && !out.is_empty()) {
            next = json!({ "start": t, "start_hms": hms(t) });
            break;
        }
        size += len;
        out.push(v);
    }
    json!({ "items": out, "next": next })
}

fn limit(args: &Value, default: usize) -> usize {
    args["limit"]
        .as_u64()
        .map_or(default, |l| l.clamp(1, 1000) as usize)
}

// Tools.

fn list_sessions(home: &Home, config: &Config, args: &Value) -> R<Value> {
    let query = args["query"].as_str().map(str::to_lowercase);
    let mut out = Vec::new();
    for dir in ac_library::session_dirs(home, config).iter().rev() {
        let Ok(s) = Session::open(dir) else { continue };
        let title = s.meta.title_at_start.clone().unwrap_or_default();
        let name = name(dir);
        if let Some(q) = &query
            && !name.to_lowercase().contains(q)
            && !title.to_lowercase().contains(q)
        {
            continue;
        }
        let duration = read_json::<Value>(&dir.join("media/master.json"))
            .ok()
            .and_then(|m| m["duration_s"].as_f64());
        out.push(json!({
            "name": name,
            "episode": s.meta.episode,
            "date": s.meta.created_at,
            "duration_s": duration,
            "duration_hms": duration.map(hms),
            "status": s.meta.status,
            "title": title,
            "processed": dir.join("derived/index.md").exists(),
            "has_suggestions": dir.join("ai/suggestions.json").exists(),
        }));
        if out.len() >= limit(args, 50) {
            break;
        }
    }
    Ok(json!({ "sessions": out }))
}

fn session_overview(o: Open) -> R<Value> {
    let index = fs::read_to_string(o.need("index.md")?).map_err(|e| e.to_string())?;
    let head = index.split("\n## Timeline").next().unwrap_or_default();
    let head: String = head.chars().take(MAX_CHARS / 2).collect();
    let totals = read_json::<Segments>(&o.ctx.derived("segments.json"))
        .map(|s| json!(s.totals))
        .unwrap_or(Value::Null);
    Ok(json!({
        "session": o.ctx.session.name(),
        "manifest": read_json::<Value>(&o.ctx.derived("manifest.json")).unwrap_or(Value::Null),
        "segment_totals_s": totals,
        "overview": head,
    }))
}

fn read_index(o: Open, args: &Value) -> R<Value> {
    let index = fs::read_to_string(o.need("index.md")?).map_err(|e| e.to_string())?;
    let (head, body) = index.split_once("\n## Timeline\n").unwrap_or((&index, ""));
    let (start, end) = o.window(args)?;
    let mut items = Vec::new();
    if args["start"].is_null() {
        items.push((-1.0, Value::String(format!("{head}\n## Timeline\n"))));
    }
    let mut block: Option<(f64, String)> = None;
    let mut blocks = Vec::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("### ") {
            blocks.extend(block.take());
            let t = rest
                .split_whitespace()
                .next()
                .and_then(|t| parse_time(t).ok());
            block = Some((t.unwrap_or(f64::NAN), String::new()));
        }
        if let Some((_, text)) = &mut block {
            text.push_str(line);
            text.push('\n');
        }
    }
    blocks.extend(block);
    items.extend(
        blocks
            .into_iter()
            .filter(|(t, _)| *t >= start && *t < end)
            .map(|(t, text)| (t, Value::String(text))),
    );
    let mut paged = page(items, None);
    let text: String = paged["items"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    paged["items"] = Value::Null;
    Ok(json!({ "text": text, "next": paged["next"] }))
}

fn get_transcript(o: Open, args: &Value) -> R<Value> {
    let words: Words = read_json(&o.need("words.json")?)?;
    let (a, b) = o.window(args)?;
    let by_word = args["granularity"].as_str() == Some("word");
    let mut items = o.placeholders(a, b);
    for s in words
        .segments
        .iter()
        .filter(|s| s.end > a && s.start < b && !o.is_redacted(s.start))
    {
        if by_word {
            for w in s
                .words
                .iter()
                .filter(|w| w.end > a && w.start < b && !o.is_redacted(w.start))
            {
                items.push((w.start, json!({ "w": w.w.trim(), "start": w.start, "end": w.end, "p": w.p, "start_hms": hms(w.start) })));
            }
        } else {
            items.push((s.start, json!({ "start": s.start, "end": s.end, "start_hms": hms(s.start), "text": s.text })));
        }
    }
    Ok(page(items, None))
}

fn search_transcript(home: &Home, config: &Config, args: &Value) -> R<Value> {
    let query = args["query"].as_str().ok_or("query is required")?;
    let session = match args["session"].as_str() {
        Some(s) => Some(name(&session_dir(home, config, s)?)),
        None => None,
    };
    let mut lib = ac_library::Library::open(home)?;
    lib.sync(home, config)?;
    let hits = lib.search(query, session.as_deref(), limit(args, 20))?;
    let hits: Vec<Value> = hits
        .into_iter()
        .map(|h| {
            json!({ "session": h.session, "t": h.start, "t_hms": hms(h.start), "end": h.end,
                    "text": h.text, "context": h.context })
        })
        .collect();
    Ok(json!({ "hits": hits }))
}

fn get_events(o: Open, args: &Value) -> R<Value> {
    let (a, b) = o.window(args)?;
    let types: Vec<&str> = args["types"]
        .as_array()
        .map(|t| t.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut items = o.placeholders(a, b);
    for e in &o.ctx.events {
        let t = o.ctx.rec_clamped(e.t, o.duration);
        if t < a || t >= b || o.is_redacted(t) {
            continue;
        }
        let kind = serde_json::to_value(&e.kind).map_err(|e| e.to_string())?;
        let ty = kind["type"].as_str().unwrap_or_default();
        let wanted = types.is_empty()
            || types
                .iter()
                .any(|w| ty == *w || ty.starts_with(&format!("{w}.")));
        if wanted {
            items.push((
                t,
                json!({ "t": t, "t_hms": hms(t), "type": ty, "data": kind["data"] }),
            ));
        }
    }
    Ok(page(items, None))
}

fn get_activity(o: Open, args: &Value) -> R<Value> {
    let csv = fs::read_to_string(o.need("activity.csv")?).map_err(|e| e.to_string())?;
    let (a, b) = o.window(args)?;
    let bin = match args["bin_s"].as_u64() {
        None => 60.0,
        Some(s @ (10 | 60 | 300)) => s as f64,
        Some(s) => return Err(format!("bin_s must be 10, 60 or 300, got {s}")),
    };
    let mut lines = csv.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();
    let mut bins: Vec<(f64, serde_json::Map<String, Value>)> = Vec::new();
    for line in lines {
        let cells: Vec<&str> = line.split(',').collect();
        let Some(t) = cells.first().and_then(|t| t.parse::<f64>().ok()) else {
            continue;
        };
        if t < a || t >= b {
            continue;
        }
        let key = (t / bin).floor() * bin;
        if bins.last().is_none_or(|(k, _)| *k != key) {
            let mut row = serde_json::Map::new();
            row.insert("t".into(), json!(key));
            row.insert("t_hms".into(), json!(hms(key)));
            bins.push((key, row));
        }
        let row = &mut bins.last_mut().expect("pushed").1;
        for (name, cell) in header.iter().zip(&cells).skip(1) {
            match cell.parse::<f64>() {
                Ok(n) => {
                    let sum = row.get(*name).and_then(Value::as_f64).unwrap_or(0.0) + n;
                    row.insert((*name).into(), json!((sum * 1000.0).round() / 1000.0));
                }
                Err(_) if !cell.is_empty() => {
                    row.insert((*name).into(), json!(cell));
                }
                Err(_) => {}
            }
        }
    }
    Ok(page(
        bins.into_iter()
            .map(|(t, r)| (t, Value::Object(r)))
            .collect(),
        None,
    ))
}

fn get_segments(o: Open, args: &Value) -> R<Value> {
    let segments: Segments = read_json(&o.need("segments.json")?)?;
    let (a, b) = o.window(args)?;
    let items = segments
        .segments
        .iter()
        .filter(|s| s.end > a && s.start < b)
        .map(|s| {
            let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
            v["start_hms"] = json!(hms(s.start));
            v["end_hms"] = json!(hms(s.end));
            (s.start, v)
        })
        .collect();
    Ok(page(items, None))
}

/// `frames.json` entries (already pruned of redacted ranges by the pipeline).
fn frames(o: &Open) -> R<Vec<Value>> {
    let frames: Value = read_json(&o.need("frames.json")?)?;
    Ok(frames["frames"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| !f["t"].as_f64().is_some_and(|t| o.is_redacted(t)))
        .collect())
}

fn list_frames(o: Open, args: &Value) -> R<Value> {
    let (a, b) = o.window(args)?;
    let mut items = o.placeholders(a, b);
    for mut f in frames(&o)? {
        let t = f["t"].as_f64().unwrap_or(-1.0);
        if t < a || t >= b {
            continue;
        }
        if let Some(ocr) = f["ocr"].as_str() {
            f["ocr"] = json!(ocr.chars().take(OCR_SNIPPET).collect::<String>());
        }
        f["t_hms"] = json!(hms(t));
        items.push((t, f));
    }
    Ok(page(items, None))
}

fn get_frame(o: Open, args: &Value) -> R<Vec<Value>> {
    let frames = frames(&o)?;
    let frame = if let Some(file) = args["file"].as_str() {
        frames.into_iter().find(|f| f["file"] == file)
    } else {
        let t = time(args, "t")?.ok_or("give file or t")?;
        frames.into_iter().min_by(|x, y| {
            let d = |f: &Value| (f["t"].as_f64().unwrap_or(f64::MAX) - t).abs();
            d(x).total_cmp(&d(y))
        })
    }
    .ok_or("no such frame (list_frames shows them)")?;
    let file = frame["file"].as_str().unwrap_or_default();
    let path = o.ctx.derived("frames").join(file);
    let img = image::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let max = args["max_width"].as_u64().unwrap_or(960).clamp(64, 4096) as u32;
    let img = if img.width() > max {
        img.resize(max, u32::MAX, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), 85)
        .encode_image(&img.to_rgb8())
        .map_err(|e| e.to_string())?;
    let t = frame["t"].as_f64().unwrap_or_default();
    let meta = json!({ "file": file, "t": t, "t_hms": hms(t), "reason": frame["reason"],
                       "width": img.width(), "height": img.height() });
    Ok(vec![
        json!({ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(&jpeg), "mimeType": "image/jpeg" }),
        json!({ "type": "text", "text": meta.to_string() }),
    ])
}

fn get_chat(o: Open, args: &Value) -> R<Value> {
    let (a, b) = o.window(args)?;
    let text = fs::read_to_string(o.ctx.dir.join("chat.jsonl")).unwrap_or_default();
    let mut items = o.placeholders(a, b);
    for m in text
        .lines()
        .filter_map(|l| serde_json::from_str::<ChatMessage>(l).ok())
    {
        let t = o.ctx.rec_clamped(m.t_received, o.duration);
        if t < a || t >= b || o.is_redacted(t) {
            continue;
        }
        items.push((
            t,
            json!({ "t": t, "t_hms": hms(t), "author": m.author_name, "text": m.text, "kind": m.kind,
                    "owner": m.is_owner, "moderator": m.is_moderator, "member": m.is_member, "amount": m.amount }),
        ));
    }
    Ok(page(items, Some(limit(args, 200))))
}

fn suggestions_arg(args: &Value) -> R<ac_render::Suggestions> {
    let v = match &args["suggestions"] {
        Value::String(s) => serde_json::from_str(s).map_err(|e| format!("suggestions: {e}"))?,
        Value::Null => return Err("suggestions is required".into()),
        v => v.clone(),
    };
    let s: ac_render::Suggestions = serde_json::from_value(v)
        .map_err(|e| format!("suggestions do not match the schema: {e}"))?;
    if s.schema_version != 1 {
        return Err(format!(
            "schema_version must be 1, got {}",
            s.schema_version
        ));
    }
    Ok(s)
}

/// Validation report; `true` when no item has errors.
fn check(o: &Open, s: &ac_render::Suggestions) -> R<(bool, Value)> {
    let tl = ac_render::Timeline::load(&o.ctx)?;
    let checked = ac_render::check(s, &o.ctx.session.name(), &tl, &o.ctx.config.render);
    let valid = checked.iter().all(|c| c.errors.is_empty());
    let items: Vec<Value> = checked
        .iter()
        .map(|c| {
            let ranges: Vec<Value> = c
                .cuts
                .iter()
                .map(|r| {
                    json!({ "start": r.start, "end": r.end, "speed": r.speed, "suggested": r.suggested,
                            "start_hms": hms(r.start), "end_hms": hms(r.end) })
                })
                .collect();
            json!({ "id": c.item.id, "kind": c.item.kind, "title": c.item.title,
                    "duration_s": (c.out_len() * 1000.0).round() / 1000.0,
                    "errors": c.errors, "warnings": c.warnings, "ranges": ranges })
        })
        .collect();
    Ok((valid, json!({ "valid": valid, "items": items })))
}

fn validate(o: Open, args: &Value) -> R<Value> {
    let s = suggestions_arg(args)?;
    Ok(check(&o, &s)?.1)
}

fn write_suggestions(o: Open, args: &Value) -> R<Value> {
    let mut s = suggestions_arg(args)?;
    let path = o.ctx.dir.join("ai/suggestions.json");
    match args["mode"].as_str().unwrap_or("merge") {
        "replace" => {}
        "merge" => {
            if let Ok(old) = read_json::<ac_render::Suggestions>(&path) {
                let mut items = old.items;
                for item in s.items {
                    match items.iter_mut().find(|i| i.id == item.id) {
                        Some(slot) => *slot = item,
                        None => items.push(item),
                    }
                }
                s.items = items;
            }
        }
        other => return Err(format!("mode must be replace or merge, got {other:?}")),
    }
    let (valid, report) = check(&o, &s)?;
    if !valid {
        return Err(format!(
            "not written: fix the errors first\n{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        ));
    }
    ac_post::write_json(&path, &s)?;
    if let Ok(mut lib) = ac_library::Library::open(&o.ctx.home) {
        let _ = lib.upsert(&o.ctx.dir);
    }
    Ok(json!({ "written": path.to_string_lossy(), "items": s.items.len(), "report": report }))
}

fn render(mut o: Open, args: &Value) -> R<Value> {
    let dry_run = args["dry_run"].as_bool().unwrap_or(false);
    if !dry_run && args["confirm"].as_bool() != Some(true) {
        return Err("rendering writes video files and takes a while: call again with confirm: true (or dry_run: true)".into());
    }
    let ids: Vec<String> = args["ids"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| i.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let render_cfg = &o.ctx.config.render;
    let opts = ac_render::Options {
        ids,
        dry_run,
        force: false,
        keys: render_cfg.keys_overlay,
        captions: render_cfg.captions,
    };
    // ponytail: no progress notifications; the call returns when every item is done.
    let report = ac_render::run(&mut o.ctx, &opts)?;
    let items: Vec<Value> = report
        .iter()
        .map(|(c, outcome)| {
            let (status, detail) = match outcome {
                ac_render::Outcome::Valid => ("valid", Value::Null),
                ac_render::Outcome::Rendered { duration_s } => ("rendered", json!(duration_s)),
                ac_render::Outcome::UpToDate => ("up to date", Value::Null),
                ac_render::Outcome::Failed(why) => ("failed", json!(why)),
            };
            json!({ "id": c.item.id, "kind": c.item.kind, "title": c.item.title, "status": status,
                    "detail": detail, "warnings": c.warnings,
                    "out_dir": ac_render::out_dir(&o.ctx.dir, &c.item).to_string_lossy() })
        })
        .collect();
    if !dry_run && let Ok(mut lib) = ac_library::Library::open(&o.ctx.home) {
        let _ = lib.upsert(&o.ctx.dir);
    }
    Ok(json!({ "items": items }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_parse_both_ways() {
        assert_eq!(parse_time("4925.2").unwrap(), 4925.2);
        assert_eq!(parse_time("01:22:05").unwrap(), 4925.0);
        assert_eq!(parse_time("22:05.5").unwrap(), 1325.5);
        assert!(parse_time("1:x").is_err());
        assert_eq!(hms(4925.2), "01:22:05.200");
    }

    #[test]
    fn pages_stop_at_the_limit_with_a_next_cursor() {
        let items = (0..5).map(|i| (i as f64, json!(i))).collect();
        let p = page(items, Some(2));
        assert_eq!(p["items"], json!([0, 1]));
        assert_eq!(p["next"]["start"], 2.0);
        let big = "x".repeat(MAX_CHARS - 1);
        let p = page(vec![(0.0, json!(big)), (1.0, json!("y"))], None);
        assert_eq!(p["items"].as_array().unwrap().len(), 1);
        assert_eq!(p["next"]["start_hms"], "00:00:01.000");
    }
}
