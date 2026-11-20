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
    let (raw, dropped) = clean(raw);
    if dropped > 0 {
        ctx.notes.push(format!(
            "transcribe: left out {dropped} things Whisper made up (repeated segments or phrases, words read from its hints)"
        ));
    }
    let words = without(raw, &ctx.privacy(duration));
    // Whisper's own words, for the correct stage (which rewrites words.json from it).
    write_json(&ctx.derived(crate::correct::RAW), &words)?;
    write_outputs(ctx, &words)
}

/// Mean word confidence below which a segment is taken as made up (Whisper reading its hint
/// list into silence scores about 0).
const MADE_UP_P: f64 = 0.15;
/// Faster than anyone speaks: a repeat Whisper squeezed into a fraction of a second.
const MAX_WORDS_PER_S: f64 = 7.0;
/// A repeated phrase inside a segment averaging less than this per word is made up.
const MIN_WORD_S: f64 = 0.1;
/// Longest repeated phrase looked for.
const MAX_REPEAT_WORDS: usize = 8;

/// Drops segments Whisper made up: the same text as the segment before, at least 4 words
/// faster than speech, or near-zero confidence. Returns the rest and how many were dropped.
fn clean(mut words: Words) -> (Words, usize) {
    let before = words.segments.len();
    let norm = |s: &str| {
        s.to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut last = String::new();
    words.segments.retain(|s| {
        let text = norm(&s.text);
        let n = s.words.len();
        let repeat = !text.is_empty() && text == last;
        let too_fast = n >= 4 && n as f64 / (s.end - s.start).max(0.01) > MAX_WORDS_PER_S;
        let ps: Vec<f64> = s.words.iter().filter_map(|w| w.p).collect();
        let unsure = !ps.is_empty() && ps.iter().sum::<f64>() / (ps.len() as f64) < MADE_UP_P;
        last = text;
        !(repeat || too_fast || unsure)
    });
    let mut dropped = before - words.segments.len();
    for s in &mut words.segments {
        let n = squeezed_repeats(&mut s.words);
        if n > 0 {
            dropped += n;
            s.text = s
                .words
                .iter()
                .map(|w| w.w.trim())
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    (words, dropped)
}

/// A phrase repeated inside a segment faster than anyone speaks ("I will test it now I will
/// test it" with the second copy squeezed into 0.3 s): removed. Real repeats ("let me, let
/// me") take normal time and stay. Returns how many phrases were removed.
fn squeezed_repeats(words: &mut Vec<Word>) -> usize {
    let key = |w: &Word| {
        w.w.trim()
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let mut removed = 0;
    let mut i = 2;
    'scan: while i < words.len() {
        for n in (2..=MAX_REPEAT_WORDS.min(words.len() - i)).rev() {
            let copy = &words[i..i + n];
            let span = copy[n - 1].end - copy[0].start;
            if span / n as f64 >= MIN_WORD_S {
                continue;
            }
            // The same words right before, or with one word in between.
            for gap in 0..=1 {
                let Some(from) = i.checked_sub(n + gap) else {
                    continue;
                };
                if (0..n).all(|k| key(&words[from + k]) == key(&copy[k])) {
                    words.drain(i..i + n);
                    removed += 1;
                    continue 'scan;
                }
            }
        }
        i += 1;
    }
    removed
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
    let script = tool::write_helpers(&ctx.home)?.join("transcribe.py");
    let out = ctx.derived("words.raw.json");
    let python = ctx.home.python(&ctx.config.paths.python);
    let models = ctx.home.models_dir();
    let p = &ctx.config.post;
    let mut args: Vec<OsString> = vec![
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
    if models.exists() {
        args.extend(["--models".into(), models.into()]);
    }
    let (prompt, hotwords) = context(ctx);
    args.extend(["--prompt".into(), prompt.into()]);
    if !hotwords.is_empty() {
        args.extend(["--hotwords".into(), hotwords.into()]);
    }
    let model = ctx.config.post.whisper_model.clone();
    ctx.note_tool("transcriber", format!("{backend} {model}"));
    ctx.set_message(format!("{backend} {model}"));
    tool::run_lines(ctx, &python.to_string_lossy(), args, |line| {
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

/// Longest prompt passed to Whisper; it keeps only about 220 tokens of it.
const PROMPT_MAX: usize = 800;

/// The transcriber's context: a prompt (series, title, `post.vocabulary` and this
/// stream's terms from `vocab.json`, chapter and quest titles) and the vocabulary as hotwords, applied to every 30 s window. Words only:
/// Whisper reads the prompt as the speech before the recording, and descriptive sentences
/// ("about the Rust language", "in English and Hindi") made it mishear more in tests.
pub fn context(ctx: &Ctx) -> (String, String) {
    use ac_core::event::EventKind;
    let mut topics: Vec<String> = Vec::new();
    for e in &ctx.events {
        let title = match &e.kind {
            EventKind::Checkpoint { title, .. } | EventKind::QuestSet { title, .. } => title,
            _ => continue,
        };
        if !topics.contains(title) {
            topics.push(title.clone());
        }
    }
    let terms = crate::vocab::all(ctx);
    let prompt = build_prompt(
        &ctx.config.channel.series,
        ctx.session.meta.title_at_start.as_deref().unwrap_or(""),
        &topics,
        &terms,
    );
    // Hotwords are repeated for every window; the strongest terms are enough.
    (
        prompt,
        terms
            .iter()
            .take(HOTWORDS)
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// Terms passed as hotwords: `post.vocabulary` first, then the stream's strongest.
const HOTWORDS: usize = 24;

fn build_prompt(series: &str, title: &str, topics: &[String], vocabulary: &[String]) -> String {
    let mut parts = vec![format!("{series}, a live coding stream.")];
    if !title.is_empty() {
        parts.push(format!("{title}."));
    }
    if !vocabulary.is_empty() {
        parts.push(format!("Terms: {}.", vocabulary.join(", ")));
    }
    if !topics.is_empty() {
        parts.push(format!("Topics: {}.", topics.join(", ")));
    }
    // Topics last: the least important part is the one cut.
    parts.join(" ").chars().take(PROMPT_MAX).collect::<String>()
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
    let (prompt, _) = context(ctx);
    let args: Vec<OsString> = vec![
        "--prompt".into(),
        prompt.into(),
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

    #[test]
    fn made_up_segments_are_dropped() {
        let seg = |start: f64, end: f64, text: &str, p: f64| Segment {
            start,
            end,
            text: text.into(),
            words: text
                .split(' ')
                .map(|w| Word {
                    w: w.into(),
                    start,
                    end,
                    p: Some(p),
                })
                .collect(),
        };
        let words = Words {
            language: "en".into(),
            model: "t".into(),
            segments: vec![
                seg(431.0, 440.0, "So now I should end the stream now", 0.9),
                seg(463.3, 463.7, "So now, I should end the stream now.", 0.8),
                seg(470.0, 470.3, "I will test it and push the readme file", 0.9),
                seg(480.0, 483.0, "What was the point?", 0.9),
                seg(488.7, 499.8, "WezTerm world mock git Rehearsal", 0.02),
                seg(500.0, 502.0, "okay bye", 0.9),
            ],
        };
        let (kept, dropped) = clean(words);
        let texts: Vec<&str> = kept.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "So now I should end the stream now",
                "What was the point?",
                "okay bye"
            ]
        );
        assert_eq!(dropped, 3);
    }

    #[test]
    fn squeezed_repeats_inside_a_segment_are_dropped() {
        // From the owner's stream: (word, start, end).
        let w = |list: &[(&str, f64, f64)]| -> Vec<Word> {
            list.iter()
                .map(|&(w, start, end)| Word {
                    w: w.into(),
                    start,
                    end,
                    p: Some(0.9),
                })
                .collect()
        };
        let mut made_up = w(&[
            ("I", 463.32, 464.14),
            ("will", 464.14, 464.3),
            ("test", 464.3, 464.62),
            ("it", 464.62, 464.88),
            ("now", 464.88, 465.36),
            ("I", 465.36, 465.58),
            ("will", 465.58, 465.68),
            ("test", 465.68, 465.68),
            ("it", 465.68, 465.68),
            ("I", 465.68, 465.68),
            ("should", 465.68, 465.74),
        ]);
        assert_eq!(squeezed_repeats(&mut made_up), 1);
        let text: Vec<&str> = made_up.iter().map(|x| x.w.as_str()).collect();
        assert_eq!(text, ["I", "will", "test", "it", "now", "I", "should"]);
        // Said twice at speaking speed: kept.
        let mut real = w(&[
            ("let", 1.0, 1.2),
            ("me", 1.2, 1.4),
            ("let", 1.5, 1.7),
            ("me", 1.7, 1.9),
            ("check", 1.9, 2.3),
        ]);
        assert_eq!(squeezed_repeats(&mut real), 0);
        assert_eq!(real.len(), 5);
    }

    #[test]
    fn prompt_has_series_title_terms_then_topics_and_is_capped() {
        let vocab = ["Rust".to_owned(), "cargo".to_owned()];
        let p = build_prompt(
            "IDD",
            "Parser day",
            &["Setup".to_owned(), "Lexer".to_owned()],
            &vocab,
        );
        assert_eq!(
            p,
            "IDD, a live coding stream. Parser day. Terms: Rust, cargo. Topics: Setup, Lexer."
        );
        let many: Vec<String> = (0..300).map(|i| format!("topic {i}")).collect();
        let p = build_prompt("IDD", "", &many, &vocab);
        assert_eq!(p.chars().count(), PROMPT_MAX);
        assert!(p.starts_with("IDD, a live coding stream. Terms: Rust, cargo. Topics: topic 0"));
    }

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
