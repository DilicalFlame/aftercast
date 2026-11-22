//! The Starting Soon banner: while OBS shows the starting scene, a banner over the panes
//! in block letters with a shimmer running through them, how long the starting screen has
//! been up, and Enter to move on to the main scene.

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::theme::{FAINT, MUTED, hint, mix};

const PINK: Color = Color::Rgb(0xFF, 0x6E, 0xC8);
const VIOLET: Color = Color::Rgb(0x96, 0x5A, 0xF0);

/// 5 rows per letter, `#` is ink.
fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'S' => [" ####", "#    ", " ### ", "    #", "#### "],
        'T' => ["#####", "  #  ", "  #  ", "  #  ", "  #  "],
        'A' => [" ### ", "#   #", "#####", "#   #", "#   #"],
        'R' => ["#### ", "#   #", "#### ", "#  # ", "#   #"],
        'I' => ["###", " # ", " # ", " # ", "###"],
        'N' => ["#   #", "##  #", "# # #", "#  ##", "#   #"],
        'E' => ["#####", "#    ", "#### ", "#    ", "#####"],
        'M' => ["#   #", "## ##", "# # #", "#   #", "#   #"],
        'D' => ["#### ", "#   #", "#   #", "#   #", "#### "],
        'G' => [" ####", "#    ", "#  ##", "#   #", " ####"],
        'O' => [" ### ", "#   #", "#   #", "#   #", " ### "],
        _ => ["  ", "  ", "  ", "  ", "  "],
    }
}

/// The word in block letters, one string per row.
pub fn letters(word: &str) -> [String; 5] {
    let mut rows: [String; 5] = Default::default();
    for (i, c) in word.chars().enumerate() {
        for (row, part) in rows.iter_mut().zip(glyph(c)) {
            if i > 0 {
                row.push(' ');
            }
            row.push_str(part);
        }
    }
    rows
}

/// Colors each ink cell: a pink to violet band sweeping left to right, still without motion.
fn shimmer(rows: &[String], t: f64, motion: bool) -> Vec<Line<'static>> {
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
                    let pos = (x as f64 + y as f64 * 0.6) / width;
                    let wave = if motion {
                        ((pos * std::f64::consts::TAU) - t * 2.4).sin() * 0.5 + 0.5
                    } else {
                        pos
                    };
                    Span::styled("█", Style::new().fg(mix(VIOLET, PINK, wave)))
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// A row of sparkles twinkling one after another.
fn sparkles(width: usize, t: f64, motion: bool) -> Line<'static> {
    const MARKS: [&str; 4] = ["·", "•", "✦", "•"];
    let spans = (0..width)
        .map(|x| {
            if x % 4 != 0 {
                return Span::raw(" ");
            }
            let phase = if motion {
                t * 3.0 - x as f64 * 0.35
            } else {
                x as f64
            };
            let level = phase.sin() * 0.5 + 0.5;
            let mark = MARKS[(level * 3.99) as usize];
            Span::styled(mark, Style::new().fg(mix(FAINT, PINK, level)))
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// Draws the banner centered in `area`. `up` is how long the starting screen has shown,
/// `t` the animation clock.
pub fn draw(frame: &mut Frame, area: Rect, up: Duration, t: Duration, motion: bool, main: &str) {
    let t = t.as_secs_f64();
    // One line of letters when it fits, else two, else plain words.
    let one = letters("STARTING SOON");
    let wide = one[0].chars().count() as u16 + 8;
    let art: Vec<Line<'static>> = if area.width >= wide {
        shimmer(&one, t, motion)
    } else if area.width >= 52 {
        let mut lines = shimmer(&letters("STARTING"), t, motion);
        lines.push(Line::raw(""));
        lines.extend(shimmer(&letters("SOON"), t + 0.4, motion));
        lines
    } else {
        vec![Line::styled("STARTING SOON", Style::new().fg(PINK).bold())]
    };
    let art_width = art.iter().map(Line::width).max().unwrap_or(13) as u16;
    let secs = up.as_secs();
    let clock = if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{:02}:{:02}", secs / 60, secs % 60)
    };
    let up_line = Line::from(vec![
        Span::styled("Viewers see the starting screen  ", Style::new().fg(MUTED)),
        Span::styled(clock, Style::new().fg(PINK).bold()),
    ]);
    let mut keys = vec![];
    keys.extend(hint("Enter", &format!("go to {main}")));
    keys.push(Span::raw("   "));
    keys.extend(hint(":scene", "pick another"));
    let keys = Line::from(keys);
    // Sparkles and air when there is room; the letters, clock and key always.
    let roomy = art.len() as u16 + 10 <= area.height;
    let mut lines = vec![];
    if roomy {
        lines.extend([sparkles(art_width as usize, t, motion), Line::raw("")]);
    }
    lines.extend(art);
    if roomy {
        lines.extend([
            Line::raw(""),
            sparkles(art_width as usize, t + 1.3, motion),
            Line::raw(""),
        ]);
    }
    lines.push(up_line);
    if roomy {
        lines.push(Line::raw(""));
    }
    lines.push(keys);

    let width = (art_width + 8).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let glow = if motion {
        (t * 1.2).sin() * 0.5 + 0.5
    } else {
        0.5
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(mix(VIOLET, PINK, glow)))
        .title(Line::styled(" On air soon ", Style::new().fg(PINK).bold()).centered());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        rect,
    );
}

/// The end screen's banner: ENDING and the seconds left before the stream stops.
pub fn draw_ending(frame: &mut Frame, area: Rect, left: Duration, t: Duration, motion: bool) {
    let t = t.as_secs_f64();
    let one = letters("ENDING");
    let art: Vec<Line<'static>> = if area.width >= one[0].chars().count() as u16 + 8 {
        shimmer(&one, t, motion)
    } else {
        vec![Line::styled("ENDING", Style::new().fg(PINK).bold())]
    };
    let art_width = art.iter().map(Line::width).max().unwrap_or(6) as u16;
    let secs = left.as_secs_f64().ceil() as u64;
    let clock = format!("{:02}:{:02}", secs / 60, secs % 60);
    let mut lines = art;
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("The stream ends in  ", Style::new().fg(MUTED)),
        Span::styled(clock, Style::new().fg(PINK).bold()),
    ]));
    lines.push(Line::from(hint(":end cancel", "stay live").into_iter().collect::<Vec<_>>()));
    let width = (art_width + 8).max(34).min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(VIOLET))
        .title(Line::styled(" Ending ", Style::new().fg(PINK).bold()).centered());
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(block),
        rect,
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn letters_line_up_row_by_row() {
        let rows = super::letters("SOON");
        assert!(
            rows.iter()
                .all(|r| r.chars().count() == rows[0].chars().count())
        );
        assert_eq!(rows[0], " ####  ###   ###  #   #");
    }
}
