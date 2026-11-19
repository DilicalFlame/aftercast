//! Stage 5, transcribe: `derived/words.json`, `transcript.srt`, `transcript.txt`. Nothing
//! said during privacy mode is kept.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use ac_core::config::Transcriber;
use ac_core::time::hms;
use serde::{Deserialize, Serialize};

use crate::ctx::{inside, read_json, write_json};
use crate::media::round_ms;
use crate::{Ctx, Result, tool};

/// Test hook: a ready `words.json` to use instead of running a transcriber.
pub const FIXTURE_ENV: &str = "AFTERCAST_TRANSCRIBE_FIXTURE";

const HELPER: &str = include_str!("../../../python/transcribe.py");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Words {
    pub language: String,
    pub model: String,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default)]
    pub words: Vec<Word>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Word {
    pub w: String,
    pub start: f64,
    pub end: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let raw = match std::env::var_os(FIXTURE_ENV) {
        Some(path) => read_json(&PathBuf::from(path))?,
        None => match ctx.config.post.transcriber {
            Transcriber::FasterWhisper => python(ctx, "faster-whisper")?,
            Transcriber::Whisperx => python(ctx, "whisperx")?,
            Transcriber::WhisperCpp => whisper_cpp(ctx)?,
        },
    };
    let duration = ctx.master()?.duration_s;
    let words = without(raw, &ctx.privacy(duration));
    write_outputs(ctx, &words)
}

/// Writes `words.json`, `transcript.srt` and `transcript.txt`.
pub fn write_outputs(ctx: &Ctx, words: &Words) -> Result<()> {
    write_json(&ctx.derived("words.json"), words)?;
    let srt: String = words
        .segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            format!(
                "{}\n{} --> {}\n{}\n\n",
                i + 1,
                srt_time(s.start),
                srt_time(s.end),
                s.text
            )
        })
        .collect();
    let txt: String = words
        .segments
        .iter()
        .map(|s| format!("[{}] {}\n", hms(s.start), s.text))
        .collect();
    for (name, text) in [("transcript.srt", srt), ("transcript.txt", txt)] {
        let path = ctx.derived(name);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Drops words (and segments left empty) whose middle falls inside `ranges`.
pub fn without(mut words: Words, ranges: &[(f64, f64)]) -> Words {
    let mid = |a: f64, b: f64| (a + b) / 2.0;
    words.segments.retain_mut(|s| {
        if s.words.is_empty() {
            return !inside(ranges, mid(s.start, s.end));
        }
        let before = s.words.len();
        s.words.retain(|w| !inside(ranges, mid(w.start, w.end)));
        if s.words.len() != before {
            s.text = s
                .words
                .iter()
                .map(|w| w.w.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if let (Some(first), Some(last)) = (s.words.first(), s.words.last()) {
                s.start = first.start;
                s.end = last.end;
            }
        }
        !s.words.is_empty()
    });
    for s in &mut words.segments {
        s.start = round_ms(s.start);
        s.end = round_ms(s.end);
        for w in &mut s.words {
            w.start = round_ms(w.start);
            w.end = round_ms(w.end);
        }
    }
    words
}

/// faster-whisper or whisperx through the bundled Python helper.
fn python(ctx: &mut Ctx, backend: &str) -> Result<Words> {
    let script = ctx.home.join("state/transcribe.py");
    fs::write(&script, HELPER).map_err(|e| format!("{}: {e}", script.display()))?;
    let out = ctx.derived("words.raw.json");
    let p = &ctx.config.post;
    let args: Vec<OsString> = vec![
        script.into(),
        "--backend".into(),
        backend.into(),
        "--model".into(),
        p.whisper_model.clone().into(),
        "--compute".into(),
        p.whisper_compute_type.clone().into(),
        "--language".into(),
        ctx.language().into(),
        "--in".into(),
        ctx.media("audio16k.wav").into(),
        "--out".into(),
        out.clone().into(),
    ];
    let model = p.whisper_model.clone();
    ctx.note_tool("transcriber", format!("{backend} {model}"));
    ctx.set_message(format!("{backend} {model}"));
    tool::run_lines(ctx, "python", args, |line| {
        if let Some(f) = line
            .strip_prefix("progress ")
            .and_then(|f| f.trim().parse().ok())
        {
            ctx.set_progress(f);
        }
    })?;
    let words = read_json(&out)?;
    let _ = fs::remove_file(&out);
    Ok(words)
}

/// whisper.cpp's `whisper-cli` with one word per segment (`-ml 1`), regrouped into
/// sentences. `post.whisper_model` is the path of the ggml model file.
fn whisper_cpp(ctx: &mut Ctx) -> Result<Words> {
    let model = ctx.config.post.whisper_model.clone();
    if !model.ends_with(".bin") {
        return Err(format!(
            "with transcriber = \"whisper-cpp\", post.whisper_model must be the path of a ggml model (.bin), got {model:?}"
        ));
    }
    let base = ctx.derived("whisper");
    let language = ctx.language().to_owned();
    let args: Vec<OsString> = vec![
        "-m".into(),
        model.clone().into(),
        "-f".into(),
        ctx.media("audio16k.wav").into(),
        "-l".into(),
        language.clone().into(),
        "-ml".into(),
        "1".into(),
        "-ojf".into(),
        "-of".into(),
        base.clone().into(),
    ];
    ctx.note_tool("transcriber", format!("whisper-cpp {model}"));
    tool::run(ctx, "whisper-cli", args)?;
    let json_path = base.with_extension("json");
    let v: serde_json::Value = read_json(&json_path)?;
    let _ = fs::remove_file(&json_path);
    let mut words = Vec::new();
    for item in v["transcription"].as_array().into_iter().flatten() {
        let text = item["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let ms = |k: &str| item["offsets"][k].as_f64().unwrap_or(0.0) / 1000.0;
        let p = item["tokens"]
            .as_array()
            .and_then(|t| t.first())
            .and_then(|t| t["p"].as_f64());
        words.push(Word {
            w: text.to_owned(),
            start: ms("from"),
            end: ms("to"),
            p,
        });
    }
    let language = v["result"]["language"]
        .as_str()
        .unwrap_or(&language)
        .to_owned();
    Ok(Words {
        language,
        model,
        segments: sentences(words),
    })
}

/// Groups words into segments at sentence ends, pauses over 0.8 s, or 10 s.
fn sentences(words: Vec<Word>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let flush = |current: &mut Vec<Word>, out: &mut Vec<Segment>| {
        if let (Some(first), Some(last)) = (current.first(), current.last()) {
            out.push(Segment {
                start: first.start,
                end: last.end,
                text: current
                    .iter()
                    .map(|w| w.w.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                words: std::mem::take(current),
            });
        }
    };
    for w in words {
        if let (Some(first), Some(last)) = (current.first(), current.last())
            && (w.start - last.end > 0.8 || w.end - first.start > 10.0)
        {
            flush(&mut current, &mut out);
        }
        let ends = w.w.ends_with(['.', '?', '!']);
        current.push(w);
        if ends {
            flush(&mut current, &mut out);
        }
    }
    flush(&mut current, &mut out);
    out
}

fn srt_time(secs: f64) -> String {
    let ms = (secs.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(w: &str, start: f64, end: f64) -> Word {
        Word {
            w: w.into(),
            start,
            end,
            p: None,
        }
    }

    #[test]
    fn privacy_ranges_remove_words_and_empty_segments() {
        let words = Words {
            language: "en".into(),
            model: "m".into(),
            segments: vec![
                Segment {
                    start: 1.0,
                    end: 4.0,
                    text: "keep my secret".into(),
                    words: vec![
                        word("keep", 1.0, 1.5),
                        word("my", 1.6, 2.0),
                        word("secret", 3.0, 4.0),
                    ],
                },
                Segment {
                    start: 10.0,
                    end: 11.0,
                    text: "gone".into(),
                    words: vec![word("gone", 10.0, 11.0)],
                },
            ],
        };
        let out = without(words, &[(2.5, 20.0)]);
        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].text, "keep my");
        assert_eq!(out.segments[0].end, 2.0);
    }

    #[test]
    fn sentences_split_on_punctuation_and_pauses() {
        let segs = sentences(vec![
            word("Hello", 0.0, 0.4),
            word("world.", 0.5, 0.9),
            word("Next", 1.0, 1.2),
            word("after", 3.0, 3.3),
        ]);
        let texts: Vec<&str> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["Hello world.", "Next", "after"]);
    }

    #[test]
    fn srt_times() {
        assert_eq!(srt_time(3725.5), "01:02:05,500");
    }
}
