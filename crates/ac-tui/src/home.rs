//! The Home page (`0`, and where a session starts): what this session is, whether
//! everything is ready (every check, with how to fix what is not), live audio meters and
//! the scene on air. Everything on it can be reached with j/k and acted on with Enter.
//!
//! A new session is a draft here: nothing lasting or public happens until it is started,
//! and quitting before that keeps nothing.

use std::time::Duration;

use ac_core::command::{Command, OutputAction};
use ac_core::obs::{Audio, Media};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::theme::{ACCENT, FAINT, INFO, MUTED, OK, RAISED, SELECTED, TEXT, WARN};
use crate::{Check, CheckState, View};

/// Something on Home that j/k can reach and Enter acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// Start (or resume) the session.
    Start,
    /// Quit; a new draft keeps nothing.
    Quit,
    Dashboard,
    /// A check aftercast can fix with this key (R, S, M).
    Fix(char),
    /// An audio input: Enter mutes or unmutes, h/l change the volume.
    Audio(String),
    /// A source in the scene on air: Enter shows or hides it.
    Source(String),
}

/// What `Enter` on an item asks, as a command for the session (None: handled by the TUI).
pub fn command(item: &Item, view: &View) -> Option<Command> {
    Some(match item {
        Item::Start => Command::Begin,
        Item::Quit => Command::Quit,
        Item::Dashboard => return None,
        Item::Fix('R') => Command::Record {
            action: OutputAction::Start,
        },
        Item::Fix('S') => Command::Stream { on: true },
        Item::Fix(_) => Command::Mute {
            input: mic(&view.obs)?.name.clone(),
            on: Some(false),
        },
        Item::Audio(name) => Command::Mute {
            input: name.clone(),
            on: None,
        },
        Item::Source(source) => Command::Source {
            source: source.clone(),
            scene: None,
            visible: None,
        },
    })
}

/// Everything j/k can reach, in order: the decision, the fixes, audio, the scene on air.
pub fn items(view: &View) -> Vec<Item> {
    let mut items = if view.begun {
        vec![Item::Dashboard]
    } else {
        vec![Item::Start, Item::Quit]
    };
    items.extend(
        sorted_checks(view)
            .into_iter()
            .filter_map(|c| fix_key(c).map(Item::Fix)),
    );
    items.extend(
        sorted_audio(view)
            .into_iter()
            .map(|a| Item::Audio(a.name.clone())),
    );
    let scene = view.state.scene.as_deref().unwrap_or_default();
    items.extend(
        view.obs
            .sources_in(scene)
            .map(|i| Item::Source(i.source.clone())),
    );
    items
}

/// The first item of the next group after `pick` (Tab), wrapping to the top.
pub fn next_group(items: &[Item], pick: usize) -> usize {
    let group = |i: &Item| match i {
        Item::Start | Item::Quit | Item::Dashboard => 0,
        Item::Fix(_) => 1,
        Item::Audio(_) => 2,
        Item::Source(_) => 3,
    };
    let now = items.get(pick).map_or(0, group);
    items
        .iter()
        .enumerate()
        .skip(pick)
        .find(|(_, i)| group(i) != now)
        .map_or(0, |(n, _)| n)
}

/// What Enter does on `item`, for the key hints.
pub fn what(item: &Item, view: &View) -> String {
    match item {
        Item::Start if view.fresh => "start the session".into(),
        Item::Start => "resume the session".into(),
        Item::Quit if view.fresh => "quit, keep nothing".into(),
        Item::Quit => "quit".into(),
        Item::Dashboard => "go to the dashboard".into(),
        Item::Fix('R') => "start recording".into(),
        Item::Fix('S') => "start streaming".into(),
        Item::Fix(_) => "unmute the mic".into(),
        Item::Audio(name) => match view.obs.audio(name) {
            Some(a) if a.muted => format!("unmute {name}"),
            _ => format!("mute {name}"),
        },
        Item::Source(source) => match view
            .obs
            .item(view.state.scene.as_deref().unwrap_or_default(), source)
        {
            Some(i) if i.enabled => format!("hide {source}"),
            _ => format!("show {source}"),
        },
    }
}

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

/// The microphone `M` acts on: the first audio input capture.
pub fn mic(obs: &Media) -> Option<&Audio> {
    obs.audio.iter().find(|a| a.is_mic())
}

/// What needs you first, then what is still checking, then what is fine.
fn sorted_checks(view: &View) -> Vec<&Check> {
    let mut checks: Vec<&Check> = view.preflight.iter().collect();
    checks.sort_by_key(|c| match c.state {
        CheckState::Fail => 0,
        CheckState::Pending => 1,
        CheckState::Ok => 2,
    });
    checks
}

/// Mics first.
fn sorted_audio(view: &View) -> Vec<&Audio> {
    let mut audio: Vec<&Audio> = view.obs.audio.iter().collect();
    audio.sort_by_key(|a| !a.is_mic());
    audio
}

/// Draws the page over everything but the key hints (the status bar would repeat it).
/// `live` is the status bar's live part, `spin` a spinner frame, `pick` the focused item.
pub fn draw(frame: &mut Frame, view: &View, live: Line<'static>, spin: &'static str, pick: usize) {
    let items = items(view);
    let focus = items.get(pick).cloned();
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
        )
        .title_bottom(focus_hint(focus.as_ref(), view));
    let inner = block.inner(outer).inner(Margin::new(2, 1));
    frame.render_widget(block, outer);

    // Header: the logo beside the title, what is live, and the decision; one line on
    // what starting means below.
    let [head, about, _, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(inner);
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

    let (ok, failing, total) = counts(&view.preflight);
    let ready = failing == 0 && ok == total;
    let verdict = match failing {
        0 if ready => Span::styled("Everything is ready.", Style::new().fg(OK).bold()),
        0 => Span::styled("Still checking…", Style::new().fg(MUTED)),
        1 => Span::styled("1 thing needs you.", Style::new().fg(ACCENT).bold()),
        n => Span::styled(
            format!("{n} things need you."),
            Style::new().fg(ACCENT).bold(),
        ),
    };
    // Buttons keep their width whatever is focused: focus is color only.
    let button = |label: &str, item: Item, color: Color| {
        let style = if focus.as_ref() == Some(&item) {
            Style::new().fg(Color::Black).bg(color).bold()
        } else {
            Style::new().fg(TEXT).bg(RAISED)
        };
        Span::styled(format!("  {label}  "), style)
    };
    let go = if ready { OK } else { WARN };
    let (mut decision, explain) = if draft {
        let (start, quit) = if view.fresh {
            ("Start the session", "Quit, keep nothing")
        } else {
            ("Resume the session", "Quit")
        };
        let explain = if view.fresh {
            "Nothing is kept until you start (session folder, description push, key capture, \
             bot)."
        } else {
            "This session already exists; resuming continues it where it stopped."
        };
        (
            vec![
                button(start, Item::Start, go),
                Span::raw(" "),
                button(quit, Item::Quit, ACCENT),
            ],
            explain,
        )
    } else {
        (
            vec![
                Span::styled(" ● STARTED ", Style::new().fg(Color::Black).bg(OK).bold()),
                Span::raw(" "),
                button("Dashboard", Item::Dashboard, ACCENT),
            ],
            "0 brings you back here from the dashboard.",
        )
    };
    decision.push(Span::raw("   "));
    decision.push(verdict);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("#{:03}  ", view.meta.episode),
                    Style::new().fg(ACCENT).bold(),
                ),
                Span::styled(title, Style::new().fg(TEXT).bold()),
                Span::styled(
                    format!("   youtu.be/{}", view.meta.video_id),
                    Style::new().fg(INFO),
                ),
            ]),
            live,
            Line::from(decision),
        ]),
        who,
    );
    frame.render_widget(
        crate::truncate(
            Line::styled(explain, Style::new().fg(MUTED)),
            usize::from(about.width),
        ),
        about,
    );

    // Columns: checks, audio, the scene on air (audio and the scene share a column when
    // the terminal is narrow).
    let wide = body.width >= 120;
    let (left, audio_area, scene_area) = if wide {
        let [l, m, r] = Layout::horizontal([
            Constraint::Percentage(44),
            Constraint::Percentage(34),
            Constraint::Percentage(22),
        ])
        .areas(body);
        (l, m, Some(r))
    } else {
        let [l, r] = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)])
            .areas(body);
        (l, r, None)
    };
    let lit = |line: Line<'static>, on: bool, width: u16| {
        if !on {
            return line;
        }
        // The bar takes the first cell (a space); the rest of the indent stays.
        let mut spans = vec![Span::styled("▍", Style::new().fg(ACCENT))];
        let mut rest = line.spans.into_iter();
        if let Some(first) = rest.next() {
            let tail: String = first.content.chars().skip(1).collect();
            if !tail.is_empty() {
                spans.push(Span::styled(tail, first.style));
            }
        }
        spans.extend(rest);
        let used: usize = spans.iter().map(Span::width).sum();
        spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
        Line::from(spans).style(Style::new().bg(SELECTED))
    };

    // Checks: what is not fine yet in full, with its fix; the fine ones as one row of chips.
    let mut focus_line = None;
    let checks = sorted_checks(view);
    let open: Vec<&&Check> = checks
        .iter()
        .filter(|c| c.state != CheckState::Ok)
        .collect();
    let mut lines = vec![section(&format!("Needs you · {}", open.len()))];
    if open.is_empty() {
        lines.push(Line::styled(
            " ✔ Nothing. Every check is fine.",
            Style::new().fg(OK),
        ));
    }
    let detail_room = usize::from(left.width).saturating_sub(4);
    for c in open {
        let (mark, style) = match c.state {
            CheckState::Fail => ("✖", Style::new().fg(ACCENT).bold()),
            _ => (spin, Style::new().fg(MUTED)),
        };
        let key = fix_key(c);
        let on = key.is_some() && focus == key.map(Item::Fix);
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(format!("{mark} "), style),
            Span::styled(c.label.clone(), Style::new().fg(TEXT).bold()),
        ];
        // Details stay on the check's line, cut to fit.
        let room = detail_room.saturating_sub(c.label.chars().count() + 2);
        if !c.detail.is_empty() && room > 3 {
            let mut detail: String = c.detail.chars().take(room).collect();
            if c.detail.chars().count() > room {
                detail.pop();
                detail.push('…');
            }
            spans.push(Span::styled(format!("  {detail}"), Style::new().fg(FAINT)));
        }
        if on {
            focus_line = Some(lines.len());
        }
        lines.push(lit(Line::from(spans), on, left.width));
        if !c.fix.is_empty() {
            let mut fix = vec![Span::raw(" "), Span::raw("  ")];
            match key {
                Some(key) => {
                    fix.push(Span::styled(
                        format!(" {key} "),
                        Style::new().fg(Color::Black).bg(WARN).bold(),
                    ));
                    fix.push(Span::raw(" "));
                }
                None => fix.push(Span::styled("↳ ", Style::new().fg(WARN))),
            }
            let indent = fix.iter().map(Span::width).sum();
            fix.push(Span::styled(c.fix.clone(), Style::new().fg(WARN)));
            for row in crate::wrap(Line::from(fix), usize::from(left.width), indent) {
                if on {
                    focus_line = Some(lines.len());
                }
                lines.push(lit(row, on, left.width));
            }
        }
    }
    let fine: Vec<&&Check> = checks
        .iter()
        .filter(|c| c.state == CheckState::Ok)
        .collect();
    if !fine.is_empty() {
        lines.push(Line::raw(""));
        lines.push(section(&format!("Fine · {}", fine.len())));
        // Chips fill rows whole: a check's name never breaks across lines.
        let room = usize::from(left.width);
        let mut row = vec![Span::raw(" ")];
        let mut used = 1;
        for c in fine {
            let width = c.label.chars().count() + 2;
            if used > 1 && used + 3 + width > room {
                lines.push(Line::from(std::mem::replace(
                    &mut row,
                    vec![Span::raw(" ")],
                )));
                used = 1;
            }
            if used > 1 {
                row.push(Span::raw("   "));
                used += 3;
            }
            row.push(Span::styled("✔ ", Style::new().fg(OK)));
            row.push(Span::styled(c.label.clone(), Style::new().fg(MUTED)));
            used += width;
        }
        lines.push(Line::from(row));
    }
    let scroll = scroll_to(&lines, focus_line, left.width, left.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), left);

    // Audio meters, mics first.
    let mut focus_line = None;
    let width = usize::from(audio_area.width);
    let mut lines = vec![section("Audio")];
    let levels = view.levels.lock().map(|m| m.clone()).unwrap_or_default();
    let audio = sorted_audio(view);
    if audio.is_empty() {
        lines.push(Line::styled(
            " OBS is not connected, or has no audio inputs.",
            Style::new().fg(FAINT).italic(),
        ));
    }
    let name_width = audio
        .iter()
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(14);
    // " name " + "▕bar▏" + " -38dB" + " -12"
    let bar = width.saturating_sub(name_width + 16).clamp(6, 40);
    for a in audio {
        let level = levels.get(&a.name).copied();
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(
                format!("{:<name_width$.name_width$} ", a.name),
                Style::new().fg(TEXT),
            ),
        ];
        spans.extend(meter(level, a.muted, bar));
        spans.push(Span::styled(
            match (a.muted, level) {
                (true, _) => " muted".to_owned(),
                (false, Some(db)) if db > -60.0 => format!(" {db:>4.0}dB"),
                (false, _) => "  …   ".to_owned(),
            },
            Style::new().fg(if a.muted { ACCENT } else { MUTED }),
        ));
        spans.push(Span::styled(
            format!(" {:+.0}", a.volume_db),
            Style::new().fg(FAINT),
        ));
        let on = focus.as_ref() == Some(&Item::Audio(a.name.clone()));
        if on {
            focus_line = Some(lines.len());
        }
        lines.push(lit(Line::from(spans), on, audio_area.width));
    }

    // The scene on air: its own column when wide, else under the audio.
    let scene = view.state.scene.clone().unwrap_or_default();
    let scene_width = scene_area.unwrap_or(audio_area).width;
    let mut scene_lines = Vec::new();
    let mut scene_focus = None;
    if !scene.is_empty() {
        scene_lines.push(section(&format!("On air · {scene}")));
        for item in view.obs.sources_in(&scene) {
            let (mark, style) = if item.enabled {
                ("━━●", Style::new().fg(OK))
            } else {
                ("●━━", Style::new().fg(FAINT))
            };
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled(format!("{mark} "), style),
                Span::styled(
                    item.source.clone(),
                    Style::new().fg(if item.enabled { TEXT } else { MUTED }),
                ),
            ]);
            let on = focus.as_ref() == Some(&Item::Source(item.source.clone()));
            if on {
                scene_focus = Some(scene_lines.len());
            }
            scene_lines.push(lit(line, on, scene_width));
        }
    }
    match scene_area {
        Some(area) => {
            let scroll = scroll_to(&scene_lines, scene_focus, area.width, area.height);
            frame.render_widget(Paragraph::new(scene_lines).scroll((scroll, 0)), area);
        }
        None => {
            if !scene_lines.is_empty() {
                lines.push(Line::raw(""));
                let base = lines.len();
                focus_line = scene_focus.map(|i| base + i).or(focus_line);
                lines.extend(scene_lines);
            }
        }
    }
    let scroll = scroll_to(&lines, focus_line, audio_area.width, audio_area.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), audio_area);
}

/// How far to scroll `lines` (wrapped at `width`) so the line `focus` ends inside `height`
/// rows; 0 when nothing in the column is focused.
fn scroll_to(lines: &[Line], focus: Option<usize>, width: u16, height: u16) -> u16 {
    let Some(focus) = focus else {
        return 0;
    };
    let rows = |l: &Line| l.width().div_ceil(usize::from(width.max(1))).max(1);
    let end: usize = lines.iter().take(focus + 1).map(rows).sum();
    end.saturating_sub(usize::from(height)) as u16
}

/// What the focused item offers, faded in the page's bottom border.
fn focus_hint(focus: Option<&Item>, view: &View) -> Line<'static> {
    let Some(item) = focus else {
        return Line::default();
    };
    let mut pairs = vec![("Enter".to_owned(), what(item, view))];
    match item {
        Item::Audio(_) => pairs.push(("h/l".into(), "volume".into())),
        Item::Source(_) => pairs.push(("h/l".into(), "hide/show".into())),
        _ => {}
    }
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, label)) in pairs.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::new().fg(FAINT)));
        }
        spans.push(Span::styled(k, Style::new().fg(MUTED)));
        spans.push(Span::styled(format!(" {label}"), Style::new().fg(FAINT)));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// Checks that are fine, failing, and all.
fn counts(checks: &[Check]) -> (usize, usize, usize) {
    let of = |s| checks.iter().filter(|c| c.state == s).count();
    (of(CheckState::Ok), of(CheckState::Fail), checks.len())
}

fn section(name: &str) -> Line<'static> {
    Line::styled(
        format!(" {}", name.to_uppercase()),
        Style::new().fg(MUTED).bold(),
    )
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

/// Footer hints: the same keys whatever is focused, so the line never shifts (what Enter
/// does on the focused item is in the page's bottom border).
pub fn hints(view: &View) -> Vec<(String, String)> {
    let quit = match (view.begun, view.fresh) {
        (false, true) => "quit, keep nothing",
        _ => "quit",
    };
    [
        ("j/k", "move"),
        ("Tab", "next group"),
        ("Enter", "do it"),
        ("q", quit),
        (",", "settings"),
        (":", "command"),
    ]
    .into_iter()
    .map(|(k, l)| (k.to_owned(), l.to_owned()))
    .collect()
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

    #[test]
    fn scrolls_just_enough_to_show_the_focused_line() {
        let lines: Vec<Line> = (0..10).map(|i| Line::raw(format!("line {i}"))).collect();
        assert_eq!(scroll_to(&lines, None, 20, 4), 0);
        assert_eq!(scroll_to(&lines, Some(2), 20, 4), 0);
        assert_eq!(scroll_to(&lines, Some(9), 20, 4), 6);
        // A line that wraps over two rows counts as two.
        let mut wide = lines.clone();
        wide[0] = Line::raw("x".repeat(30));
        assert_eq!(scroll_to(&wide, Some(3), 20, 4), 1);
    }

    #[test]
    fn tab_jumps_to_the_next_group() {
        let items = [
            Item::Start,
            Item::Quit,
            Item::Fix('R'),
            Item::Audio("Mic".into()),
            Item::Audio("Desktop".into()),
            Item::Source("Camera".into()),
        ];
        assert_eq!(next_group(&items, 0), 2);
        assert_eq!(next_group(&items, 2), 3);
        assert_eq!(next_group(&items, 4), 5);
        assert_eq!(next_group(&items, 5), 0);
    }
}
