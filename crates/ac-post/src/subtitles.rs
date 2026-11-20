//! Subtitles from word timings: short cues a player can show (`transcript.srt`, and rendered
//! clips' `captions.srt`). Whisper's segments can last half a minute, far too long to show.

use std::fmt::Write;

/// Characters per line, and lines per cue.
pub const LINE: usize = 42;
const LINES: usize = 2;
/// Longest a cue stays on screen.
const MAX_S: f64 = 6.0;
/// A pause longer than this starts a new cue.
const GAP_S: f64 = 0.8;
/// Shortest a cue stays (extended into the silence after it, never over the next cue).
const MIN_S: f64 = 1.0;
/// A sentence end starts a new cue once the cue has this many characters.
const SENTENCE_MIN: usize = 15;

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A timed word; words of different `group`s (e.g. clip cuts) never share a cue.
pub struct Timed<'a> {
    pub group: usize,
    pub start: f64,
    pub end: f64,
    pub text: &'a str,
}

/// Groups words into cues of at most two 42-character lines and 6 s, breaking at pauses,
/// sentence ends and (in a full first line) commas.
pub fn cues(words: &[Timed]) -> Vec<Cue> {
    let mut out: Vec<Cue> = Vec::new();
    let mut group = usize::MAX;
    for w in words {
        let text = w.text.trim();
        if text.is_empty() {
            continue;
        }
        let join = match out.last() {
            Some(c) if w.group == group => {
                let chars = c.text.chars().count();
                let ends_sentence = c.text.ends_with(['.', '?', '!']) && chars >= SENTENCE_MIN;
                let ends_clause = c.text.ends_with([',', ';', ':']) && chars >= LINE;
                w.start - c.end <= GAP_S
                    && w.end - c.start <= MAX_S
                    && fits(&format!("{} {text}", c.text))
                    && !ends_sentence
                    && !ends_clause
            }
            _ => false,
        };
        if join {
            let c = out.last_mut().expect("checked");
            c.text.push(' ');
            c.text.push_str(text);
            c.end = w.end;
        } else {
            out.push(Cue {
                start: w.start,
                end: w.end,
                text: text.to_owned(),
            });
        }
        group = w.group;
    }
    // Short cues stay up a little longer, into the gap before the next one.
    for i in 0..out.len() {
        let next = out.get(i + 1).map_or(f64::INFINITY, |n| n.start);
        let c = &mut out[i];
        if c.end - c.start < MIN_S {
            c.end = (c.start + MIN_S).min(next).max(c.end);
        }
    }
    out
}

pub fn srt(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (i, c) in cues.iter().enumerate() {
        let _ = write!(
            out,
            "{}\n{} --> {}\n{}\n\n",
            i + 1,
            srt_time(c.start),
            srt_time(c.end),
            wrap(&c.text)
        );
    }
    out
}

pub fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// Wraps into at most [`LINES`] lines of at most [`LINE`] characters.
fn fits(text: &str) -> bool {
    let w = wrap(text);
    w.lines().count() <= LINES && w.lines().all(|l| l.chars().count() <= LINE)
}

/// Two balanced lines: the space nearest the middle that keeps both lines within [`LINE`].
pub fn wrap(text: &str) -> String {
    let n = text.chars().count();
    if n <= LINE {
        return text.to_owned();
    }
    let best = text
        .char_indices()
        .filter(|(_, c)| *c == ' ')
        .map(|(i, _)| (i, text[..i].chars().count()))
        .min_by_key(|&(_, before)| {
            let after = n - before - 1;
            let fits = before <= LINE && after <= LINE;
            (!fits, before.abs_diff(after))
        });
    match best {
        Some((i, _)) => format!("{}\n{}", &text[..i], &text[i + 1..]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(text: &str, start: f64, step: f64) -> Vec<(f64, f64, String)> {
        text.split(' ')
            .enumerate()
            .map(|(i, w)| {
                (
                    start + i as f64 * step,
                    start + i as f64 * step + step * 0.8,
                    w.to_owned(),
                )
            })
            .collect()
    }

    fn run(words: &[(f64, f64, String)]) -> Vec<Cue> {
        let t: Vec<Timed> = words
            .iter()
            .map(|(s, e, w)| Timed {
                group: 0,
                start: *s,
                end: *e,
                text: w,
            })
            .collect();
        cues(&t)
    }

    #[test]
    fn a_long_segment_becomes_short_cues() {
        // 40 words spoken over 16 s, one Whisper segment.
        let text = "so now I think I should end the stream now I will test it I should actually \
                    push the readme file before that also needs to be tested so I will just start \
                    episode zero once again readme";
        let cues = run(&timed(text, 100.0, 0.4));
        assert!(cues.len() >= 3, "{cues:?}");
        for c in &cues {
            assert!(c.end - c.start <= MAX_S + 1e-9, "{c:?}");
            assert!(fits(&c.text), "{c:?}");
        }
        let joined: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(joined.join(" "), text);
    }

    #[test]
    fn pauses_and_sentence_ends_split_short_cues_last_a_second() {
        let mut words = timed("Why Rust over Go for this?", 10.0, 0.3);
        words.extend(timed("Well, I guess you are asking", 11.9, 0.3));
        words.extend(timed("ok", 20.0, 0.2));
        let cues = run(&words);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Why Rust over Go for this?",
                "Well, I guess you are asking",
                "ok"
            ]
        );
        assert_eq!(cues[2].end, 21.0, "a 0.16 s word stays up for 1 s");
    }

    #[test]
    fn wrap_balances_two_lines() {
        let w = wrap("this caption is long enough that it needs two lines of text");
        let lines: Vec<&str> = w.lines().collect();
        assert_eq!(
            lines,
            [
                "this caption is long enough",
                "that it needs two lines of text"
            ]
        );
    }
}
