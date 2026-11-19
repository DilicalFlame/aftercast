//! Stage 13, index: `derived/index.md`, the text digest a model reads first
//! (specs/12-ai-handoff-and-render.md), and `derived/manifest.json`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;

use ac_core::event::EventKind;
use ac_core::session_state::SessionState;
use ac_core::time::{Timestamp, hms};
use serde_json::{Value, json};

use crate::activity;
use crate::ctx::{read_json, write_json};
use crate::frames::{Frames, Reason};
use crate::segments::{Kind, Segments};
use crate::sources;
use crate::transcribe::Words;
use crate::{Ctx, Result};

/// Per-minute chat rate z-score above which a minute is flagged.
const SPIKE_Z: f64 = 2.5;
/// OCR snippet length in the timeline.
const OCR_SNIPPET: usize = 80;

/// One thing that happened at a master time, listed under its minute.
struct Item {
    t: f64,
    line: String,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let master = ctx.master()?;
    let duration = master.duration_s;
    let meta = ctx.session.meta.clone();
    let segs: Segments = read_json(&ctx.derived("segments.json"))?;
    let words: Option<Words> = read_json(&ctx.derived("words.json")).ok();
    let frames: Option<Frames> = read_json(&ctx.derived("frames.json")).ok();
    let bins = activity::read(ctx)?;
    let shell = sources::shell(ctx);
    let commits = sources::commits(ctx);
    let keys = sources::keys(ctx);
    let secrets: Vec<Value> = read_json(&ctx.derived("secrets.json")).unwrap_or_default();
    let mut state = SessionState::default();
    for e in &ctx.events {
        state.apply(e);
    }
    let rec = |t: Timestamp| ctx.rec(t);

    let mut md = String::new();
    let date = serde_json::to_value(meta.created_at)
        .ok()
        .and_then(|v| v.as_str().map(|s| s[..10].to_owned()))
        .unwrap_or_default();
    let _ = writeln!(
        md,
        "# {} #{:03} · {date} · {}\n",
        ctx.config.channel.short,
        meta.episode,
        span(duration)
    );
    let _ = writeln!(
        md,
        "video_id: {} · master: {} · fps {} · timeline: rec_t (master) · vod_offset: {:.2} s\n",
        meta.video_id,
        master.path,
        master.fps.round(),
        meta.anchors.vod_offset_ms as f64 / 1000.0
    );

    // Summary stats.
    let total = |k: Kind| segs.totals.get(k.name()).copied().unwrap_or(0.0);
    let word_count: usize = words
        .iter()
        .flat_map(|w| &w.segments)
        .map(|s| s.words.len())
        .sum();
    let answered: Vec<_> = state
        .questions
        .iter()
        .filter(|q| q.state == ac_core::session_state::QuestionState::Answered)
        .collect();
    let _ = writeln!(md, "## Summary stats");
    let _ = writeln!(
        md,
        "speech {} · focus {} · dead {} · brb {} · redacted {} · words {} · keys {} · commits {} · questions {}\n",
        span(total(Kind::Speech)),
        span(total(Kind::Focus)),
        span(total(Kind::Dead)),
        span(total(Kind::Brb)),
        span(total(Kind::Redacted)),
        thousands(word_count),
        thousands(keys.len()),
        commits.len(),
        state.questions.len()
    );

    // Chapters.
    let mut chapters = vec![format!(
        "00:00:00 {}",
        ctx.config.stream.first_chapter_title
    )];
    let mut cps = state.checkpoints.clone();
    cps.sort_by_key(|c| c.pressed_at);
    for c in &cps {
        if let Some(t) = rec(c.pressed_at) {
            chapters.push(format!("{} {}", hms(t), c.title));
        }
    }
    let _ = writeln!(md, "## Chapters (as published)\n{}\n", chapters.join(" · "));

    // Marks and notes.
    let mut marks: Vec<(f64, String)> = Vec::new();
    for (t, label) in &state.marks {
        if let Some(s) = rec(*t) {
            marks.push((
                s,
                label
                    .as_ref()
                    .map_or("mark".into(), |l| format!("mark {l:?}")),
            ));
        }
    }
    for (t, text) in &state.notes {
        if let Some(s) = rec(*t) {
            marks.push((s, format!("note {text:?}")));
        }
    }
    marks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let marks: Vec<String> = marks
        .iter()
        .map(|(t, l)| format!("{} {l}", hms(*t)))
        .collect();
    let _ = writeln!(
        md,
        "## Marks and notes (streamer)\n{}\n",
        or_none(&marks.join(" · "))
    );

    // Questions answered.
    let mut asked: BTreeMap<String, f64> = BTreeMap::new();
    let mut done: BTreeMap<String, f64> = BTreeMap::new();
    for e in &ctx.events {
        match &e.kind {
            EventKind::QuestionAdd { qid, .. } => {
                asked.insert(qid.clone(), ctx.rec_clamped(e.t, duration));
            }
            EventKind::QuestionAnswered { qid } => {
                done.insert(qid.clone(), ctx.rec_clamped(e.t, duration));
            }
            _ => {}
        }
    }
    let _ = writeln!(md, "## Questions answered");
    for q in &answered {
        let shown = q
            .shown_at
            .and_then(rec)
            .map(hms)
            .unwrap_or_else(|| "?".into());
        let _ = writeln!(
            md,
            "{} {}: {}  (shown {shown}, answered {})",
            asked.get(&q.qid).map_or("?".into(), |t| hms(*t)),
            q.author,
            q.text,
            done.get(&q.qid).map_or("?".into(), |t| hms(*t))
        );
    }
    if answered.is_empty() {
        let _ = writeln!(md, "(none)");
    }
    md.push('\n');

    // Commits.
    let _ = writeln!(md, "## Commits");
    for c in &commits {
        let at = rec(c.t).map_or_else(|| "--:--:--".into(), hms);
        let _ = writeln!(
            md,
            "{at} {} {} {:?}",
            c.repo,
            &c.hash[..c.hash.len().min(7)],
            c.subject
        );
    }
    if commits.is_empty() {
        let _ = writeln!(md, "(none)");
    }
    md.push('\n');

    // Timeline items by minute.
    let mut items: Vec<Item> = Vec::new();
    for s in words.iter().flat_map(|w| &w.segments) {
        items.push(Item {
            t: s.start,
            line: format!("[{}] {}", hms(s.start), s.text),
        });
    }
    for l in &shell {
        if let Some(t) = rec(l.t) {
            items.push(Item {
                t,
                line: format!("> $ {}   ({})", l.cmd, hms(t)),
            });
        }
    }
    for f in frames.iter().flat_map(|f| &f.frames) {
        let why = if f.reason == Reason::Scene {
            "scene"
        } else {
            "interval"
        };
        let ocr = f
            .ocr
            .as_deref()
            .map(|o| {
                let one: String = o.split_whitespace().collect::<Vec<_>>().join(" ");
                let cut: String = one.chars().take(OCR_SNIPPET).collect();
                format!(" · ocr: {cut:?}")
            })
            .unwrap_or_default();
        items.push(Item {
            t: f.t,
            line: format!("> frame {} ({why}){ocr}", f.file),
        });
    }
    for c in &commits {
        if let Some(t) = rec(c.t) {
            let hash = &c.hash[..c.hash.len().min(7)];
            items.push(Item {
                t,
                line: format!("> commit {} {hash} {:?}", c.repo, c.subject),
            });
        }
    }
    for c in &cps {
        if let Some(t) = rec(c.pressed_at) {
            items.push(Item {
                t,
                line: format!("> chapter {:?}", c.title),
            });
        }
    }
    for (t, label) in &state.marks {
        if let Some(t) = rec(*t) {
            let what = label.as_ref().map_or(String::new(), |l| format!(" {l:?}"));
            items.push(Item {
                t,
                line: format!("> mark{what}"),
            });
        }
    }
    for q in &state.questions {
        if let Some(t) = asked.get(&q.qid) {
            items.push(Item {
                t: *t,
                line: format!("> question {}: {}", q.author, q.text),
            });
        }
    }
    items.sort_by(|a, b| a.t.total_cmp(&b.t));

    // Chat spikes: z-score of chat messages per minute.
    let minutes = (duration / 60.0).ceil().max(1.0) as usize;
    let per_min = |f: &dyn Fn(&activity::Bin) -> u32| -> Vec<u32> {
        let mut v = vec![0; minutes];
        for b in &bins {
            if let Some(slot) = v.get_mut((b.t / 60.0) as usize) {
                *slot += f(b);
            }
        }
        v
    };
    let chat = per_min(&|b| b.chat_msgs);
    let keys_min = per_min(&|b| b.keys);
    let mean = chat.iter().sum::<u32>() as f64 / minutes as f64;
    let sd = (chat.iter().map(|c| (*c as f64 - mean).powi(2)).sum::<f64>() / minutes as f64).sqrt();

    let _ = writeln!(md, "## Timeline");
    let kind_of = |m: usize| -> Kind {
        let (a, b) = (m as f64 * 60.0, ((m + 1) as f64 * 60.0).min(duration));
        let mut best = (Kind::Dead, 0.0);
        for s in &segs.segments {
            let o = (s.end.min(b) - s.start.max(a)).max(0.0);
            if o <= 0.0 {
                continue;
            }
            // Any redacted second hides the whole minute.
            if s.kind == Kind::Redacted {
                return Kind::Redacted;
            }
            if o > best.1 {
                best = (s.kind, o);
            }
        }
        best.0
    };
    let mut m = 0;
    while m < minutes {
        let kind = kind_of(m);
        if matches!(kind, Kind::Dead | Kind::Brb | Kind::Redacted) {
            let mut end = m + 1;
            while end < minutes && kind_of(end) == kind {
                end += 1;
            }
            let to = (end as f64 * 60.0).min(duration);
            let what = if kind == Kind::Redacted {
                "[redacted]".to_owned()
            } else {
                format!("[{}] ({} min collapsed)", kind.name(), end - m)
            };
            let _ = writeln!(md, "### {} to {}  {what}", hms(m as f64 * 60.0), hms(to));
            m = end;
            continue;
        }
        let (a, b) = (m as f64 * 60.0, ((m + 1) as f64 * 60.0).min(duration));
        let scene = bins
            .iter()
            .rev()
            .find(|x| x.t >= a && x.t < b && !x.scene.is_empty())
            .map(|x| format!(" · scene {}", x.scene))
            .unwrap_or_default();
        let z = if sd > 0.0 {
            (chat[m] as f64 - mean) / sd
        } else {
            0.0
        };
        let spike = if z > SPIKE_Z {
            format!(" · ⚑ chat spike (z={z:.1})")
        } else {
            String::new()
        };
        let _ = writeln!(
            md,
            "### {} to {}  [{}]  keys {} · chat {}{scene}{spike}",
            hms(a),
            hms(b),
            kind.name(),
            keys_min[m],
            chat[m]
        );
        let mine: Vec<&Item> = items.iter().filter(|i| i.t >= a && i.t < b).collect();
        let spoken = mine.iter().any(|i| i.line.starts_with('['));
        for i in &mine {
            let _ = writeln!(md, "{}", i.line);
        }
        if !spoken && kind == Kind::Focus {
            let _ = writeln!(md, "(typing, no speech)");
        }
        m += 1;
    }
    let path = ctx.derived("index.md");
    fs::write(&path, md).map_err(|e| format!("{}: {e}", path.display()))?;

    manifest(
        ctx,
        &master,
        &segs,
        word_count,
        frames.as_ref(),
        &keys,
        &shell,
        &commits,
        &state,
        &secrets,
    )
}

#[allow(clippy::too_many_arguments)]
fn manifest(
    ctx: &Ctx,
    master: &crate::ctx::Master,
    segs: &Segments,
    words: usize,
    frames: Option<&Frames>,
    keys: &[ac_capture::keys::KeyRecord],
    shell: &[ac_capture::shell::ShellLine],
    commits: &[sources::Commit],
    state: &SessionState,
    secrets: &[Value],
) -> Result<()> {
    let candidates = [
        ("master", master.path.as_str()),
        ("master_info", "media/master.json"),
        ("audio", "media/audio16k.wav"),
        ("vod", "media/vod.mp4"),
        ("calibration", "derived/calibration.json"),
        ("words", "derived/words.json"),
        ("transcript_srt", "derived/transcript.srt"),
        ("transcript_txt", "derived/transcript.txt"),
        ("silence", "derived/silence.json"),
        ("git", "git.jsonl"),
        ("frames", "derived/frames.json"),
        ("frames_dir", "derived/frames"),
        ("activity", "derived/activity.csv"),
        ("segments", "derived/segments.json"),
        ("secrets", "derived/secrets.json"),
        ("index", "derived/index.md"),
        ("events", "events.jsonl"),
        ("chat", "chat.jsonl"),
        ("keys", "keys.jsonl.zst"),
        ("shell", "shell.jsonl"),
    ];
    let artifacts: BTreeMap<&str, String> = candidates
        .iter()
        .filter(|(_, p)| ctx.dir.join(p).exists())
        .map(|(n, p)| (*n, p.replace('\\', "/")))
        .collect();
    let stages: BTreeMap<String, crate::StageRecord> =
        read_json(&ctx.derived(".stages.json")).unwrap_or_default();
    let mut tools: BTreeMap<String, String> = BTreeMap::new();
    for r in stages.values() {
        tools.extend(r.tool_versions.clone());
    }
    let chat = sources::chat(ctx).len();
    let value = json!({
        "schema_version": 1,
        "session": ctx.session.name(),
        "episode": ctx.session.meta.episode,
        "master": master.path,
        "duration_s": master.duration_s,
        "fps": master.fps,
        "resolution": format!("{}x{}", master.width, master.height),
        "vod_offset_ms": ctx.session.meta.anchors.vod_offset_ms,
        "latency_s": ctx.session.meta.anchors.latency_s.unwrap_or(ctx.config.stream.default_latency_s),
        "counts": {
            "words": words,
            "frames": frames.map_or(0, |f| f.frames.len()),
            "segments": segs.segments.len(),
            "keys": keys.len(),
            "shell_cmds": shell.len(),
            "commits": commits.len(),
            "chat_msgs": chat,
            "questions": state.questions.len(),
            "marks": state.marks.len(),
            "chapters": state.checkpoints.len(),
            "secrets": secrets.len(),
        },
        "artifacts": artifacts,
        "tool_versions": tools,
        "generated_at": Timestamp::now(),
    });
    write_json(&ctx.derived("manifest.json"), &value)
}

/// `5h12m`, `12m`, `45s`.
fn span(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match (s / 3600, s / 60 % 60) {
        (0, 0) => format!("{s}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}

fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn or_none(s: &str) -> &str {
    if s.is_empty() { "(none)" } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_thousands() {
        assert_eq!(span(18_720.0), "5h12m");
        assert_eq!(span(720.0), "12m");
        assert_eq!(span(45.0), "45s");
        assert_eq!(thousands(31_204), "31,204");
        assert_eq!(thousands(999), "999");
    }
}
