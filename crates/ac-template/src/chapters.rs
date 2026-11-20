//! YouTube chapters (specs/08-readme-and-description.md, "Chapters").

use ac_core::config;
use ac_core::session_state::{QuestionState, SessionState};
use ac_core::time::{Timestamp, chapter_ts};
use serde::Serialize;

/// Title of a BRB chapter (`stream.chapter_on_brb`).
pub const BREAK: &str = "Break";
/// Minimum distance between chapters, and the window for the first one.
const MIN_GAP_S: f64 = 10.0;
const MAX_TITLE_CHARS: usize = 100;
/// YouTube shows chapters only when there are at least this many.
const MIN_CHAPTERS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChapterKind {
    Start,
    Checkpoint,
    Quest,
    Break,
    Question,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Chapter {
    pub ts: String,
    pub title: String,
    pub vod_t: f64,
    pub kind: ChapterKind,
}

/// A chapter source event: wall time, title, kind.
pub type Candidate = (Timestamp, String, ChapterKind);

/// Chapter candidates from session state, honoring the `[stream]` chapter options.
pub fn candidates(state: &SessionState, stream: &config::Stream) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = state
        .checkpoints
        .iter()
        .map(|c| (c.pressed_at, c.title.clone(), ChapterKind::Checkpoint))
        .collect();
    if stream.chapter_on_quest_change {
        out.extend(
            state
                .quests
                .iter()
                .map(|q| (q.t, q.title.clone(), ChapterKind::Quest)),
        );
    }
    if stream.chapter_on_brb {
        // After a break the stream is back in the chapter it was in (or one started during
        // the break), so viewers can skip exactly the break.
        let resume: Vec<Candidate> = state
            .brb_ends
            .iter()
            .map(|&end| {
                let title = out
                    .iter()
                    .filter(|c| c.0 <= end)
                    .max_by_key(|c| c.0)
                    .map_or_else(|| stream.first_chapter_title.clone(), |c| c.1.clone());
                (end, title, ChapterKind::Checkpoint)
            })
            .collect();
        out.extend(
            state
                .brb_starts
                .iter()
                .map(|&t| (t, BREAK.to_owned(), ChapterKind::Break)),
        );
        out.extend(resume);
    }
    if stream.chapter_on_question {
        out.extend(state.questions.iter().filter_map(|q| {
            // Super Chats are thanked on stream, not chapters.
            if q.support.is_some() {
                return None;
            }
            let shown = q.shown_at.filter(|_| q.state != QuestionState::Dismissed)?;
            Some((shown, format!("Q: {}", q.text), ChapterKind::Question))
        }));
    }
    out
}

/// Builds chapters from `(vod_t, title, kind)` entries for a VOD of `vod_len` seconds.
pub fn build(
    entries: Vec<(f64, String, ChapterKind)>,
    vod_len: f64,
    first_title: &str,
) -> Vec<Chapter> {
    let mut entries: Vec<_> = entries
        .into_iter()
        .filter(|(t, ..)| (0.0..=vod_len).contains(t))
        .collect();
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));

    // Closer than 10 s to the previous one: the later title wins.
    let mut kept: Vec<(f64, String, ChapterKind)> = Vec::with_capacity(entries.len());
    for entry in entries {
        if kept.last().is_some_and(|last| entry.0 - last.0 < MIN_GAP_S) {
            kept.pop();
        }
        kept.push(entry);
    }

    match kept.first_mut() {
        Some(first) if first.0 < MIN_GAP_S => first.0 = 0.0,
        _ => kept.insert(0, (0.0, first_title.to_owned(), ChapterKind::Start)),
    }

    let long = vod_len >= 3600.0;
    kept.into_iter()
        .map(|(vod_t, title, kind)| Chapter {
            ts: chapter_ts(vod_t, long),
            title: clean_title(&title),
            vod_t,
            kind,
        })
        .collect()
}

/// The chapters as description lines, or `""` when YouTube would not accept them.
pub fn block(chapters: &[Chapter]) -> String {
    if chapters.len() < MIN_CHAPTERS {
        return String::new();
    }
    chapters
        .iter()
        .map(|c| format!("{} {}", c.ts, c.title))
        .collect::<Vec<_>>()
        .join("\n")
}

fn clean_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !matches!(c, '<' | '>'))
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_break_returns_to_the_chapter_before_it() {
        use ac_core::session_state::Checkpoint;
        let at = |s: f64| {
            "2026-11-20T10:00:00Z"
                .parse::<Timestamp>()
                .unwrap()
                .add_secs(s)
        };
        let mut state = SessionState::default();
        state.checkpoints.push(Checkpoint {
            cid: "c".into(),
            title: "Parser".into(),
            pressed_at: at(100.0),
            by: ac_core::event::By::Tui,
        });
        state.brb_starts = vec![at(200.0), at(400.0)];
        state.brb_ends = vec![at(260.0), at(460.0)];
        let mut c = candidates(&state, &config::Stream::default());
        c.sort_by_key(|c| c.0);
        let titles: Vec<&str> = c.iter().map(|c| c.1.as_str()).collect();
        assert_eq!(titles, ["Parser", "Break", "Parser", "Break", "Parser"]);
        // Off: no break chapters at all.
        let off = config::Stream {
            chapter_on_brb: false,
            ..config::Stream::default()
        };
        assert_eq!(candidates(&state, &off).len(), 1);
    }

    fn cp(t: f64, title: &str) -> (f64, String, ChapterKind) {
        (t, title.to_owned(), ChapterKind::Checkpoint)
    }

    fn lines(entries: Vec<(f64, String, ChapterKind)>, len: f64) -> Vec<String> {
        build(entries, len, "Start")
            .into_iter()
            .map(|c| format!("{} {}", c.ts, c.title))
            .collect()
    }

    #[test]
    fn no_checkpoints_gives_empty_block() {
        let chapters = build(vec![], 600.0, "Start");
        assert_eq!(chapters.len(), 1);
        assert_eq!(block(&chapters), "");
    }

    #[test]
    fn first_at_45s_gets_start_inserted() {
        assert_eq!(
            lines(vec![cp(45.0, "Setup"), cp(300.0, "Parser")], 600.0),
            ["0:00 Start", "0:45 Setup", "5:00 Parser"]
        );
    }

    #[test]
    fn first_within_10s_is_moved_to_zero() {
        assert_eq!(lines(vec![cp(7.5, "Intro")], 600.0), ["0:00 Intro"]);
    }

    #[test]
    fn two_within_6s_keep_the_later() {
        assert_eq!(
            lines(
                vec![cp(100.0, "Old"), cp(106.0, "New"), cp(200.0, "Next")],
                600.0
            ),
            ["0:00 Start", "1:46 New", "3:20 Next"]
        );
        // Collapsing near zero still yields a 0:00 first chapter.
        assert_eq!(lines(vec![cp(3.0, "A"), cp(8.0, "B")], 600.0), ["0:00 B"]);
    }

    #[test]
    fn over_an_hour_uses_long_format_everywhere() {
        assert_eq!(
            lines(vec![cp(60.0, "Setup"), cp(3915.0, "Parser")], 4000.0),
            ["0:00:00 Start", "0:01:00 Setup", "1:05:15 Parser"]
        );
    }

    #[test]
    fn out_of_range_dropped_and_titles_cleaned() {
        let long_title = "x".repeat(150);
        let chapters = build(
            vec![
                cp(-5.0, "before"),
                cp(700.0, "after"),
                cp(60.0, " <b>Bold</b> "),
                cp(120.0, &long_title),
            ],
            600.0,
            "Start",
        );
        assert_eq!(chapters.len(), 3);
        assert_eq!(chapters[1].title, "bBold/b");
        assert_eq!(chapters[2].title.chars().count(), 100);
        assert_eq!(block(&chapters).lines().count(), 3);
    }
}
