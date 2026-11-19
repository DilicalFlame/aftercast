//! Stage 11, segments: `derived/segments.json`, the master split into speech, focus, dead,
//! brb and redacted ranges.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::activity::{self, BIN_S};
use crate::ctx::{overlap, read_json, write_json};
use crate::media::round_ms;
use crate::transcribe::Words;
use crate::{Ctx, Result};

/// Classification resolution, and the shortest segment (spec: min length 5 s).
const CELL_S: f64 = 5.0;
/// Speech covering at least this share of a cell makes it `speech`.
const SPEECH_SHARE: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Speech,
    Focus,
    Dead,
    Brb,
    Redacted,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Speech => "speech",
            Self::Focus => "focus",
            Self::Dead => "dead",
            Self::Brb => "brb",
            Self::Redacted => "redacted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segments {
    pub totals: BTreeMap<String, f64>,
    pub segments: Vec<Segment>,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let duration = ctx.master()?.duration_s;
    let bins = activity::read(ctx)?;
    let speech: Vec<(f64, f64)> = read_json::<Words>(&ctx.derived("words.json"))
        .map(|w| w.segments.iter().map(|s| (s.start, s.end)).collect())
        .unwrap_or_default();
    let privacy = ctx.privacy(duration);
    let brb = ctx.brb(duration);
    let min_keys = ctx.config.post.focus_min_keys_per_10s;

    let mut cells = Vec::new();
    let mut a = 0.0;
    while a < duration {
        let b = (a + CELL_S).min(duration);
        let bin = bins.get((a / BIN_S) as usize);
        let (kind, reason) = if overlap(&privacy, a, b) > 0.0 {
            (Kind::Redacted, Some("privacy".to_owned()))
        } else if overlap(&brb, a, b) * 2.0 >= b - a {
            (Kind::Brb, None)
        } else if overlap(&speech, a, b) >= SPEECH_SHARE * (b - a) {
            (Kind::Speech, None)
        } else if bin.is_some_and(|x| {
            x.keys >= min_keys || x.shell_cmds > 0 || x.commits > 0 || x.frame_change > 0
        }) {
            (Kind::Focus, None)
        } else {
            (Kind::Dead, None)
        };
        cells.push(Segment {
            start: a,
            end: b,
            kind,
            reason,
        });
        a = b;
    }
    write_json(&ctx.derived("segments.json"), &finish(cells))
}

/// Merges neighbours of the same kind and computes totals.
pub fn finish(cells: Vec<Segment>) -> Segments {
    let mut segments: Vec<Segment> = Vec::new();
    for c in cells {
        match segments.last_mut() {
            Some(last) if last.kind == c.kind && (last.end - c.start).abs() < 1e-6 => {
                last.end = c.end;
                if last.reason != c.reason {
                    last.reason = match (last.reason.take(), c.reason) {
                        (Some(a), Some(b)) if !a.contains(&b) => Some(format!("{a}, {b}")),
                        (a, b) => a.or(b),
                    };
                }
            }
            _ => segments.push(c),
        }
    }
    let mut totals = BTreeMap::new();
    for s in &mut segments {
        s.start = round_ms(s.start);
        s.end = round_ms(s.end);
        *totals.entry(s.kind.name().to_owned()).or_insert(0.0) += s.end - s.start;
    }
    for v in totals.values_mut() {
        *v = round_ms(*v);
    }
    Segments { totals, segments }
}

/// Marks `ranges` as redacted (with `reason`), splitting segments at the range edges.
pub fn redact(segments: Segments, ranges: &[(f64, f64)], reason: &str) -> Segments {
    let mut cells = Vec::new();
    for s in segments.segments {
        // Cut points inside this segment.
        let mut cuts = vec![s.start, s.end];
        for (a, b) in ranges {
            for x in [*a, *b] {
                if x > s.start && x < s.end {
                    cuts.push(x);
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let hit = overlap(ranges, a, b) > 0.0;
            cells.push(if hit && s.kind != Kind::Redacted {
                Segment {
                    start: a,
                    end: b,
                    kind: Kind::Redacted,
                    reason: Some(reason.to_owned()),
                }
            } else {
                Segment {
                    start: a,
                    end: b,
                    ..s.clone()
                }
            });
        }
    }
    finish(cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: f64, end: f64, kind: Kind) -> Segment {
        Segment {
            start,
            end,
            kind,
            reason: None,
        }
    }

    #[test]
    fn merges_neighbours_and_totals() {
        let s = finish(vec![
            seg(0.0, 5.0, Kind::Speech),
            seg(5.0, 10.0, Kind::Speech),
            seg(10.0, 15.0, Kind::Dead),
        ]);
        assert_eq!(
            s.segments,
            [seg(0.0, 10.0, Kind::Speech), seg(10.0, 15.0, Kind::Dead)]
        );
        assert_eq!(s.totals["speech"], 10.0);
    }

    #[test]
    fn redaction_splits_segments() {
        let s = finish(vec![
            seg(0.0, 20.0, Kind::Speech),
            seg(20.0, 40.0, Kind::Focus),
        ]);
        let s = redact(s, &[(15.0, 25.0)], "secret (shell)");
        let kinds: Vec<(f64, f64, Kind)> = s
            .segments
            .iter()
            .map(|x| (x.start, x.end, x.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                (0.0, 15.0, Kind::Speech),
                (15.0, 25.0, Kind::Redacted),
                (25.0, 40.0, Kind::Focus)
            ]
        );
        assert_eq!(s.segments[1].reason.as_deref(), Some("secret (shell)"));
    }
}
