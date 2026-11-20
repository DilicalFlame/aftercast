//! `captions.srt` and `keys.ass` for a rendered item, in output time.

use std::fmt::Write;

use ac_capture::keys::KeyRecord;
use ac_core::config::Overlay;
use ac_post::Word;

use crate::{Cut, MUTE_ABOVE, round_ms};

/// Characters per caption line, and lines per caption.
const LINE: usize = 42;
const LINES: usize = 2;
/// Words closer than this share a caption.
const GAP_S: f64 = 1.2;
/// How long a shortcut chip stays, and how many stack up.
const CHIP_S: f64 = 1.2;
const CHIP_SLOTS: usize = 3;
/// The same chip again within this long after it ends extends it instead of stacking.
const REPEAT_S: f64 = 0.3;
/// Typing fades after this much idle time and starts over.
const IDLE_S: f64 = 1.5;
/// Typed characters shown.
const TYPED: usize = 24;

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Words inside the cuts, in output time. Muted (fast) cuts have none.
fn out_words<'a>(words: &'a [Word], cuts: &[Cut]) -> Vec<(usize, f64, f64, &'a Word)> {
    let mut out = Vec::new();
    let mut off = 0.0;
    for (i, cut) in cuts.iter().enumerate() {
        if cut.speed <= MUTE_ABOVE {
            for w in words
                .iter()
                .filter(|w| w.start >= cut.start && w.end <= cut.end)
            {
                let at = |t: f64| round_ms(off + (t - cut.start) / cut.speed);
                out.push((i, at(w.start), at(w.end), w));
            }
        }
        off += cut.out_len();
    }
    out
}

/// Captions of at most two 42-character lines; words more than [`GAP_S`] apart or in
/// different cuts start a new caption.
pub fn captions(words: &[Word], cuts: &[Cut]) -> Vec<Cue> {
    let mut cues: Vec<Cue> = Vec::new();
    let mut last_cut = usize::MAX;
    for (cut, start, end, w) in out_words(words, cuts) {
        let text = w.w.trim();
        if text.is_empty() {
            continue;
        }
        match cues.last_mut() {
            Some(c)
                if cut == last_cut
                    && start - c.end <= GAP_S
                    && c.text.chars().count() + 1 + text.chars().count() <= LINE * LINES =>
            {
                c.text.push(' ');
                c.text.push_str(text);
                c.end = end;
            }
            _ => cues.push(Cue {
                start,
                end,
                text: text.to_owned(),
            }),
        }
        last_cut = cut;
    }
    cues
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

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

/// Breaks a caption into two lines at the last space that fits the first.
fn wrap(text: &str) -> String {
    if text.chars().count() <= LINE {
        return text.to_owned();
    }
    let mut first = 0;
    for (i, ch) in text.char_indices() {
        if text[..i].chars().count() > LINE {
            break;
        }
        if ch == ' ' {
            first = i;
        }
    }
    if first == 0 {
        return text.to_owned();
    }
    format!("{}\n{}", &text[..first], &text[first + 1..])
}

/// Thumbnail time: the most confidently heard word in the first 3 s, else 1 s in.
// ponytail: word confidence stands in for loudness; measure audio energy if thumbs pick
// dull frames.
pub fn thumb_time(words: &[Word], cuts: &[Cut], total: f64) -> f64 {
    out_words(words, cuts)
        .into_iter()
        .filter(|(_, start, _, _)| *start < 3.0)
        .max_by(|a, b| a.3.p.unwrap_or(0.0).total_cmp(&b.3.p.unwrap_or(0.0)))
        .map_or(1.0_f64.min(total / 2.0), |(_, s, e, _)| (s + e) / 2.0)
}

struct Line {
    start: f64,
    end: f64,
    style: &'static str,
    slot: usize,
    fade: bool,
    text: String,
}

/// `keys.ass`: shortcuts as chips stacked bottom-left, typing as one line of the last
/// characters under them. `keys` are master times, sorted. `None` when nothing was typed in
/// the cuts.
pub fn keys_ass(
    keys: &[(f64, KeyRecord)],
    cuts: &[Cut],
    theme: &Overlay,
    w: u32,
    h: u32,
) -> Option<String> {
    let mut lines: Vec<Line> = Vec::new();
    let mut off = 0.0;
    for cut in cuts {
        let end = off + cut.out_len();
        let inside: Vec<(f64, &KeyRecord)> = keys
            .iter()
            .filter(|(t, _)| *t >= cut.start && *t < cut.end)
            .map(|(t, k)| (off + (t - cut.start) / cut.speed, k))
            .collect();
        chips(&inside, end, &mut lines);
        typing(&inside, end, &mut lines);
        off = end;
    }
    if lines.is_empty() {
        return None;
    }
    let size = (f64::from(h) * 0.045).round().max(8.0);
    let pad = (size * 0.3).round();
    let margin_l = (f64::from(w) * 0.025).round();
    let margin_v = (f64::from(h) * 0.04).round();
    let row = (size * 1.7).round();
    let (fg, bg, accent) = (color(&theme.fg), color(&theme.bg), color(&theme.accent));
    let font = &theme.font_mono;
    let mut out = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {w}\nPlayResY: {h}\nWrapStyle: 2\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\n\
         Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
         Style: Typing,{font},{size},{fg},{fg},{bg},{bg},0,0,0,0,100,100,0,0,3,{pad},0,1,{margin_l},{margin_l},{margin_v},1\n\
         Style: Chip,{font},{size},{fg},{fg},{accent},{accent},1,0,0,0,100,100,0,0,3,{pad},0,1,{margin_l},{margin_l},{margin_v},1\n\n\
         [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n"
    );
    for l in &lines {
        let v = margin_v + row * (l.slot as f64);
        let fade = if l.fade { "{\\fad(0,250)}" } else { "" };
        let _ = writeln!(
            out,
            "Dialogue: 0,{},{},{},,0,0,{v},,{fade}{}",
            ass_time(l.start),
            ass_time(l.end),
            l.style,
            escape(&l.text)
        );
    }
    Some(out)
}

/// Shortcut chips in slots 1 to 3 (slot 0 is the typing line); a fourth pushes out the
/// oldest.
fn chips(keys: &[(f64, &KeyRecord)], cut_end: f64, lines: &mut Vec<Line>) {
    let mut slots: [Option<usize>; CHIP_SLOTS] = [None; CHIP_SLOTS];
    let mut last: Option<usize> = None;
    for (t, k) in keys {
        let Some(chip) = k.chip() else { continue };
        let text = chip.replace('+', " + ");
        // A held key (auto-repeat) or the same key again right away: one chip that stays.
        if let Some(i) = last.filter(|&i| lines[i].text == text && lines[i].end + REPEAT_S >= *t) {
            lines[i].end = (t + CHIP_S).min(cut_end);
            continue;
        }
        let free = slots
            .iter()
            .position(|s| s.is_none_or(|i| lines[i].end <= *t));
        let slot = free.unwrap_or_else(|| {
            let (slot, _) = slots
                .iter()
                .enumerate()
                .min_by(|a, b| {
                    let end = |s: &Option<usize>| s.map_or(0.0, |i| lines[i].end);
                    end(a.1).total_cmp(&end(b.1))
                })
                .expect("three slots");
            if let Some(i) = slots[slot] {
                lines[i].end = *t;
            }
            slot
        });
        slots[slot] = Some(lines.len());
        last = Some(lines.len());
        lines.push(Line {
            start: *t,
            end: (t + CHIP_S).min(cut_end),
            style: "Chip",
            slot: slot + 1,
            fade: false,
            text,
        });
    }
}

#[derive(Clone, Copy)]
enum Edit {
    Char(char),
    Back,
    /// Ctrl+Backspace: delete the last word.
    Word,
}

/// Typed characters as one line of the last [`TYPED`], cleared by Enter or idle time.
fn typing(keys: &[(f64, &KeyRecord)], cut_end: f64, lines: &mut Vec<Line>) {
    let typed: Vec<(f64, Edit)> = keys
        .iter()
        .filter_map(|(t, k)| {
            let edit = match (k.combo().as_deref(), k.ch.as_deref()) {
                (Some("Ctrl+Backspace"), _) => Edit::Word,
                (Some(_), _) => return None,
                (None, Some(ch)) => Edit::Char(ch.chars().next()?),
                (None, None) if k.k == "Backspace" => Edit::Back,
                (None, None) => return None,
            };
            Some((*t, edit))
        })
        .collect();
    let mut text: Vec<char> = Vec::new();
    for (i, &(t, edit)) in typed.iter().enumerate() {
        if i > 0 && t - typed[i - 1].0 > IDLE_S {
            text.clear();
        }
        match edit {
            Edit::Char('\n' | '\r') => text.clear(),
            Edit::Char('\t') => text.push(' '),
            Edit::Char(c) => text.push(c),
            Edit::Back => {
                text.pop();
            }
            Edit::Word => {
                while text.last() == Some(&' ') {
                    text.pop();
                }
                while text.last().is_some_and(|c| *c != ' ') {
                    text.pop();
                }
            }
        }
        let next = typed.get(i + 1).map(|n| n.0).filter(|n| *n <= t + IDLE_S);
        let (end, fade) = match next {
            Some(n) => (n.min(cut_end), false),
            None => ((t + IDLE_S).min(cut_end), true),
        };
        if !text.is_empty() && end > t {
            let shown: String = text[text.len().saturating_sub(TYPED)..].iter().collect();
            lines.push(Line {
                start: t,
                end,
                style: "Typing",
                slot: 0,
                fade,
                text: shown,
            });
        }
    }
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!(
        "{}:{:02}:{:02}.{:02}",
        cs / 360_000,
        cs / 6000 % 60,
        cs / 100 % 60,
        cs % 100
    )
}

/// Literal text for an ASS line: braces escaped; a backslash gets a zero-width space after
/// it so `\n`, `\N` and `\h` stay as typed.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\\' => out.push_str("\\\u{200B}"),
            c => out.push(c),
        }
    }
    out
}

/// `#RRGGBB` as an ASS colour (`&HAABBGGRR`, opaque); white if unparsable.
fn color(hex: &str) -> String {
    let hex = hex.trim_start_matches('#');
    let byte = |i: usize| {
        hex.get(i..i + 2)
            .and_then(|b| u8::from_str_radix(b, 16).ok())
    };
    match (hex.len(), byte(0), byte(2), byte(4)) {
        (6, Some(r), Some(g), Some(b)) => format!("&H00{b:02X}{g:02X}{r:02X}"),
        _ => "&H00FFFFFF".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::time::Timestamp;

    fn cut(start: f64, end: f64, speed: f64) -> Cut {
        Cut {
            start,
            end,
            speed,
            suggested: [start, end],
            label: None,
        }
    }

    fn word(w: &str, start: f64) -> Word {
        Word {
            w: w.into(),
            start,
            end: start + 0.3,
            p: Some(0.5),
        }
    }

    fn key(k: &str, ch: Option<&str>, mods: &[&str]) -> KeyRecord {
        KeyRecord {
            t: Timestamp::now(),
            k: k.into(),
            mods: mods.iter().map(|m| m.to_string()).collect(),
            app: "Code.exe".into(),
            ch: ch.map(String::from),
        }
    }

    #[test]
    fn captions_shift_into_output_time_and_split_on_gaps_and_cuts() {
        let words = [
            word("one", 10.0),
            word("two", 10.5),
            word("three", 13.0),
            word("four", 21.0),
            word("fast", 31.0),
        ];
        let cuts = [
            cut(9.0, 15.0, 1.0),
            cut(20.0, 22.0, 2.0),
            cut(30.0, 40.0, 8.0),
        ];
        let cues = captions(&words, &cuts);
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        // A 2.2 s gap splits; the 8x cut is muted, so no captions.
        assert_eq!(texts, ["one two", "three", "four"]);
        assert_eq!((cues[0].start, cues[0].end), (1.0, 1.8));
        // 21.0 is 1 s into a 2x cut that starts at output 6.0.
        assert_eq!(cues[2].start, 6.5);
        assert_eq!(
            srt(&cues[..1]),
            "1\n00:00:01,000 --> 00:00:01,800\none two\n\n"
        );
    }

    #[test]
    fn long_captions_wrap_into_two_lines() {
        let text = "this caption is long enough that it needs two lines of text";
        let wrapped = wrap(text);
        let lines: Vec<&str> = wrapped.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() <= LINE);
    }

    #[test]
    fn keys_overlay_has_chips_and_escaped_typing() {
        let keys = vec![
            (1.0, key("KeyS", None, &["Ctrl"])),
            (1.1, key("KeyP", None, &["Ctrl", "Shift"])),
            (1.2, key("KeyF", None, &["Ctrl"])),
            (1.3, key("KeyG", None, &["Ctrl"])),
            (2.0, key("KeyA", Some("{"), &[])),
            (2.1, key("KeyB", Some("\\"), &[])),
            (2.2, key("KeyN", Some("n"), &[])),
            (2.3, key("Backspace", None, &[])),
        ];
        let ass = keys_ass(
            &keys,
            &[cut(0.0, 10.0, 1.0)],
            &Overlay::default(),
            1920,
            1080,
        )
        .unwrap();
        assert!(ass.contains("PlayResX: 1920"));
        assert!(ass.contains(",Chip,,0,0,") && ass.contains("Ctrl + Shift + P"));
        // The fourth chip pushes out the first (Ctrl + S ends at 1.3).
        assert!(
            ass.contains("Dialogue: 0,0:00:01.00,0:00:01.30,Chip"),
            "{ass}"
        );
        assert!(ass.contains("\\{\\\u{200B}n"), "{ass}");
        // Backspace removes the n; the last line fades.
        assert!(
            ass.contains(
                "Dialogue: 0,0:00:02.30,0:00:03.80,Typing,,0,0,43,,{\\fad(0,250)}\\{\\\u{200B}"
            ),
            "{ass}"
        );
        assert!(
            keys_ass(
                &keys,
                &[cut(5.0, 10.0, 1.0)],
                &Overlay::default(),
                1920,
                1080
            )
            .is_none()
        );
    }

    #[test]
    fn held_arrow_is_one_chip_and_enter_shows() {
        let mut keys: Vec<(f64, KeyRecord)> = (0..10)
            .map(|i| (1.0 + i as f64 * 0.1, key("RightArrow", None, &[])))
            .collect();
        keys.push((
            3.0,
            key(
                "Return",
                Some(
                    "
",
                ),
                &[],
            ),
        ));
        let ass = keys_ass(
            &keys,
            &[cut(0.0, 10.0, 1.0)],
            &Overlay::default(),
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(ass.matches(",Chip,,").count(), 2, "{ass}");
        // The held arrow lasts until 1.2 s after the last repeat; Enter is its own chip.
        assert!(
            ass.contains("Dialogue: 0,0:00:01.00,0:00:03.10,Chip,"),
            "{ass}"
        );
        assert!(ass.lines().any(|l| l.ends_with(",,→")), "{ass}");
        assert!(
            ass.contains("Dialogue: 0,0:00:03.00,0:00:04.20,Chip,"),
            "{ass}"
        );
        assert!(ass.lines().any(|l| l.ends_with(",,Enter")), "{ass}");
    }

    #[test]
    fn colors_are_bgr() {
        assert_eq!(color("#FF5555"), "&H005555FF");
        assert_eq!(color("nope"), "&H00FFFFFF");
    }
}
