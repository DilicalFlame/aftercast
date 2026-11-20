//! The Home page (`0`, and where a session starts): what this session is, whether
//! everything is ready (every check, with how to fix what is not), live audio meters and
//! the scene on air.
//!
//! A new session is a draft here: nothing lasting or public happens until Enter starts
//! it, and `q` before that quits keeping nothing.

use std::time::Duration;

use ac_core::obs::{Audio, Media};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::theme::{ACCENT, FAINT, INFO, MUTED, OK, RAISED, TEXT, WARN};
use crate::{Check, CheckState, View};

/// The key Home offers for a check that is not fine, if aftercast can fix it.
pub fn fix_key(check: &Check) -> Option<char> {
    if check.state == CheckState::Ok || check.fix.is_empty() {
        return None;
    }
    match check.label.as_str() {
        "OBS recording" => Some('R'),
        "OBS streaming" => Some('S'),
        "Mic live" if check.state == CheckState::Fail => Some('M'),
        _ => None,
    }
}

/// The microphone `M` mutes: the first audio input capture.
pub fn mic(obs: &Media) -> Option<&Audio> {
    obs.audio.iter().find(|a| a.is_mic())
}

/// Draws the page over everything but the key hints (the status bar would repeat it).
/// `live` is the status bar's live part; `spin` a spinner frame.
pub fn draw(frame: &mut Frame, view: &View, live: Line<'static>, spin: &'static str) {
    let area = frame.area();
    let outer = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(Clear, outer);
    let draft = !view.begun;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if draft { WARN } else { ACCENT }))
        .title(Line::from(vec![
            Span::styled(" ◉ ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
            Span::styled(" aftercast ", Style::new().fg(TEXT).bold()),
        ]))
        .title(
            Span::styled(format!(" {} ", view.session), Style::new().fg(FAINT))
                .into_right_aligned_line(),
        );
    let inner = block.inner(outer).inner(Margin::new(2, 1));
    frame.render_widget(block, outer);

    let [head, _, action, _, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(inner);

    // The logo tile, the episode and what is live.
    let [logo, who] = Layout::horizontal([Constraint::Length(11), Constraint::Min(10)]).areas(head);
    let tile = Style::new().fg(Color::White).bg(ACCENT).bold();
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("  ▗▄▄▖   ", tile),
            Line::styled(" ▐  ●    ", tile),
            Line::styled("  ▝▀▀▘   ", tile),
        ]),
        logo,
    );
    let title = view
        .meta
        .title_at_start
        .clone()
        .unwrap_or_else(|| format!("Episode {}", view.meta.episode));
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("#{:03}  ", view.meta.episode),
                    Style::new().fg(ACCENT).bold(),
                ),
                Span::styled(title, Style::new().fg(TEXT).bold()),
            ]),
            Line::from(Span::styled(
                format!("youtu.be/{}", view.meta.video_id),
                Style::new().fg(INFO).underlined(),
            )),
            live,
        ]),
        who,
    );

    // The one decision on this page.
    let (ok, failing, total) = counts(&view.preflight);
    let ready = failing == 0 && ok == total;
    let verdict = match failing {
        0 if ready => Span::styled("Everything is ready.", Style::new().fg(OK).bold()),
        0 => Span::styled("Still checking…", Style::new().fg(MUTED)),
        1 => Span::styled("1 thing needs you below.", Style::new().fg(ACCENT).bold()),
        n => Span::styled(
            format!("{n} things need you below."),
            Style::new().fg(ACCENT).bold(),
        ),
    };
    let lines = if draft {
        let what = if view.fresh {
            "Nothing is kept until you start. Starting creates the session, pushes the \
             description (if set), and starts key capture and the bot."
        } else {
            "This session already exists; Resume continues it where it stopped."
        };
        let (start, quit) = if view.fresh {
            (" Enter  Start the session ", " q  Quit, keep nothing ")
        } else {
            (" Enter  Resume the session ", " q  Quit ")
        };
        vec![
            Line::from(vec![
                Span::styled(
                    start,
                    Style::new()
                        .fg(Color::Black)
                        .bg(if ready { OK } else { WARN })
                        .bold(),
                ),
                Span::raw("   "),
                Span::styled(quit, Style::new().fg(TEXT).bg(RAISED)),
                Span::raw("   "),
                verdict,
            ]),
            Line::raw(""),
            Line::styled(what, Style::new().fg(MUTED)),
        ]
    } else {
        vec![
            Line::from(vec![
                Span::styled(" ● STARTED ", Style::new().fg(Color::Black).bg(OK).bold()),
                Span::raw("   "),
                verdict,
            ]),
            Line::raw(""),
            Line::styled(
                "Enter, Esc or 0 go to the dashboard; 0 brings you back here.",
                Style::new().fg(MUTED),
            ),
        ]
    };
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), action);

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(body);

    // Checks: what needs you first, each with how to fix it; then the fine ones.
    let mut checks: Vec<&Check> = view.preflight.iter().collect();
    checks.sort_by_key(|c| match c.state {
        CheckState::Fail => 0,
        CheckState::Pending => 1,
        CheckState::Ok => 2,
    });
    let mut lines = vec![
        section(&format!("Checks · {ok} of {total} fine")),
        Line::raw(""),
    ];
    for c in checks {
        let (mark, style) = match c.state {
            CheckState::Ok => ("✔", Style::new().fg(OK)),
            CheckState::Fail => ("✖", Style::new().fg(ACCENT).bold()),
            CheckState::Pending => (spin, Style::new().fg(MUTED)),
        };
        let label = if c.state == CheckState::Ok {
            Style::new().fg(MUTED)
        } else {
            Style::new().fg(TEXT).bold()
        };
        let mut spans = vec![
            Span::styled(format!("{mark} "), style),
            Span::styled(c.label.clone(), label),
        ];
        if !c.detail.is_empty() {
            spans.push(Span::styled(
                format!("  {}", c.detail),
                Style::new().fg(FAINT),
            ));
        }
        lines.push(Line::from(spans));
        if !c.fix.is_empty() {
            let mut fix = vec![Span::raw("  ")];
            if let Some(key) = fix_key(c) {
                fix.push(Span::styled(
                    format!(" {key} "),
                    Style::new().fg(Color::Black).bg(WARN).bold(),
                ));
                fix.push(Span::raw(" "));
            } else {
                fix.push(Span::styled("↳ ", Style::new().fg(WARN)));
            }
            fix.push(Span::styled(c.fix.clone(), Style::new().fg(WARN)));
            lines.push(Line::from(fix));
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), left);

    // Audio meters, mics first, then the scene on air.
    let width = right.width as usize;
    let mut lines = vec![section("Audio"), Line::raw("")];
    let levels = view.levels.lock().map(|m| m.clone()).unwrap_or_default();
    let mut audio: Vec<&Audio> = view.obs.audio.iter().collect();
    audio.sort_by_key(|a| !a.is_mic());
    if audio.is_empty() {
        lines.push(Line::styled(
            "OBS is not connected, or has no audio inputs.",
            Style::new().fg(FAINT).italic(),
        ));
    }
    let name_width = audio
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(16);
    let bar = width.saturating_sub(name_width + 18).clamp(6, 30);
    for a in audio {
        let level = levels.get(&a.name).copied();
        let mut spans = vec![Span::styled(
            format!("{:<name_width$.name_width$} ", a.name),
            Style::new().fg(TEXT),
        )];
        spans.extend(meter(level, a.muted, bar));
        spans.push(Span::styled(
            match (a.muted, level) {
                (true, _) => " muted".to_owned(),
                (false, Some(db)) if db > -60.0 => format!(" {db:>4.0} dB"),
                (false, _) => "  …".to_owned(),
            },
            Style::new().fg(if a.muted { ACCENT } else { MUTED }),
        ));
        lines.push(Line::from(spans));
    }
    let scene = view.state.scene.clone().unwrap_or_default();
    if !scene.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section(&format!("On air · {scene}")));
        lines.push(Line::raw(""));
        for item in view.obs.sources_in(&scene) {
            let (mark, style) = if item.enabled {
                ("━━●", Style::new().fg(OK))
            } else {
                ("●━━", Style::new().fg(FAINT))
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{mark} "), style),
                Span::styled(
                    item.source.clone(),
                    Style::new().fg(if item.enabled { TEXT } else { MUTED }),
                ),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(lines), right);
}

/// Checks that are fine, failing, and all.
fn counts(checks: &[Check]) -> (usize, usize, usize) {
    let of = |s| checks.iter().filter(|c| c.state == s).count();
    (of(CheckState::Ok), of(CheckState::Fail), checks.len())
}

fn section(name: &str) -> Line<'static> {
    Line::styled(name.to_uppercase(), Style::new().fg(MUTED).bold())
}

/// A level meter: `cells` wide over -60..0 dBFS, green, amber from -12, red from -3.
pub fn meter(level: Option<f32>, muted: bool, cells: usize) -> Vec<Span<'static>> {
    let db = level.unwrap_or(-60.0);
    let lit = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * cells as f32).round() as usize;
    let color_at = |i: usize| {
        let at = -60.0 + 60.0 * (i as f32 + 0.5) / cells as f32;
        match () {
            _ if muted => FAINT,
            _ if at >= -3.0 => ACCENT,
            _ if at >= -12.0 => WARN,
            _ => OK,
        }
    };
    let mut spans = vec![Span::styled("▕", Style::new().fg(RAISED))];
    for i in 0..cells {
        if i < lit {
            spans.push(Span::styled("█", Style::new().fg(color_at(i))));
        } else {
            spans.push(Span::styled("░", Style::new().fg(RAISED)));
        }
    }
    spans.push(Span::styled("▏", Style::new().fg(RAISED)));
    spans
}

/// Key hints for the page: the decision first, then the fixes on offer.
pub fn hints(view: &View) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = if view.begun {
        vec![("Enter".into(), "dashboard".into())]
    } else if view.fresh {
        vec![
            ("Enter".into(), "start".into()),
            ("q".into(), "quit, keep nothing".into()),
        ]
    } else {
        vec![
            ("Enter".into(), "resume".into()),
            ("q".into(), "quit".into()),
        ]
    };
    let mut mic_fixed = false;
    for c in &view.preflight {
        if let Some(key) = fix_key(c) {
            mic_fixed |= key == 'M';
            let what = c.fix.split(" (").next().unwrap_or_default();
            pairs.push((key.to_string(), what.to_owned()));
        }
    }
    if !mic_fixed && let Some(m) = mic(&view.obs) {
        let what = if m.muted { "unmute mic" } else { "mute mic" };
        pairs.push(("M".into(), what.into()));
    }
    pairs.push((",".into(), "settings".into()));
    pairs.push((":".into(), "command".into()));
    pairs
}

/// How often the page redraws to keep meters fluid.
pub const FRAME: Duration = Duration::from_millis(50);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_fill_and_color_by_level() {
        let text = |spans: Vec<Span>| {
            spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(text(meter(Some(-60.0), false, 6)), "▕░░░░░░▏");
        assert_eq!(text(meter(Some(0.0), false, 6)), "▕██████▏");
        assert_eq!(text(meter(Some(-30.0), false, 6)), "▕███░░░▏");
        // 20 cells: the last one is in the red zone above -3 dB, the first is green.
        let loud = meter(Some(0.0), false, 20);
        assert_eq!(loud[20].style.fg, Some(ACCENT));
        assert_eq!(loud[18].style.fg, Some(WARN));
        assert_eq!(loud[1].style.fg, Some(OK));
        assert_eq!(meter(Some(0.0), true, 6)[6].style.fg, Some(FAINT));
    }

    #[test]
    fn checks_aftercast_can_fix_have_keys() {
        let check = |label: &str, state| Check {
            label: label.into(),
            state,
            detail: String::new(),
            fix: "do it".into(),
        };
        assert_eq!(
            fix_key(&check("OBS recording", CheckState::Fail)),
            Some('R')
        );
        assert_eq!(fix_key(&check("OBS recording", CheckState::Ok)), None);
        assert_eq!(fix_key(&check("Mic live", CheckState::Fail)), Some('M'));
        assert_eq!(fix_key(&check("Config valid", CheckState::Fail)), None);
    }
}
