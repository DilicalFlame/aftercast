//! The Home page (`0`, opened when a session starts): who and what is live, whether
//! everything is ready (the pre-flight checks, each failing one with the key that fixes
//! it), live audio meters and the sources of the scene on air.

use std::time::Duration;

use ac_core::obs::{Audio, Media};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::theme::{ACCENT, FAINT, INFO, MUTED, OK, RAISED, TEXT, WARN};
use crate::{Check, CheckState, View};

/// The key that fixes a failing check, if there is one.
pub fn fix(check: &Check) -> Option<(char, &'static str)> {
    if check.state != CheckState::Fail {
        return None;
    }
    match check.label.as_str() {
        "OBS recording" => Some(('R', "start recording")),
        "OBS streaming" => Some(('S', "start streaming")),
        _ => None,
    }
}

/// The microphone `M` mutes: the first audio input capture.
pub fn mic(obs: &Media) -> Option<&Audio> {
    obs.audio.iter().find(|a| a.is_mic())
}

/// Draws the page over everything between the status bar and the key hints.
/// `status` is the live line from the status bar; `spin` a spinner frame.
pub fn draw(frame: &mut Frame, view: &View, status: Line<'static>, spin: &'static str) {
    let area = frame.area();
    let outer = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(2),
        ..area
    };
    frame.render_widget(Clear, outer);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(vec![
            Span::styled(" ◉ ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
            Span::styled(" Home ", Style::new().fg(TEXT).bold()),
        ]))
        .title(
            Span::styled(format!(" {} ", view.session), Style::new().fg(FAINT))
                .into_right_aligned_line(),
        );
    let inner = block.inner(outer).inner(Margin::new(2, 1));
    frame.render_widget(block, outer);

    let [head, _, ready, _, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(inner);

    // The logo tile and who is live.
    let [logo, who] = Layout::horizontal([Constraint::Length(10), Constraint::Min(10)]).areas(head);
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
                Span::styled("aftercast  ", Style::new().fg(ACCENT).bold()),
                Span::styled(title, Style::new().fg(TEXT).bold()),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("#{:03} · ", view.meta.episode),
                    Style::new().fg(MUTED),
                ),
                Span::styled(
                    format!("youtu.be/{}", view.meta.video_id),
                    Style::new().fg(INFO).underlined(),
                ),
            ]),
            status,
        ]),
        who,
    );

    // Readiness: how many checks pass, and a gauge.
    let total = view.preflight.len();
    let ok = view
        .preflight
        .iter()
        .filter(|c| c.state == CheckState::Ok)
        .count();
    let failing = view
        .preflight
        .iter()
        .filter(|c| c.state == CheckState::Fail)
        .count();
    let (headline, color) = match failing {
        0 if ok == total => ("Ready to go".to_owned(), OK),
        0 => ("Checking…".to_owned(), MUTED),
        1 => ("1 thing needs you".to_owned(), ACCENT),
        n => (format!("{n} things need you"), ACCENT),
    };
    let cells = 24;
    let filled = (ok * cells).checked_div(total).unwrap_or(0);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(headline, Style::new().fg(color).bold()),
                Span::styled(
                    format!("   {ok} of {total} checks pass"),
                    Style::new().fg(MUTED),
                ),
            ]),
            Line::from(vec![
                Span::styled("▰".repeat(filled), Style::new().fg(color)),
                Span::styled("▱".repeat(cells - filled), Style::new().fg(RAISED)),
            ]),
        ]),
        ready,
    );

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(body);

    // Checks, the failing ones first, each with its fix key.
    let mut checks: Vec<&Check> = view.preflight.iter().collect();
    checks.sort_by_key(|c| match c.state {
        CheckState::Fail => 0,
        CheckState::Pending => 1,
        CheckState::Ok => 2,
    });
    let mut lines = vec![section("Checks"), Line::raw("")];
    for c in checks {
        let (mark, style) = match c.state {
            CheckState::Ok => ("✔", Style::new().fg(OK)),
            CheckState::Fail => ("✖", Style::new().fg(ACCENT).bold()),
            CheckState::Pending => (spin, Style::new().fg(MUTED)),
        };
        let mut spans = vec![
            Span::styled(format!("{mark} "), style),
            Span::styled(
                c.label.clone(),
                if c.state == CheckState::Fail {
                    Style::new().fg(TEXT).bold()
                } else {
                    Style::new().fg(TEXT)
                },
            ),
        ];
        if let Some((key, what)) = fix(c) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!(" {key} "),
                Style::new().fg(Color::Black).bg(WARN).bold(),
            ));
            spans.push(Span::styled(format!(" {what}"), Style::new().fg(WARN)));
        } else if !c.detail.is_empty() {
            spans.push(Span::styled(
                format!("  {}", c.detail),
                Style::new().fg(FAINT),
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), left);

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
    let sources: Vec<_> = view.obs.sources_in(&scene).collect();
    if !scene.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("On air  ", Style::new().fg(ACCENT).bold()),
            Span::styled(scene.clone(), Style::new().fg(TEXT).bold()),
        ]));
        for item in sources {
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

/// Key hints for the page.
pub fn hints(view: &View) -> Vec<(&'static str, String)> {
    let mut pairs = vec![("Enter", "dashboard".to_owned())];
    for c in &view.preflight {
        if let Some((key, what)) = fix(c) {
            pairs.push((if key == 'R' { "R" } else { "S" }, what.to_owned()));
        }
    }
    if let Some(m) = mic(&view.obs) {
        let what = if m.muted { "unmute mic" } else { "mute mic" };
        pairs.push(("M", what.to_owned()));
    }
    pairs.push((",", "settings".into()));
    pairs.push((":", "command".into()));
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
    fn failing_recording_and_streaming_have_fix_keys() {
        let check = |label: &str, state| Check {
            label: label.into(),
            state,
            detail: String::new(),
        };
        assert_eq!(
            fix(&check("OBS recording", CheckState::Fail)),
            Some(('R', "start recording"))
        );
        assert_eq!(fix(&check("OBS recording", CheckState::Ok)), None);
        assert_eq!(fix(&check("Config valid", CheckState::Fail)), None);
    }
}
