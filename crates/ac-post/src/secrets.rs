//! Stage 12, secrets: `derived/secrets.json` (never the secret itself), and every hit's
//! range `[t - 5 s, t + 30 s]` marked redacted in `segments.json`. Then frames and transcript
//! inside any redacted segment (privacy or secret) are removed.

use std::fs;

use ac_core::event::{CaptureSource, EventKind};
use serde::Serialize;

use crate::ctx::{inside, read_json, write_json};
use crate::frames::Frames;
use crate::media::round_ms;
use crate::segments::{self, Kind, Segments};
use crate::sources;
use crate::transcribe::{self, Words};
use crate::{Ctx, Result};

const BEFORE_S: f64 = 5.0;
const AFTER_S: f64 = 30.0;
/// Shell lines redacted while capturing contain this marker.
const MARKER: &str = "[REDACTED]";

#[derive(Debug, Serialize)]
struct Hit {
    t: f64,
    source: &'static str,
    pattern_name: String,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let duration = ctx.master()?.duration_s;
    let patterns = ctx.config.capture.secret_patterns.clone();
    let matching = |text: &str| -> Option<String> {
        if text.contains(MARKER) {
            return Some("redacted while capturing".into());
        }
        patterns
            .iter()
            .find(|p| p.0.is_match(text))
            .map(|p| p.0.as_str().to_owned())
    };
    let mut hits = Vec::new();
    let frames_path = ctx.derived("frames.json");
    let mut frames: Option<Frames> = read_json(&frames_path).ok();
    for f in frames.iter().flat_map(|f| &f.frames) {
        if let Some(name) = f.ocr.as_deref().and_then(&matching) {
            hits.push(Hit {
                t: f.t,
                source: "ocr",
                pattern_name: name,
            });
        }
    }
    for l in sources::shell(ctx) {
        let found = matching(&l.cmd).or_else(|| matching(&l.cwd));
        if let (Some(name), Some(t)) = (found, ctx.rec(l.t)) {
            hits.push(Hit {
                t,
                source: "shell",
                pattern_name: name,
            });
        }
    }
    let words_path = ctx.derived("words.json");
    let words: Option<Words> = read_json(&words_path).ok();
    for s in words.iter().flat_map(|w| &w.segments) {
        if let Some(name) = matching(&s.text) {
            hits.push(Hit {
                t: s.start,
                source: "transcript",
                pattern_name: name,
            });
        }
    }
    for e in &ctx.events {
        if let EventKind::CaptureSecretDropped { source } = &e.kind
            && let Some(t) = ctx.rec(e.t)
        {
            let source = match source {
                CaptureSource::Keys => "keys",
                CaptureSource::Shell => "shell",
            };
            hits.push(Hit {
                t,
                source,
                pattern_name: "dropped while capturing".into(),
            });
        }
    }
    hits.sort_by(|a, b| a.t.total_cmp(&b.t));
    for h in &mut hits {
        h.t = round_ms(h.t);
    }
    write_json(&ctx.derived("secrets.json"), &hits)?;

    let ranges: Vec<(f64, f64)> = hits
        .iter()
        .map(|h| ((h.t - BEFORE_S).max(0.0), (h.t + AFTER_S).min(duration)))
        .collect();
    let seg_path = ctx.derived("segments.json");
    let mut segs: Segments = read_json(&seg_path)?;
    for (h, r) in hits.iter().zip(&ranges) {
        segs = segments::redact(segs, &[*r], &format!("secret ({})", h.source));
    }
    write_json(&seg_path, &segs)?;

    // Every redacted segment (privacy rounded out to whole cells, and secrets): nothing
    // inside one stays in frames or the transcript.
    let redacted: Vec<(f64, f64)> = segs
        .segments
        .iter()
        .filter(|s| s.kind == Kind::Redacted)
        .map(|s| (s.start, s.end))
        .collect();
    if let Some(f) = &mut frames {
        let dir = ctx.derived("frames");
        f.frames.retain(|fr| {
            let keep = !inside(&redacted, fr.t);
            if !keep {
                let _ = fs::remove_file(dir.join(&fr.file));
                let _ = fs::remove_file(ctx.derived("ocr").join(fr.file.replace(".jpg", ".txt")));
            }
            keep
        });
        write_json(&frames_path, f)?;
    }
    if let Some(w) = words {
        transcribe::write_outputs(ctx, &transcribe::without(w, &redacted))?;
    }
    if hits.is_empty() {
        return Ok(());
    }
    let sources: Vec<&str> = hits.iter().map(|h| h.source).collect();
    ctx.notes.push(format!(
        "{} possible secret(s) on screen ({}): their ranges are redacted here. Check the VOD on \
         YouTube too and blur them in the Studio editor; details in derived/secrets.json.",
        hits.len(),
        sources.join(", ")
    ));
    Ok(())
}
