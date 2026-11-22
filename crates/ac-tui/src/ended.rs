//! The Stream Ended prompt: when the broadcast ends, a banner over the panes in block letters
//! with a sunset glow running through them, embers rising, a border that breathes, and the
//! key that finishes the session pulsing, so it is not missed. (The bell rings once, too.)

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::starting::letters;
use crate::theme::{FAINT, MUTED, OK, TEXT, hint, mix};

const AMBER: Color = Color::Rgb(0xFF, 0xB3, 0x47);
const EMBER: Color = Color::Rgb(0xFF, 0x5A, 0x3C);
const DEEP: Color = Color::Rgb(0xB0, 0x20, 0x3A);

/// One thing the end has to get through, as the prompt lists it.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Still going: a spinner.
    Working(String),
    Done(String),
    /// Will not happen by itself, or is taking long: a plain note.
    Note(String),
}

fn step_line(step: &Step, spin: &str) -> Line<'static> {
    let (mark, color, text) = match step {
        Step::Working(t) => (spin.to_owned(), AMBER, t),
        Step::Done(t) => ("✓".to_owned(), OK, t),
        Step::Note(t) => ("·".to_owned(), MUTED, t),
    };
    let text_color = if matches!(step, Step::Done(_)) {
        MUTED
    } else {
        TEXT
    };
    Line::from(vec![
        Span::styled(format!("{mark} "), Style::new().fg(color).bold()),
        Span::styled(text.clone(), Style::new().fg(text_color)),
    ])
}

/// Colors each ink cell: amber to ember to deep red, a glow sweeping through.
fn glow(rows: &[String], t: f64, motion: bool) -> Vec<Line<'static>> {
    let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(1) as f64;
    rows.iter()
        .enumerate()
        .map(|(y, row)| {
            let spans: Vec<Span<'static>> = row
                .chars()
                .enumerate()
                .map(|(x, c)| {
                    if c == ' ' {
                        return Span::raw(" ");
                    }
                    let pos = (x as f64 + y as f64 * 0.7) / width;
                    let wave = if motion {
                        ((pos * std::f64::consts::TAU) - t * 2.2).sin() * 0.5 + 0.5
                    } else {
                        pos
                    };
                    // Two blends: amber to ember, then ember to deep red, along the wave.
                    let color = if wave < 0.5 {
                        mix(AMBER, EMBER, wave * 2.0)
                    } else {
                        mix(EMBER, DEEP, (wave - 0.5) * 2.0)
                    };
                    Span::styled("█", Style::new().fg(color))
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// A row of embers: each rises through three marks, offset from its neighbours.
fn embers(width: usize, t: f64, motion: bool, shift: f64) -> Line<'static> {
    const MARKS: [&str; 5] = ["·", "˙", "•", "✦", "·"];
    let spans = (0..width)
        .map(|x| {
            if x % 3 != 1 {
                return Span::raw(" ");
            }
            let phase = if motion {
                t * 2.4 + x as f64 * 0.9 + shift
            } else {
                x as f64
            };
            let level = phase.sin() * 0.5 + 0.5;
            let mark = MARKS[(level * 4.99) as usize];
            Span::styled(mark, Style::new().fg(mix(FAINT, AMBER, level)))
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// The key that ends the session, filled and pulsing; the others quiet beside it.
fn keys(t: f64, motion: bool) -> Line<'static> {
    let beat = if motion {
        (t * 3.0).sin() * 0.5 + 0.5
    } else {
        0.5
    };
    let fill = mix(EMBER, AMBER, beat);
    let mut spans = vec![
        Span::styled(
            " f ",
            Style::new()
                .bg(fill)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " Finalize ",
            Style::new()
                .fg(mix(TEXT, AMBER, beat))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
    ];
    spans.extend(hint("k", "keep open"));
    spans.extend(hint("p", "finalize + post-process"));
    Line::from(spans)
}

/// Draws the prompt centered in `area`. `on_air` is how long the stream ran, `t` the animation
/// clock.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    on_air: Option<Duration>,
    t: Duration,
    motion: bool,
    spin: &str,
    steps: &[Step],
) {
    let t = t.as_secs_f64();
    let one = letters("STREAM ENDED");
    let wide = one[0].chars().count() as u16 + 8;
    let art: Vec<Line<'static>> = if area.width >= wide {
        glow(&one, t, motion)
    } else if area.width >= 44 {
        let mut lines = glow(&letters("STREAM"), t, motion);
        lines.push(Line::raw(""));
        lines.extend(glow(&letters("ENDED"), t + 0.4, motion));
        lines
    } else {
        vec![Line::styled("STREAM ENDED", Style::new().fg(AMBER).bold())]
    };
    let art_width = art.iter().map(Line::width).max().unwrap_or(12) as u16;
    let secs = on_air.map_or(0, |d| d.as_secs());
    let clock = if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    };
    let mut info = vec![Span::styled("You were on air  ", Style::new().fg(MUTED))];
    if on_air.is_some() {
        info.push(Span::styled(clock, Style::new().fg(AMBER).bold()));
    }
    let roomy = art.len() as u16 + 10 + (steps.len() as u16) < area.height;
    let mut lines = vec![];
    if roomy {
        lines.extend([embers(art_width as usize, t, motion, 0.0), Line::raw("")]);
    }
    lines.extend(art);
    if roomy {
        lines.extend([
            Line::raw(""),
            embers(art_width as usize, t, motion, 2.1),
            Line::raw(""),
        ]);
    }
    lines.push(Line::from(info));
    lines.push(Line::styled(
        "Finalize pushes the last description and ends the session",
        Style::new().fg(FAINT),
    ));
    lines.push(Line::raw(""));
    // What the end is still getting through (OBS stopping, YouTube ending the broadcast).
    if !steps.is_empty() {
        lines.extend(steps.iter().map(|s| step_line(s, spin)));
        lines.push(Line::raw(""));
    }
    lines.push(keys(t, motion));

    let width = (art_width + 10).max(70).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    // The border breathes between amber and ember; the dot in the title blinks.
    let breath = if motion {
        (t * 1.6).sin() * 0.5 + 0.5
    } else {
        0.5
    };
    let dot = if !motion || ((t * 2.0) as u64).is_multiple_of(2) {
        "●"
    } else {
        "○"
    };
    let block = Block::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(mix(DEEP, AMBER, breath)))
        .title(Line::styled(format!(" {dot} Off air "), Style::new().fg(AMBER).bold()).centered());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        rect,
    );
}
