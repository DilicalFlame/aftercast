//! Stage 10, activity: `derived/activity.csv`, 10 s bins over the master timeline.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;

use ac_core::event::EventKind;

use crate::ctx::{inside, overlap, read_json};
use crate::sources;
use crate::transcribe::Words;
use crate::{Ctx, Result};

pub const BIN_S: f64 = 10.0;
/// Viewers react this long after seeing something (spec 05).
// ponytail: fixed; a `post.reaction_s` setting once someone needs another value.
const REACTION_S: f64 = 2.0;

const HEADER: &str = "t,words,speech_s,keys,keys_chars,shortcuts,shell_cmds,commits,chat_msgs,chat_unique,questions,marks,scene,brb,privacy,frame_change";

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bin {
    pub t: f64,
    pub words: u32,
    pub speech_s: f64,
    pub keys: u32,
    pub keys_chars: u32,
    pub shortcuts: u32,
    pub shell_cmds: u32,
    pub commits: u32,
    pub chat_msgs: u32,
    pub chat_unique: u32,
    pub questions: u32,
    pub marks: u32,
    pub scene: String,
    pub brb: bool,
    pub privacy: bool,
    pub frame_change: u32,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let bins = compute(ctx)?;
    let mut csv = String::from(HEADER);
    csv.push('\n');
    for b in &bins {
        let _ = writeln!(
            csv,
            "{},{},{:.1},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            b.t,
            b.words,
            b.speech_s,
            b.keys,
            b.keys_chars,
            b.shortcuts,
            b.shell_cmds,
            b.commits,
            b.chat_msgs,
            b.chat_unique,
            b.questions,
            b.marks,
            b.scene.replace([',', '\n'], ";"),
            u8::from(b.brb),
            u8::from(b.privacy),
            b.frame_change
        );
    }
    let path = ctx.derived("activity.csv");
    fs::write(&path, csv).map_err(|e| format!("{}: {e}", path.display()))
}

fn compute(ctx: &Ctx) -> Result<Vec<Bin>> {
    let duration = ctx.master()?.duration_s;
    let n = (duration / BIN_S).ceil().max(1.0) as usize;
    let mut bins: Vec<Bin> = (0..n)
        .map(|i| Bin {
            t: i as f64 * BIN_S,
            ..Bin::default()
        })
        .collect();
    let bin = |s: f64| -> Option<usize> {
        (s >= 0.0 && s < duration).then(|| ((s / BIN_S) as usize).min(n - 1))
    };

    if let Ok(words) = read_json::<Words>(&ctx.derived("words.json")) {
        let spans: Vec<(f64, f64)> = words.segments.iter().map(|s| (s.start, s.end)).collect();
        for s in &words.segments {
            for w in &s.words {
                if let Some(i) = bin(w.start) {
                    bins[i].words += 1;
                }
            }
        }
        for b in &mut bins {
            b.speech_s = overlap(&spans, b.t, b.t + BIN_S);
        }
    }
    for k in sources::keys(ctx) {
        if let Some(i) = ctx.rec(k.t).and_then(bin) {
            bins[i].keys += 1;
            bins[i].keys_chars += u32::from(k.ch.is_some());
            bins[i].shortcuts += u32::from(k.combo().is_some());
        }
    }
    for l in sources::shell(ctx) {
        if let Some(i) = ctx.rec(l.t).and_then(bin) {
            bins[i].shell_cmds += 1;
        }
    }
    for c in sources::commits(ctx) {
        if let Some(i) = ctx.rec(c.t).and_then(bin) {
            bins[i].commits += 1;
        }
    }
    // Chat reacts to what viewers saw `latency + reaction` earlier.
    let shift = ctx
        .session
        .meta
        .anchors
        .latency_s
        .unwrap_or(ctx.config.stream.default_latency_s)
        + REACTION_S;
    let mut authors: Vec<HashSet<String>> = vec![HashSet::new(); n];
    for m in sources::chat(ctx) {
        if let Some(i) = ctx.rec(m.t_received.add_secs(-shift)).and_then(bin) {
            bins[i].chat_msgs += 1;
            authors[i].insert(m.author_channel_id);
        }
    }
    for (b, a) in bins.iter_mut().zip(&authors) {
        b.chat_unique = a.len() as u32;
    }
    for e in &ctx.events {
        match &e.kind {
            EventKind::QuestionAdd { .. } => {
                if let Some(i) = ctx.rec(e.t.add_secs(-shift)).and_then(bin) {
                    bins[i].questions += 1;
                }
            }
            EventKind::Mark { pressed_at, .. } => {
                if let Some(i) = ctx.rec(*pressed_at).and_then(bin) {
                    bins[i].marks += 1;
                }
            }
            _ => {}
        }
    }
    // The scene in effect at each bin's start.
    let scenes: Vec<(f64, String)> = ctx
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::ObsScene { to, .. } => Some((ctx.rec_clamped(e.t, duration), to.clone())),
            _ => None,
        })
        .collect();
    let brb = ctx.brb(duration);
    let privacy = ctx.privacy(duration);
    for b in &mut bins {
        if let Some((_, s)) = scenes.iter().rev().find(|(t, _)| *t <= b.t) {
            b.scene = s.clone();
        }
        b.brb = overlap(&brb, b.t, b.t + BIN_S) > 0.0 || inside(&brb, b.t);
        b.privacy = overlap(&privacy, b.t, b.t + BIN_S) > 0.0;
        // ponytail: frame_change stays 0 (spec: optional 2 s low-res probe).
    }
    Ok(bins)
}

/// Reads `derived/activity.csv` back.
pub fn read(ctx: &Ctx) -> Result<Vec<Bin>> {
    let path = ctx.derived("activity.csv");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split(',').collect();
        if f.len() != 16 {
            return Err(format!("{}: bad line {line:?}", path.display()));
        }
        let u = |i: usize| f[i].parse::<u32>().unwrap_or(0);
        let x = |i: usize| f[i].parse::<f64>().unwrap_or(0.0);
        out.push(Bin {
            t: x(0),
            words: u(1),
            speech_s: x(2),
            keys: u(3),
            keys_chars: u(4),
            shortcuts: u(5),
            shell_cmds: u(6),
            commits: u(7),
            chat_msgs: u(8),
            chat_unique: u(9),
            questions: u(10),
            marks: u(11),
            scene: f[12].to_owned(),
            brb: f[13] == "1",
            privacy: f[14] == "1",
            frame_change: u(15),
        });
    }
    Ok(out)
}
