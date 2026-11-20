//! Stage `correct` (optional, `post.correct`): words that sound like one of the stream's
//! terms ("rest" for "Rust") go to a small local model as a choice between what Whisper
//! wrote and the look-alike term; the sentence decides. Nothing else can change: no other
//! words, no rewording, word timings stay. Every change is listed in
//! `derived/corrections.json`. Reads `words.whisper.json`, writes `words.json`.

use std::ffi::OsString;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::ctx::{read_json, write_json};
use crate::transcribe::{self, Words};
use crate::{Ctx, Result, tool, vocab};

/// Whisper's own output, before corrections (and the input of this stage).
pub const RAW: &str = "words.whisper.json";
/// Look-alike terms offered per word.
const MAX_OPTIONS: usize = 3;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correction {
    /// Master seconds.
    pub t: f64,
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p: Option<f64>,
    /// The sentence as Whisper wrote it.
    pub line: String,
}

/// One word to decide: keep it (option A) or one of the look-alike terms (B, C, ...).
#[derive(Debug, Serialize)]
struct Question {
    q: usize,
    /// The sentence before, for context.
    before: String,
    /// The sentence with the word in [brackets].
    text: String,
    word: String,
    options: Vec<String>,
    #[serde(skip)]
    seg: usize,
    #[serde(skip)]
    index: usize,
}

#[derive(Debug, Deserialize)]
struct Answer {
    q: usize,
    /// `A` keeps the word; `B`, `C`, ... pick `options[0]`, `options[1]`, ...
    choice: String,
}

#[derive(Deserialize)]
struct Answers {
    answers: Vec<Answer>,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let mut words: Words = read_json(&ctx.derived(RAW))
        .map_err(|e| format!("{e} (run the transcribe stage first)"))?;
    let terms = vocab::all(ctx);
    // Names read out from chat ("Bob asks...") are never corrected.
    let names: Vec<String> = crate::sources::chat(ctx)
        .into_iter()
        .flat_map(|m| {
            m.author_name
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    let questions = questions(&words, &terms, &names, ctx.config.post.correct_below);
    let corrections = if questions.is_empty() {
        Vec::new()
    } else {
        ctx.set_message(format!("{} look-alike words", questions.len()));
        let answers = ask(ctx, &questions)?;
        apply(&mut words, &questions, &answers)
    };
    write_json(&ctx.derived("corrections.json"), &corrections)?;
    transcribe::write_outputs(ctx, &words)
}

/// The word without surrounding punctuation.
fn core(w: &str) -> &str {
    w.trim().trim_matches(|c: char| !c.is_alphanumeric())
}

/// Levenshtein distance.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// First letter and the consonants after it, repeats collapsed: "rest" and "Rust" are both
/// "rst". Speech recognisers mostly confuse vowels.
fn skeleton(w: &str) -> String {
    let mut out = String::new();
    for (i, c) in w.chars().enumerate() {
        if (i == 0 || !"aeiouyhw".contains(c)) && !out.ends_with(c) {
            out.push(c);
        }
    }
    out
}

/// Sounds or is spelled almost like `term`, but is not the same word: same skeleton
/// ("rest"/"Rust"), the term's skeleton plus one trailing consonant ("roster" for "a Rust"),
/// or a small spelling difference.
fn sounds_like(word: &str, term: &str) -> bool {
    let (w, t) = (word.to_lowercase(), term.to_lowercase());
    w.chars().count() >= 3
        && t.chars().count() >= 3
        && !t.contains(' ')
        && w != t
        && (skeleton(&w) == skeleton(&t)
            || (skeleton(&w).starts_with(&skeleton(&t))
                && skeleton(&w).chars().count() == skeleton(&t).chars().count() + 1)
            || distance(&w, &t) <= (t.chars().count() / 4).max(1))
}

/// Words that sound like a term and that Whisper was less sure of than `below`. Words that
/// already are a term, chat names, and non-ASCII words (Hindi) are never asked about.
fn questions(words: &Words, terms: &[String], names: &[String], below: f64) -> Vec<Question> {
    let segs = &words.segments;
    let mut out = Vec::new();
    for (si, s) in segs.iter().enumerate() {
        for (wi, w) in s.words.iter().enumerate() {
            let bare = core(&w.w);
            if !bare.is_ascii()
                || w.p.unwrap_or(1.0) >= below
                || terms.iter().any(|t| t.eq_ignore_ascii_case(bare))
                || names.iter().any(|n| n.eq_ignore_ascii_case(bare))
            {
                continue;
            }
            let options: Vec<String> = terms
                .iter()
                .filter(|t| sounds_like(bare, t))
                .take(MAX_OPTIONS)
                .cloned()
                .collect();
            if options.is_empty() {
                continue;
            }
            let text = s
                .words
                .iter()
                .enumerate()
                .map(|(i, x)| {
                    if i == wi {
                        format!("[{}]", x.w.trim())
                    } else {
                        x.w.trim().to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            out.push(Question {
                q: out.len() + 1,
                before: si
                    .checked_sub(1)
                    .and_then(|i| segs.get(i))
                    .map_or(String::new(), |p| p.text.clone()),
                text,
                word: bare.to_owned(),
                options,
                seg: si,
                index: wi,
            });
        }
    }
    out
}

/// Runs the model through `correct.py`.
fn ask(ctx: &mut Ctx, questions: &[Question]) -> Result<Vec<Answer>> {
    let script = tool::write_helpers(&ctx.home)?.join("correct.py");
    let tasks = ctx.derived("correct.tasks.json");
    let out = ctx.derived("correct.out.json");
    let context = transcribe::context(ctx);
    write_json(
        &tasks,
        &serde_json::json!({ "context": context, "questions": questions }),
    )?;
    let python = ctx.home.python(&ctx.config.paths.python);
    let model = ctx.config.post.correct_model.clone();
    let args: Vec<OsString> = vec![
        script.into(),
        "--model".into(),
        model.clone().into(),
        "--models".into(),
        ctx.home.models_dir().into(),
        "--tasks".into(),
        tasks.clone().into(),
        "--out".into(),
        out.clone().into(),
    ];
    ctx.note_tool("corrector", model);
    tool::run_lines(ctx, &python.to_string_lossy(), args, |line| {
        if let Some(f) = line
            .strip_prefix("progress ")
            .and_then(|f| f.trim().parse().ok())
        {
            ctx.set_progress(f);
        }
    })?;
    let answers: Answers = read_json(&out)?;
    let _ = fs::remove_file(&tasks);
    let _ = fs::remove_file(&out);
    Ok(answers.answers)
}

/// Applies the chosen terms, keeping each word's punctuation and timing.
fn apply(words: &mut Words, questions: &[Question], answers: &[Answer]) -> Vec<Correction> {
    let mut out = Vec::new();
    for a in answers {
        let Some(q) = questions.iter().find(|q| q.q == a.q) else {
            continue;
        };
        let pick = match a.choice.trim().as_bytes() {
            [c @ b'B'..=b'Z'] => q.options.get(usize::from(c - b'B')),
            _ => None, // A keeps the word
        };
        let Some(term) = pick else { continue };
        let seg = &mut words.segments[q.seg];
        let line = seg.text.clone();
        let w = &mut seg.words[q.index];
        let trimmed = w.w.trim();
        let Some(at) = trimmed.find(&q.word) else {
            continue;
        };
        let fixed = format!("{}{term}{}", &trimmed[..at], &trimmed[at + q.word.len()..]);
        out.push(Correction {
            t: w.start,
            from: trimmed.to_owned(),
            to: fixed.clone(),
            p: w.p,
            line,
        });
        w.w = fixed;
        seg.text = seg
            .words
            .iter()
            .map(|w| w.w.trim())
            .collect::<Vec<_>>()
            .join(" ");
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::{Segment, Word};

    fn words(text: &str, ps: &[f64]) -> Words {
        let ws: Vec<Word> = text
            .split(' ')
            .zip(ps)
            .enumerate()
            .map(|(i, (w, p))| Word {
                w: w.into(),
                start: i as f64,
                end: i as f64 + 0.5,
                p: Some(*p),
            })
            .collect();
        Words {
            language: "en".into(),
            model: "test".into(),
            segments: vec![Segment {
                start: 0.0,
                end: 9.0,
                text: text.into(),
                words: ws,
            }],
        }
    }

    #[test]
    fn asks_only_about_unsure_sound_alikes() {
        let w = words(
            "committing get the rest of trust, Rust ठीक garage edit bob",
            &[0.2, 0.5, 0.9, 0.88, 0.9, 0.5, 0.3, 0.2, 0.3, 0.4, 0.5],
        );
        let terms = vec![
            "Rust".to_owned(),
            "git".to_owned(),
            "repo".to_owned(),
            "brb".into(),
        ];
        let qs = questions(&w, &terms, &["Bob".to_owned()], 0.95);
        let asked: Vec<(&str, Vec<&str>)> = qs
            .iter()
            .map(|q| {
                (
                    q.word.as_str(),
                    q.options.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            asked,
            [
                ("get", vec!["git"]),
                ("rest", vec!["Rust"]),
                ("trust", vec!["Rust"])
            ]
        );
        assert_eq!(
            qs[1].text,
            "committing get the [rest] of trust, Rust ठीक garage edit bob"
        );
        assert_eq!(skeleton("rest"), skeleton("rust"));
        assert!(sounds_like("roster", "Rust") && !sounds_like("restaurant", "Rust"));
        assert!(!sounds_like("committing", "git") && !sounds_like("edit", "repo"));
    }

    #[test]
    fn applies_choices_keeping_punctuation_and_timing() {
        let mut w = words(
            "choosing rest, because rest. ok",
            &[0.9, 0.3, 0.9, 0.5, 0.9],
        );
        let terms = vec!["Rust".to_owned()];
        let qs = questions(&w, &terms, &[], 0.95);
        let answers = [
            Answer {
                q: 1,
                choice: "B".into(),
            },
            Answer {
                q: 2,
                choice: "A".into(),
            },
            Answer {
                q: 7,
                choice: "B".into(),
            },
        ];
        let fixes = apply(&mut w, &qs, &answers);
        assert_eq!(fixes.len(), 1);
        assert_eq!(
            (fixes[0].from.as_str(), fixes[0].to.as_str()),
            ("rest,", "Rust,")
        );
        assert_eq!(w.segments[0].text, "choosing Rust, because rest. ok");
        assert_eq!(w.segments[0].words[1].start, 1.0);
    }
}
