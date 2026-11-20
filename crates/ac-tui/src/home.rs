//! The Home page (`0`, and where a session starts): what this session is, whether
//! everything is ready (every check, with how to fix what is not), live audio meters and
//! the scene on air. Everything on it can be reached with j/k and acted on with Enter.
//!
//! A new session is a draft here: nothing lasting or public happens until it is started,
//! and quitting before that keeps nothing.

use std::time::Duration;

use ac_core::command::{Command, OutputAction};
use ac_core::config::HomeLayout;
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

/// The tabs of the `tabs` layout.
pub const TABS: [&str; 2] = ["Checks", "Studio"];

/// Everything j/k can reach, in order: the decision, the fixes, audio, the scene on air.
/// In the `tabs` layout only the open tab's (`tab`) and the buttons.
pub fn items(view: &View, tab: usize) -> Vec<Item> {
    let tabs = view.config.tui.home_layout == HomeLayout::Tabs;
    let mut items = if view.begun {
        vec![Item::Dashboard]
    } else {
        vec![Item::Start, Item::Quit]
    };
    if !tabs || tab == 0 {
        items.extend(
            sorted_checks(view)
                .into_iter()
                .filter_map(|c| fix_key(c).map(Item::Fix)),
        );
    }
    if !tabs || tab == 1 {
        let audio = sorted_audio(view)
            .into_iter()
            .map(|a| Item::Audio(a.name.clone()));
        let scene = view.state.scene.as_deref().unwrap_or_default();
        let sources = view
            .obs
            .sources_in(scene)
            .map(|i| Item::Source(i.source.clone()));
        // j/k follow the reading order: in `columns` the sources sit under the checks,
        // left, before the audio on the right.
        if view.config.tui.home_layout == HomeLayout::Columns {
            items.extend(sources);
            items.extend(audio);
        } else {
            items.extend(audio);
            items.extend(sources);
        }
    }
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
///
/// Every section takes the full width and only as many rows as it needs, so nothing is
/// cut mid-word; the sections below the header scroll as one page, only when they do not
/// fit, keeping the focused row in view.
pub fn draw(
    frame: &mut Frame,
    view: &View,
    live: Line<'static>,
    spin: &'static str,
    pick: usize,
    tab: usize,
) {
    let items = items(view, tab);
    let focus = items.get(pick).cloned();
    let area = frame.area();
    let outer = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(Clear, outer);
    let draft = !view.begun;
    let frame_block = || {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if draft { WARN } else { ACCENT }))
    };
    let inner = frame_block().inner(outer).inner(Margin::new(2, 1));
    let width = usize::from(inner.width);

    // Header: the logo beside the title, what is live (wrapping, never cut) and the decision.
    let head_width = width.saturating_sub(11);
    let mut who = vec![title_line(view)];
    who.extend(crate::wrap(live, head_width, 0));
    who.push(decision_line(view, focus.as_ref()));
    let explain = crate::wrap(
        Line::styled(explain_text(view), Style::new().fg(MUTED)),
        width,
        0,
    );
    let head_rows = who.len().max(3) as u16;
    let [head, about, _, body] = Layout::vertical([
        Constraint::Length(head_rows),
        Constraint::Length(explain.len() as u16),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(inner);
    let [logo, who_area] =
        Layout::horizontal([Constraint::Length(11), Constraint::Min(10)]).areas(head);
    let tile = Style::new().fg(Color::White).bg(ACCENT).bold();
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled("  ▗▄▄▖   ", tile),
            Line::styled(" ▐  ●    ", tile),
            Line::styled("  ▝▀▀▘   ", tile),
        ]),
        logo,
    );
    frame.render_widget(Paragraph::new(who), who_area);
    frame.render_widget(Paragraph::new(explain), about);

    // The sections, each as tall as its content, arranged as `tui.home_layout` says.
    let ctx = Ctx {
        view,
        focus: focus.as_ref(),
        spin,
    };
    let more = match view.config.tui.home_layout {
        HomeLayout::Page => page(
            frame,
            join(vec![
                ctx.needs_you(body.width),
                ctx.audio(body.width),
                ctx.on_air(body.width),
            ]),
            body,
        ),
        HomeLayout::Columns => {
            let [l, r] = columns(body, 50);
            // The scene on air fills the room under what needs you; audio has the right.
            let left = page(
                frame,
                join(vec![ctx.needs_you(l.width), ctx.on_air_list(l.width)]),
                l,
            );
            let right = page(frame, ctx.audio(r.width), r);
            (left.0 || right.0, left.1 || right.1)
        }
        HomeLayout::Classic => {
            let [l, r] = columns(body, 55);
            let left = page(frame, ctx.needs_you(l.width), l);
            let right = page(
                frame,
                join(vec![ctx.audio(r.width), ctx.on_air_list(r.width)]),
                r,
            );
            (left.0 || right.0, left.1 || right.1)
        }
        HomeLayout::Tabs => {
            let [bar, _, rest] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .areas(body);
            frame.render_widget(tab_bar(view, tab), bar);
            let content = if tab == 0 {
                ctx.needs_you(rest.width)
            } else {
                join(vec![ctx.audio(rest.width), ctx.on_air_list(rest.width)])
            };
            page(frame, content, rest)
        }
    };

    // The frame last: its bottom border says what Enter does and whether there is more.
    let mut block = frame_block()
        .title(Line::from(vec![
            Span::styled(" ◉ ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
            Span::styled(" aftercast ", Style::new().fg(TEXT).bold()),
        ]))
        .title(
            Span::styled(format!(" {} ", view.session), Style::new().fg(FAINT))
                .into_right_aligned_line(),
        )
        .title_bottom(focus_hint(focus.as_ref(), view));
    let arrows = match more {
        (true, true) => " ↑↓ more ",
        (true, false) => " ↑ more ",
        (false, true) => " ↓ more ",
        (false, false) => "",
    };
    if !arrows.is_empty() {
        block = block
            .title_bottom(Span::styled(arrows, Style::new().fg(WARN)).into_right_aligned_line());
    }
    frame.render_widget(block, outer);
}

/// Two columns of `body`, the left `left` percent wide, with a gap of 3.
fn columns(body: Rect, left: u16) -> [Rect; 2] {
    let [l, r] = Layout::horizontal([
        Constraint::Percentage(left),
        Constraint::Percentage(100 - left),
    ])
    .areas(body);
    [
        Rect {
            width: l.width.saturating_sub(3),
            ..l
        },
        r,
    ]
}

/// The `tabs` layout's tab bar: the open tab lit, each with what it holds.
fn tab_bar(view: &View, tab: usize) -> Line<'static> {
    let open = view
        .preflight
        .iter()
        .filter(|c| c.state != CheckState::Ok)
        .count();
    let counts = [
        if open == 0 {
            "all fine".to_owned()
        } else {
            format!("{open} need you")
        },
        format!("{} audio", view.obs.audio.len()),
    ];
    let mut spans = vec![Span::raw(" ")];
    for (i, name) in TABS.iter().enumerate() {
        let style = if i == tab {
            Style::new().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::new().fg(TEXT).bg(RAISED)
        };
        spans.push(Span::styled(format!("  {name} · {}  ", counts[i]), style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled("  [ ] switch", Style::new().fg(FAINT)));
    Line::from(spans)
}

/// A section's rows (already wrapped to its width) and the row holding the focus.
type Section = (Vec<Line<'static>>, Option<usize>);

/// Sections one after another, a blank row between them.
fn join(sections: Vec<Section>) -> Section {
    let mut lines = Vec::new();
    let mut focus = None;
    for (rows, f) in sections.into_iter().filter(|(rows, _)| !rows.is_empty()) {
        if !lines.is_empty() {
            lines.push(Line::raw(""));
        }
        if let Some(f) = f {
            focus = Some(lines.len() + f);
        }
        lines.extend(rows);
    }
    (lines, focus)
}

/// Draws a section in `area`, scrolled to keep its focus in view; whether rows are hidden
/// above and below.
fn page(frame: &mut Frame, (lines, focus): Section, area: Rect) -> (bool, bool) {
    let scroll = scroll_to(&lines, focus, area.width, area.height);
    let below = lines.len() > usize::from(scroll) + usize::from(area.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), area);
    (scroll > 0, below)
}

fn title_line(view: &View) -> Line<'static> {
    let title = view
        .meta
        .title_at_start
        .clone()
        .unwrap_or_else(|| format!("Episode {}", view.meta.episode));
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
    ])
}

/// The buttons (focus shown by color only, so the line never shifts) and the verdict.
fn decision_line(view: &View, focus: Option<&Item>) -> Line<'static> {
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
    let button = |label: &str, item: Item, color: Color| {
        let style = if focus == Some(&item) {
            Style::new().fg(Color::Black).bg(color).bold()
        } else {
            Style::new().fg(TEXT).bg(RAISED)
        };
        Span::styled(format!("  {label}  "), style)
    };
    let mut spans = if view.begun {
        vec![
            Span::styled(" ● STARTED ", Style::new().fg(Color::Black).bg(OK).bold()),
            Span::raw(" "),
            button("Dashboard", Item::Dashboard, ACCENT),
        ]
    } else {
        let (start, quit) = if view.fresh {
            ("Start the session", "Quit, keep nothing")
        } else {
            ("Resume the session", "Quit")
        };
        vec![
            button(start, Item::Start, if ready { OK } else { WARN }),
            Span::raw(" "),
            button(quit, Item::Quit, ACCENT),
        ]
    };
    spans.push(Span::raw("   "));
    spans.push(verdict);
    Line::from(spans)
}

fn explain_text(view: &View) -> &'static str {
    match (view.begun, view.fresh) {
        (true, _) => "0 brings you back here from the dashboard.",
        (false, true) => {
            "Nothing is kept until you start (session folder, description push, key capture, \
             bot)."
        }
        (false, false) => "This session already exists; resuming continues it where it stopped.",
    }
}

/// What every section needs to draw itself.
struct Ctx<'a> {
    view: &'a View,
    focus: Option<&'a Item>,
    spin: &'static str,
}

impl Ctx<'_> {
    /// Checks that are not fine yet, each with how to fix it, wrapped under its text.
    fn needs_you(&self, width: u16) -> Section {
        let w = usize::from(width);
        let open: Vec<&Check> = sorted_checks(self.view)
            .into_iter()
            .filter(|c| c.state != CheckState::Ok)
            .collect();
        // Passing checks are counted, not listed: only what is not fine takes room.
        let (ok, _, total) = counts(&self.view.preflight);
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!(" NEEDS YOU · {}", open.len()),
                Style::new().fg(MUTED).bold(),
            ),
            Span::styled(
                format!("     ✔ {ok} of {total} checks pass"),
                Style::new().fg(OK),
            ),
        ])];
        if open.is_empty() {
            lines.push(Line::styled(
                " Nothing. Every check passes.",
                Style::new().fg(OK),
            ));
            return (lines, None);
        }
        let mut focus = None;
        for c in open {
            let (mark, style) = match c.state {
                CheckState::Fail => ("✖", Style::new().fg(ACCENT).bold()),
                _ => (self.spin, Style::new().fg(MUTED)),
            };
            let key = fix_key(c);
            let on = key.is_some() && self.focus == key.map(Item::Fix).as_ref();
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(format!("{mark} "), style),
                Span::styled(c.label.clone(), Style::new().fg(TEXT).bold()),
            ];
            if !c.detail.is_empty() {
                spans.push(Span::styled(
                    format!("   {}", c.detail),
                    Style::new().fg(FAINT),
                ));
            }
            let mut rows = crate::wrap(Line::from(spans), w, 3);
            if !c.fix.is_empty() {
                let mut fix = vec![Span::raw("    ")];
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
                rows.extend(crate::wrap(Line::from(fix), w, indent));
            }
            for row in rows {
                if on {
                    focus = Some(lines.len());
                }
                lines.push(lit(row, on, width));
            }
        }
        (lines, focus)
    }

    /// A long level meter per audio input, mics first, with level, volume and mute.
    fn audio(&self, width: u16) -> Section {
        let mut lines = vec![section("Audio")];
        let levels = self
            .view
            .levels
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        let audio = sorted_audio(self.view);
        if audio.is_empty() {
            lines.push(Line::styled(
                " OBS is not connected, or has no audio inputs.",
                Style::new().fg(FAINT).italic(),
            ));
            return (lines, None);
        }
        let name_width = audio
            .iter()
            .map(|a| a.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(20);
        // " name  " + "▕bar▏" + " -38dB" + "  vol-12" + "  muted": the meter gives way
        // first, so the row fits narrow columns too.
        let bar = usize::from(width)
            .saturating_sub(name_width + 26)
            .clamp(4, 24);
        let mut focus = None;
        for a in audio {
            let level = levels.get(&a.name).copied();
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(
                    format!("{:<name_width$.name_width$}  ", a.name),
                    Style::new().fg(TEXT),
                ),
            ];
            spans.extend(meter(level, a.muted, bar));
            spans.push(Span::styled(
                match level {
                    Some(db) if db > -60.0 && !a.muted => format!(" {db:>3.0}dB"),
                    _ => "    … ".to_owned(),
                },
                Style::new().fg(MUTED),
            ));
            spans.push(Span::styled(
                format!("  vol{:+.0}", a.volume_db),
                Style::new().fg(FAINT),
            ));
            if a.muted {
                spans.push(Span::styled("  muted", Style::new().fg(ACCENT).bold()));
            }
            let on = self.focus == Some(&Item::Audio(a.name.clone()));
            if on {
                focus = Some(lines.len());
            }
            lines.push(lit(Line::from(spans), on, width));
        }
        (lines, focus)
    }

    /// The scene on air: its sources as switches in a row, the focused one lit.
    fn on_air(&self, width: u16) -> Section {
        let scene = self.view.state.scene.clone().unwrap_or_default();
        if scene.is_empty() {
            return (Vec::new(), None);
        }
        let chips: Vec<(Vec<Span<'static>>, bool)> = self
            .view
            .obs
            .sources_in(&scene)
            .map(|item| {
                let on = self.focus == Some(&Item::Source(item.source.clone()));
                let (mark, color) = if item.enabled {
                    ("━━●", OK)
                } else {
                    ("●━━", FAINT)
                };
                let bg = if on { SELECTED } else { Color::Reset };
                let text = if item.enabled { TEXT } else { MUTED };
                let mut name = Style::new().fg(text).bg(bg);
                if on {
                    name = name.bold();
                }
                (
                    vec![
                        Span::styled(format!(" {mark} "), Style::new().fg(color).bg(bg)),
                        Span::styled(format!("{} ", item.source), name),
                    ],
                    on,
                )
            })
            .collect();
        let head = Span::styled(
            format!(" ON AIR · {scene}   "),
            Style::new().fg(MUTED).bold(),
        );
        let (lines, focus) = pack(head, chips, usize::from(width));
        (lines, focus)
    }

    /// The scene on air: its sources one per line, each with its switch.
    fn on_air_list(&self, width: u16) -> Section {
        let scene = self.view.state.scene.clone().unwrap_or_default();
        if scene.is_empty() {
            return (Vec::new(), None);
        }
        let mut lines = vec![section(&format!("On air · {scene}"))];
        let mut focus = None;
        for item in self.view.obs.sources_in(&scene) {
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
            let on = self.focus == Some(&Item::Source(item.source.clone()));
            if on {
                focus = Some(lines.len());
            }
            lines.push(lit(line, on, width));
        }
        (lines, focus)
    }
}

/// Packs chips into rows of `width`, never breaking one; rows after the first are indented
/// under the first chip. Returns the rows and the row with the focused chip.
fn pack(head: Span<'static>, chips: Vec<(Vec<Span<'static>>, bool)>, width: usize) -> Section {
    let indent = head.width();
    let mut lines = Vec::new();
    let mut row = vec![head];
    let mut used = indent;
    let mut focus = None;
    for (chip, on) in chips {
        let w: usize = chip.iter().map(Span::width).sum::<usize>() + 1;
        if used > indent && used + w > width {
            lines.push(Line::from(std::mem::replace(
                &mut row,
                vec![Span::raw(" ".repeat(indent))],
            )));
            used = indent;
        }
        if on {
            focus = Some(lines.len());
        }
        row.extend(chip);
        row.push(Span::raw(" "));
        used += w;
    }
    lines.push(Line::from(row));
    (lines, focus)
}

/// A focused row: a bar in its first cell (keeping the rest of the indent) and a
/// highlight across the width.
fn lit(line: Line<'static>, on: bool, width: u16) -> Line<'static> {
    if !on {
        return line;
    }
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
    spans.push(Span::raw(
        " ".repeat(usize::from(width).saturating_sub(used)),
    ));
    Line::from(spans).style(Style::new().bg(SELECTED))
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
    let mut pairs = vec![("j/k", "move"), ("Tab", "next group"), ("Enter", "do it")];
    if view.config.tui.home_layout == HomeLayout::Tabs {
        pairs.push(("[ ]", "tab"));
    }
    pairs.extend([
        ("v", "layout"),
        ("q", quit),
        (",", "settings"),
        (":", "command"),
    ]);
    pairs
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
