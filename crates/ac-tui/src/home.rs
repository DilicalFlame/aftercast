//! The Home page (`0`, and where a session starts).
//!
//! The header holds the episode and, top right, the decision (Start or Resume, Quit). Below
//! it, cards you act on (rounded borders: Checks, Mixer, Sources) and, in the tabs layout,
//! information that is only read (no border: Status, Devices, Session). Tab moves between cards, j/k inside one,
//! Ctrl+j/k (or Ctrl+arrows) between scenes.
//!
//! A new session is a draft here: nothing lasting or public happens until it is started,
//! and quitting before that keeps nothing.

use std::time::Duration;

use ac_core::command::{Command, OutputAction};
use ac_core::config::HomeLayout;
use ac_core::event::{OutputState, ServiceStatus, StreamStatus};
use ac_core::obs::{Audio, Media};
use ac_core::time::{Timestamp, hms};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::theme::{ACCENT, BORDER, FAINT, INFO, MUTED, OK, RAISED, SELECTED, TEXT, WARN};
use crate::{Check, CheckState, KeysStatus, View};

/// Something on Home that j/k can reach and Enter acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A check aftercast can fix with this key (R, S, M).
    Fix(char),
    /// An audio input: Enter mutes or unmutes, h/l change the volume.
    Audio(String),
    /// A source in a scene: Enter shows or hides it.
    Source { scene: String, source: String },
}

/// A part of Home that takes the focus: Checks, Mixer, Sources. (Start and Quit have keys
/// of their own, so they never need the focus.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Checks,
    Mixer,
    Sources,
}

/// Where the focus is on Home: a card, an item in it, the open tab (`tabs`) and the scene
/// the Mixer and Sources show (`None`: the scene on air).
#[derive(Debug, Clone, PartialEq)]
pub struct Focus {
    pub card: Card,
    pub pick: usize,
    pub tab: usize,
    pub scene: Option<String>,
}

impl Default for Focus {
    fn default() -> Self {
        Self {
            card: Card::Checks,
            pick: 0,
            tab: 0,
            scene: None,
        }
    }
}

/// The tabs of the `tabs` layout.
pub const TABS: [&str; 3] = ["Checks", "Mixer", "Sources"];

/// The cards Tab moves between, in order.
pub fn cards(view: &View, tab: usize) -> Vec<Card> {
    match (view.config.tui.home_layout, tab) {
        (HomeLayout::Columns, _) => vec![Card::Checks, Card::Sources, Card::Mixer],
        (HomeLayout::Tabs, 0) => vec![Card::Checks],
        (HomeLayout::Tabs, 1) => vec![Card::Mixer],
        (HomeLayout::Tabs, _) => vec![Card::Sources],
    }
}

/// The number key that jumps to `card` (columns layout; tabs are numbered instead).
pub fn number(view: &View, card: Card) -> Option<usize> {
    cards(view, 0)
        .iter()
        .position(|c| *c == card)
        .map(|n| n + 1)
}

/// The scene the Mixer and Sources show: the chosen one if OBS still has it, else the
/// scene on air, else the first.
pub fn scene(view: &View, chosen: Option<&str>) -> String {
    let scenes = &view.obs.scenes;
    chosen
        .filter(|c| scenes.iter().any(|s| s == c))
        .map(str::to_owned)
        .or_else(|| view.state.scene.clone())
        .or_else(|| scenes.first().cloned())
        .unwrap_or_default()
}

/// What j/k reach in `card`.
pub fn items(view: &View, card: Card, scene: &str) -> Vec<Item> {
    match card {
        Card::Checks => sorted_checks(view)
            .into_iter()
            .filter_map(|c| fix_key(c).map(Item::Fix))
            .collect(),
        Card::Mixer => scene_audio(view, scene)
            .into_iter()
            .map(|a| Item::Audio(a.name.clone()))
            .collect(),
        Card::Sources => view
            .obs
            .sources_in(scene)
            .map(|i| Item::Source {
                scene: scene.to_owned(),
                source: i.source.clone(),
            })
            .collect(),
    }
}

/// The audio a scene uses: OBS's global devices (desktop audio, mics) and the scene's
/// own sources that have audio. Mics first.
pub fn scene_audio<'a>(view: &'a View, scene: &str) -> Vec<&'a Audio> {
    let obs = &view.obs;
    let mut audio: Vec<&Audio> = obs
        .audio
        .iter()
        .filter(|a| {
            let placed = obs.items.iter().any(|i| i.source == a.name);
            !placed
                || obs
                    .items
                    .iter()
                    .any(|i| i.scene == scene && i.source == a.name)
        })
        .collect();
    audio.sort_by_key(|a| !a.is_mic());
    audio
}

/// Go Live replaces Start while the broadcast is upcoming: it also starts OBS recording and
/// streaming and takes the broadcast live. (A rehearsal has no upcoming state before it starts.)
pub fn go_live(view: &View) -> bool {
    !view.begun && view.state.stream == Some(StreamStatus::Upcoming)
}

/// What `Enter` on an item asks, as a command for the session.
pub fn command(item: &Item, view: &View) -> Option<Command> {
    Some(match item {
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
        Item::Source { scene, source } => Command::Source {
            source: source.clone(),
            scene: Some(scene.clone()),
            visible: None,
        },
    })
}

/// What Enter does on `item`, in as few words as possible.
pub fn what(item: &Item, view: &View) -> String {
    match item {
        Item::Fix('R') => "record".into(),
        Item::Fix('S') => "stream".into(),
        Item::Fix(_) => "unmute".into(),
        Item::Audio(name) => match view.obs.audio(name) {
            Some(a) if a.muted => "unmute".into(),
            _ => "mute".into(),
        },
        Item::Source { scene, source } => match view.obs.item(scene, source) {
            Some(i) if i.enabled => "hide".into(),
            _ => "show".into(),
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

/// The shortest start of each scene's name (at least 3 characters) that no other scene's
/// name starts with, for tabs where every scene must fit.
pub fn short_names(scenes: &[String]) -> Vec<String> {
    scenes
        .iter()
        .map(|name| {
            let chars: Vec<char> = name.chars().collect();
            (3.min(chars.len())..=chars.len())
                .map(|n| chars[..n].iter().collect::<String>())
                .find(|prefix| {
                    let p = prefix.to_lowercase();
                    scenes
                        .iter()
                        .filter(|other| *other != name)
                        .all(|other| !other.to_lowercase().starts_with(&p))
                })
                .unwrap_or_else(|| name.clone())
        })
        .collect()
}

/// Draws the page over everything but the key hints.
pub fn draw(frame: &mut Frame, view: &View, now: Timestamp, spin: &'static str, focus: &Focus) {
    let area = frame.area();
    // A margin all round: two columns at the sides, one row above and below (a cell is
    // about twice as tall as wide), then the footer of key hints.
    let page = Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(3),
    };
    // Everything but the footer of key hints.
    frame.render_widget(
        Clear,
        Rect {
            height: area.height.saturating_sub(1),
            ..area
        },
    );
    let scene = scene(view, focus.scene.as_deref());
    let ctx = Ctx {
        view,
        focus,
        spin,
        now,
        scene,
    };
    let [head, _, body] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(1),
        Constraint::Min(3),
    ])
    .areas(page);
    // The decision sits over the right pane, flush with its edges.
    let right = match view.config.tui.home_layout {
        HomeLayout::Columns => halves(body)[1],
        HomeLayout::Tabs => split(body)[1],
    };
    ctx.header(frame, head, right.x);

    match view.config.tui.home_layout {
        HomeLayout::Columns => {
            // Checks on the left, YouTube side of the stream on the right.
            let [checks_pane, youtube_pane] = halves(body);
            let youtube = ctx.youtube(youtube_pane.width);
            let checks = ctx
                .checks(checks_pane.width.saturating_sub(4), false)
                .0
                .len() as u16
                + 2;
            let height = checks.max(youtube.len() as u16 + 1).min(body.height / 2);
            let [top, rest] =
                Layout::vertical([Constraint::Length(height), Constraint::Min(3)]).areas(body);
            let [left, right] = halves(rest);
            let [top_left, top_right] = halves(top);
            ctx.card(frame, Card::Checks, top_left, true);
            ctx.info(frame, top_right, "YouTube", youtube);
            ctx.card(frame, Card::Sources, left, true);
            ctx.card(frame, Card::Mixer, right, true);
        }
        HomeLayout::Tabs => {
            let [bar, _, rest] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(3),
            ])
            .areas(body);
            frame.render_widget(tab_bar(view, focus.tab), bar);
            let [left, right] = split(rest);
            match focus.tab {
                0 => {
                    ctx.card(frame, Card::Checks, left, false);
                    let youtube = ctx.youtube(right.width);
                    let status = ctx.status(right.width);
                    let devices = ctx.devices(right.width);
                    let session = ctx.session(right.width);
                    let [s0, _, s1, _, s2, _, s3] = Layout::vertical([
                        Constraint::Length(youtube.len() as u16 + 1),
                        Constraint::Length(1),
                        Constraint::Length(status.len() as u16 + 1),
                        Constraint::Length(1),
                        Constraint::Length(devices.len() as u16 + 1),
                        Constraint::Length(1),
                        Constraint::Fill(1),
                    ])
                    .areas(right);
                    ctx.info(frame, s0, "YouTube", youtube);
                    ctx.info(frame, s1, "Status", status);
                    ctx.info(frame, s2, "Devices", devices);
                    ctx.info(frame, s3, "Session", session);
                }
                tab => {
                    let card = if tab == 1 { Card::Mixer } else { Card::Sources };
                    ctx.card(frame, card, left, false);
                    ctx.scene_list(frame, right);
                }
            }
        }
    }
}

/// The `tabs` layout's two panes: the card, and what sits beside it.
fn split(area: Rect) -> [Rect; 2] {
    Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
        .spacing(2)
        .areas(area)
}

/// Two columns of `area`, 2 apart.
fn halves(area: Rect) -> [Rect; 2] {
    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .spacing(2)
        .areas(area)
}

/// The `tabs` layout's tab bar: the open tab lit, each with a count.
fn tab_bar(view: &View, tab: usize) -> Line<'static> {
    let open = view
        .preflight
        .iter()
        .filter(|c| c.state != CheckState::Ok)
        .count();
    // Only Checks carries a mark: what needs you, or that all pass.
    let marks = [
        if open == 0 {
            " ✔".to_owned()
        } else {
            format!(" ✖{open}")
        },
        String::new(),
        String::new(),
    ];
    let mut spans = Vec::new();
    for (i, name) in TABS.iter().enumerate() {
        let style = if i == tab {
            Style::new().fg(Color::Black).bg(ACCENT).bold()
        } else {
            Style::new().fg(MUTED).bg(RAISED)
        };
        spans.push(Span::styled(
            format!(" {} {name}{}  ", i + 1, marks[i]),
            style,
        ));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// A card's rows (already wrapped to its inner width) and the row holding the focus.
type Section = (Vec<Line<'static>>, Option<usize>);

/// How far to scroll `lines` (wrapped at `width`) so the line `focus` ends inside `height`
/// rows; 0 when nothing in the card is focused.
fn scroll_to(lines: &[Line], focus: Option<usize>, width: u16, height: u16) -> u16 {
    let Some(focus) = focus else {
        return 0;
    };
    let rows = |l: &Line| l.width().div_ceil(usize::from(width.max(1))).max(1);
    let end: usize = lines.iter().take(focus + 1).map(rows).sum();
    end.saturating_sub(usize::from(height)) as u16
}

/// What every part of Home needs to draw itself.
struct Ctx<'a> {
    view: &'a View,
    focus: &'a Focus,
    spin: &'static str,
    now: Timestamp,
    /// The scene the Mixer and Sources show.
    scene: String,
}

impl Ctx<'_> {
    /// The item under the focus in `card`, if that card has the focus.
    fn focused(&self, card: Card) -> Option<Item> {
        (self.focus.card == card)
            .then(|| {
                items(self.view, card, &self.scene)
                    .into_iter()
                    .nth(self.focus.pick)
            })
            .flatten()
    }

    /// Logo, episode, link; top right the decision: buttons, state and verdict.
    /// `pane` is where the right pane starts: the buttons column begins there.
    fn header(&self, frame: &mut Frame, area: Rect, pane: u16) {
        let view = self.view;
        let room = (area.x + area.width).saturating_sub(pane).max(40);
        let [logo, who, right] = Layout::horizontal([
            Constraint::Length(14),
            Constraint::Min(10),
            Constraint::Length(room),
        ])
        .areas(area);
        // A red play button, 11 by 4. The triangle's edges fall an eighth of a row at a time
        // (lower eighth blocks; under the middle, red on white), so they slope smoothly
        // with only Block Elements, which terminals draw themselves.
        let fill = Style::new().bg(ACCENT);
        let white = Style::new().fg(Color::White).bg(ACCENT);
        let under = Style::new().fg(ACCENT).bg(Color::White);
        let red = || Span::styled("           ", fill);
        let row = |cells: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled("    ", fill)];
            spans.extend(cells);
            spans.push(Span::styled("   ", fill));
            Line::from(spans)
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(red()),
                row(vec![Span::styled("█▅▃▁", white)]),
                row(vec![Span::styled("█", white), Span::styled("▃▅▇", under)]),
                Line::from(red()),
            ]),
            logo,
        );
        let title = view
            .meta
            .title_at_start
            .clone()
            .unwrap_or_else(|| format!("Episode {}", view.meta.episode));
        let tag = if view.begun {
            Span::styled(" ● STARTED ", Style::new().fg(Color::Black).bg(OK).bold())
        } else if view.fresh {
            Span::styled(" DRAFT ", Style::new().fg(Color::Black).bg(WARN).bold())
        } else {
            Span::styled(" RESUME ", Style::new().fg(Color::Black).bg(WARN).bold())
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("#{:03}  ", view.meta.episode),
                        Style::new().fg(ACCENT).bold(),
                    ),
                    Span::styled(title, Style::new().fg(TEXT).bold()),
                ]),
                Line::styled(
                    format!("youtu.be/{}", view.meta.video_id),
                    Style::new().fg(INFO),
                ),
                Line::styled(view.session.clone(), Style::new().fg(FAINT)),
                Line::from(tag),
            ]),
            who,
        );

        // The buttons: as tall as the logo, each with its key under the label. Keys work
        // from anywhere on Home, so the buttons never take the focus.
        let buttons: Vec<(&str, &str, Style)> = if view.begun {
            vec![("Dashboard", "esc", Style::new().fg(Color::Black).bg(ACCENT))]
        } else {
            let start = if go_live(view) {
                "Go Live"
            } else if view.fresh {
                "Start"
            } else {
                "Resume"
            };
            vec![
                (start, "s", Style::new().fg(Color::Black).bg(ACCENT)),
                ("Quit", "q", Style::new().fg(TEXT).bg(RAISED)),
            ]
        };
        let mut x = right.x + right.width;
        for (label, key, style) in buttons.into_iter().rev() {
            let width = (label.chars().count() as u16 + 8).max(12);
            x = x.saturating_sub(width);
            let area = Rect { x, width, ..right };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw(""),
                    Line::styled(label.to_owned(), style.bold()),
                    Line::styled(key.to_owned(), style.add_modifier(Modifier::DIM)),
                    Line::raw(""),
                ])
                .alignment(Alignment::Center)
                .style(style),
                area,
            );
            x = x.saturating_sub(2);
        }
    }

    /// Information that is only read: a small label and rows, no border.
    fn info(&self, frame: &mut Frame, area: Rect, label: &str, rows: Vec<Line<'static>>) {
        let mut lines = vec![Line::styled(
            label.to_uppercase(),
            Style::new().fg(FAINT).bold(),
        )];
        lines.extend(rows);
        frame.render_widget(Paragraph::new(lines), area);
    }

    /// A card you act on: a rounded border (lit when it has the focus), its title, the
    /// content scrolled to the focus, and in the bottom border what Enter does there.
    fn card(&self, frame: &mut Frame, card: Card, area: Rect, scene_tabs: bool) {
        let view = self.view;
        let lit = self.focus.card == card;
        let mut title = Vec::new();
        if let Some(n) = number(view, card).filter(|_| scene_tabs) {
            let chip = if lit {
                Style::new().fg(Color::Black).bg(ACCENT).bold()
            } else {
                Style::new().fg(MUTED).bg(RAISED)
            };
            title.push(Span::raw(" "));
            title.push(Span::styled(format!(" {n} "), chip));
        }
        title.push(Span::styled(
            match card {
                Card::Checks => " Checks ",
                Card::Mixer => " Mixer ",
                Card::Sources => " Sources ",
            },
            Style::new().fg(if lit { TEXT } else { MUTED }).bold(),
        ));
        if scene_tabs && matches!(card, Card::Mixer | Card::Sources) {
            title.extend(self.scene_tabs(area.width.saturating_sub(14)));
        }
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if lit { ACCENT } else { BORDER }))
            .title(Line::from(title))
            .padding(Padding::horizontal(1));
        if card == Card::Checks {
            let (ok, _, total) = counts(&view.preflight);
            block = block.title(
                Span::styled(format!(" ✔ {ok}/{total} "), Style::new().fg(OK))
                    .into_right_aligned_line(),
            );
        }
        let inner = block.inner(area);
        let (lines, focus) = match card {
            Card::Checks => self.checks(inner.width, !scene_tabs),
            Card::Mixer => self.mixer(inner.width),
            Card::Sources => self.sources(inner.width),
        };
        let scroll = scroll_to(&lines, focus, inner.width, inner.height);
        if let Some(item) = self.focused(card) {
            block = block.title_bottom(hint(&item, view));
        }
        if lines.len() > usize::from(scroll) + usize::from(inner.height) {
            block = block
                .title_bottom(Span::styled(" ↓ ", Style::new().fg(WARN)).into_right_aligned_line());
        }
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
    }

    /// Scenes as tabs for a card's top border: shortest unique names, the shown one lit,
    /// the one on air marked; a window around the shown one when they do not all fit.
    fn scene_tabs(&self, room: u16) -> Vec<Span<'static>> {
        let scenes = &self.view.obs.scenes;
        if scenes.is_empty() {
            return Vec::new();
        }
        let short = short_names(scenes);
        let on_air = self.view.state.scene.as_deref();
        let at = scenes.iter().position(|s| *s == self.scene).unwrap_or(0);
        let air = |i: usize| on_air == Some(scenes[i].as_str());
        let width = |i: usize| short[i].chars().count() + 2 + usize::from(air(i));
        // Widen the window around the shown scene, alternating sides, while it fits.
        let room = usize::from(room).saturating_sub(4);
        let (mut from, mut to) = (at, at);
        let mut used = width(at);
        loop {
            let mut grew = false;
            if to + 1 < scenes.len() && used + width(to + 1) <= room {
                to += 1;
                used += width(to);
                grew = true;
            }
            if from > 0 && used + width(from - 1) <= room {
                from -= 1;
                used += width(from);
                grew = true;
            }
            if !grew {
                break;
            }
        }
        let mut spans = vec![Span::styled(
            if from > 0 { "‹" } else { " " },
            Style::new().fg(FAINT),
        )];
        for (i, short) in short.iter().enumerate().take(to + 1).skip(from) {
            let name = format!(" {}{short} ", if air(i) { "●" } else { "" });
            let style = if i == at {
                Style::new().fg(Color::Black).bg(ACCENT).bold()
            } else if air(i) {
                Style::new().fg(ACCENT)
            } else {
                Style::new().fg(MUTED)
            };
            spans.push(Span::styled(name, style));
        }
        spans.push(Span::styled(
            if to + 1 < scenes.len() { "› " } else { " " },
            Style::new().fg(FAINT),
        ));
        spans
    }

    /// Every scene, one per line (the `tabs` layout): the shown one lit, the one on air
    /// marked. Ctrl+j/k move through them.
    fn scene_list(&self, frame: &mut Frame, area: Rect) {
        let on_air = self.view.state.scene.as_deref();
        let width = area.width.saturating_sub(2);
        let mut lines = Vec::new();
        let mut focus = None;
        for scene in &self.view.obs.scenes {
            let air = on_air == Some(scene.as_str());
            let shown = *scene == self.scene;
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled(if air { "● " } else { "  " }, Style::new().fg(ACCENT)),
                Span::styled(
                    scene.clone(),
                    Style::new().fg(if shown || air { TEXT } else { MUTED }),
                ),
            ]);
            if shown {
                focus = Some(lines.len());
            }
            lines.push(lit(line, shown, width));
        }
        if lines.is_empty() {
            lines.push(Line::styled(" OBS not connected", Style::new().fg(FAINT)));
        }
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(BORDER))
            .title(Span::styled(" Scenes ", Style::new().fg(MUTED).bold()))
            .title_bottom(Span::styled(" ^j/k ", Style::new().fg(MUTED)));
        let inner = block.inner(area);
        let scroll = scroll_to(&lines, focus, inner.width, inner.height);
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
    }

    /// Checks that are not fine, each with its fix; with `passing`, the fine ones after.
    fn checks(&self, width: u16, passing: bool) -> Section {
        let w = usize::from(width);
        let checks = sorted_checks(self.view);
        let open: Vec<&&Check> = checks
            .iter()
            .filter(|c| c.state != CheckState::Ok)
            .collect();
        let mut lines = Vec::new();
        let mut focus = None;
        let lit_item = self.focused(Card::Checks);
        if open.is_empty() && !passing {
            lines.push(Line::styled("✔ all pass", Style::new().fg(OK)));
        }
        for c in open {
            let (mark, style) = match c.state {
                CheckState::Fail => ("✖", Style::new().fg(ACCENT).bold()),
                _ => (self.spin, Style::new().fg(MUTED)),
            };
            let key = fix_key(c);
            let on = key.is_some() && lit_item == key.map(Item::Fix);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(format!("{mark} "), style),
                Span::styled(c.label.clone(), Style::new().fg(TEXT).bold()),
            ];
            if !c.detail.is_empty() {
                spans.push(Span::styled(
                    format!("  {}", c.detail),
                    Style::new().fg(FAINT),
                ));
            }
            let mut rows = crate::wrap(Line::from(spans), w, 3);
            if !c.fix.is_empty() {
                let mut fix = vec![Span::raw("   ")];
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
        if passing {
            for c in checks.iter().filter(|c| c.state == CheckState::Ok) {
                let mut spans = vec![
                    Span::styled(" ✔ ", Style::new().fg(OK)),
                    Span::styled(c.label.clone(), Style::new().fg(MUTED)),
                ];
                if !c.detail.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", c.detail),
                        Style::new().fg(FAINT),
                    ));
                }
                lines.push(crate::truncate(Line::from(spans), w));
            }
        }
        (lines, focus)
    }

    /// The shown scene's audio: a meter, level, volume and mute per input.
    fn mixer(&self, width: u16) -> Section {
        let audio = scene_audio(self.view, &self.scene);
        if audio.is_empty() {
            return (vec![Line::styled("no audio", Style::new().fg(FAINT))], None);
        }
        let lit_item = self.focused(Card::Mixer);
        let mut lines = Vec::new();
        let mut focus = None;
        for (name, line) in self.meters(&audio, width) {
            let on = lit_item == Some(Item::Audio(name));
            if on {
                focus = Some(lines.len());
            }
            lines.push(lit(line, on, width));
        }
        (lines, focus)
    }

    /// Meter rows for `audio`, each with its input's name.
    fn meters(&self, audio: &[&Audio], width: u16) -> Vec<(String, Line<'static>)> {
        let levels = self
            .view
            .levels
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        let name_width = audio
            .iter()
            .map(|a| a.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(22);
        // " name  " + "▕bar▏" + " -38dB" + "  -12" + "  muted": the meter gives way first.
        let bar = usize::from(width)
            .saturating_sub(name_width + 23)
            .clamp(4, 24);
        audio
            .iter()
            .map(|a| {
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
                        _ => "      ".to_owned(),
                    },
                    Style::new().fg(MUTED),
                ));
                spans.push(Span::styled(
                    format!("  {:+.0}", a.volume_db),
                    Style::new().fg(FAINT),
                ));
                if a.muted {
                    spans.push(Span::styled("  muted", Style::new().fg(ACCENT).bold()));
                }
                (a.name.clone(), Line::from(spans))
            })
            .collect()
    }

    /// The shown scene's sources as switches.
    fn sources(&self, width: u16) -> Section {
        let lit_item = self.focused(Card::Sources);
        let mut lines = Vec::new();
        let mut focus = None;
        for item in self.view.obs.sources_in(&self.scene) {
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
            let on = lit_item
                == Some(Item::Source {
                    scene: self.scene.clone(),
                    source: item.source.clone(),
                });
            if on {
                focus = Some(lines.len());
            }
            lines.push(lit(line, on, width));
        }
        if lines.is_empty() {
            lines.push(Line::styled("no sources", Style::new().fg(FAINT)));
        }
        (lines, focus)
    }

    /// YouTube side: the broadcast state and visibility, whether video arrives and how
    /// healthy it is, and what YouTube flags.
    fn youtube(&self, width: u16) -> Vec<Line<'static>> {
        let row = |label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(
                format!(" {label:<11}"),
                Style::new().fg(MUTED),
            )];
            spans.extend(value);
            crate::truncate(Line::from(spans), usize::from(width))
        };
        let dot = |color: Color, text: String| Span::styled(text, Style::new().fg(color));
        let Some(yt) = &self.view.youtube else {
            return vec![row(
                "Broadcast",
                vec![dot(MUTED, format!("{} asking", self.spin))],
            )];
        };
        let broadcast = match yt.life_cycle.as_str() {
            "live" | "liveStarting" => dot(ACCENT, "● live".into()),
            "testing" | "testStarting" => dot(INFO, "● testing".into()),
            "ready" => dot(OK, "● ready".into()),
            "created" => dot(WARN, "○ created, no stream bound".into()),
            "complete" => dot(MUTED, "■ ended".into()),
            other => dot(MUTED, other.to_owned()),
        };
        let mut lines = vec![row("Broadcast", vec![broadcast])];
        if let Some(p) = &yt.privacy {
            let color = match p.as_str() {
                "public" => OK,
                "unlisted" => INFO,
                _ => WARN,
            };
            lines.push(row("Visibility", vec![dot(color, p.clone())]));
        }
        if let Some(start) = yt.scheduled_start
            && !matches!(yt.life_cycle.as_str(), "live" | "complete")
        {
            let mins = (start.secs_since(self.now) / 60.0).round() as i64;
            let at = start
                .datetime()
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            let when = match mins {
                m if m > 0 => format!("{at}  in {m} min"),
                0 => format!("{at}  now"),
                m => format!("{at}  {} min ago", -m),
            };
            lines.push(row("Scheduled", vec![dot(TEXT, when)]));
        }
        let ingest = match (yt.stream.as_deref(), yt.health.as_deref()) {
            (None, _) => dot(WARN, "no stream bound".into()),
            (Some("active"), Some("good")) => dot(OK, "● receiving · good".into()),
            (Some("active"), Some("ok")) => dot(OK, "● receiving · ok".into()),
            (Some("active"), Some("bad")) => dot(ACCENT, "✖ receiving · bad".into()),
            (Some("active"), _) => dot(WARN, "● receiving · no data yet".into()),
            _ => dot(MUTED, "○ waiting for video".into()),
        };
        lines.push(row("Ingest", vec![ingest]));
        let video: Vec<&str> = [yt.resolution.as_deref(), yt.frame_rate.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if !video.is_empty() {
            lines.push(row("Video", vec![dot(TEXT, video.join(" · "))]));
        }
        let mut options = Vec::new();
        if let Some(l) = &yt.latency {
            options.push(format!("{l} latency"));
        }
        if let Some(dvr) = yt.dvr {
            options.push(format!("DVR {}", if dvr { "on" } else { "off" }));
        }
        if !options.is_empty() {
            lines.push(row("Options", vec![dot(TEXT, options.join(" · "))]));
        }
        let auto: Vec<String> = [("start", yt.auto_start), ("stop", yt.auto_stop)]
            .into_iter()
            .filter_map(|(name, on)| Some(format!("{name} {}", if on? { "on" } else { "off" })))
            .collect();
        if !auto.is_empty() {
            lines.push(row("Auto", vec![dot(TEXT, auto.join(" · "))]));
        }
        for issue in &yt.issues {
            lines.push(crate::truncate(
                Line::from(vec![
                    Span::styled(" ⚠ ", Style::new().fg(WARN)),
                    Span::styled(issue.clone(), Style::new().fg(WARN)),
                ]),
                usize::from(width),
            ));
        }
        lines
    }

    /// What is live: stream, OBS, services, keys, quota and latency.
    fn status(&self, width: u16) -> Vec<Line<'static>> {
        let view = self.view;
        let state = &view.state;
        let anchors = &view.meta.anchors;
        let service = |name: &str| match state.services.get(name) {
            Some(ServiceStatus::Ok) => Span::styled("●", Style::new().fg(OK)),
            Some(ServiceStatus::Retrying) => Span::styled(self.spin, Style::new().fg(WARN)),
            Some(ServiceStatus::Down) => Span::styled("✖", Style::new().fg(ACCENT).bold()),
            None => Span::styled(self.spin, Style::new().fg(MUTED)),
        };
        let stream = match state.stream {
            Some(StreamStatus::Live) if !state.ended => {
                let since = anchors
                    .yt_actual_start
                    .map(|s| hms(anchors.yt_actual_end.unwrap_or(self.now).secs_since(s)))
                    .unwrap_or_default();
                Span::styled(format!("● LIVE {since}"), Style::new().fg(ACCENT).bold())
            }
            Some(StreamStatus::Upcoming) => Span::styled("○ upcoming", Style::new().fg(WARN)),
            _ if state.ended => Span::styled("■ ended", Style::new().fg(MUTED)),
            _ => Span::styled("○ offline", Style::new().fg(MUTED)),
        };
        let obs = if state.services.get("obs") == Some(&ServiceStatus::Ok) {
            let rec = match state.obs_record {
                Some(OutputState::Started | OutputState::Resumed) => {
                    Span::styled("  ● REC", Style::new().fg(ACCENT).bold())
                }
                Some(OutputState::Paused) => Span::styled("  ‖ REC", Style::new().fg(WARN)),
                _ => Span::styled("  ○ REC", Style::new().fg(FAINT)),
            };
            vec![
                Span::styled(
                    state.scene.clone().unwrap_or_else(|| "?".into()),
                    Style::new().fg(TEXT),
                ),
                rec,
            ]
        } else {
            vec![service("obs")]
        };
        let keys = match view.keys {
            KeysStatus::Off => Span::styled("off", Style::new().fg(FAINT)),
            KeysStatus::Error => Span::styled("✖", Style::new().fg(ACCENT)),
            _ if state.privacy => Span::styled("paused", Style::new().fg(WARN)),
            KeysStatus::Starting => Span::styled(self.spin, Style::new().fg(MUTED)),
            KeysStatus::On => {
                Span::styled(crate::sparkline(&view.keys_per_min), Style::new().fg(INFO))
            }
        };
        let latency = if view.latency_running {
            Span::styled(self.spin, Style::new().fg(WARN))
        } else {
            Span::styled(format!("{:.1}s", view.latency_s), Style::new().fg(TEXT))
        };
        let row = |label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(format!(" {label:<9}"), Style::new().fg(MUTED))];
            spans.extend(value);
            crate::truncate(Line::from(spans), usize::from(width))
        };
        let mut lines = vec![
            row("Stream", vec![stream]),
            row("OBS", obs),
            row(
                "YouTube",
                vec![
                    service("youtube"),
                    Span::styled("   chat ", Style::new().fg(MUTED)),
                    service("chat"),
                ],
            ),
            row("Keys", vec![keys]),
            row(
                "Quota",
                vec![
                    Span::styled(
                        crate::thousands(view.quota_remaining),
                        Style::new().fg(TEXT),
                    ),
                    Span::styled("   lat ", Style::new().fg(MUTED)),
                    latency,
                ],
            ),
        ];
        let modes: Vec<&str> = [(state.brb, "BRB"), (state.privacy, "PRIVACY")]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
        if !modes.is_empty() {
            lines.push(row(
                "Mode",
                vec![Span::styled(
                    modes.join(" · "),
                    Style::new().fg(WARN).bold(),
                )],
            ));
        }
        lines
    }

    /// The mics' and cameras' hardware, as OBS names it.
    fn devices(&self, width: u16) -> Vec<Line<'static>> {
        let mut devices: Vec<_> = self.view.obs.devices.iter().collect();
        devices.sort_by_key(|d| d.video);
        if devices.is_empty() {
            return vec![Line::styled(" ·", Style::new().fg(FAINT))];
        }
        devices
            .into_iter()
            .map(|d| {
                let line = Line::from(vec![
                    Span::styled(
                        format!(" {:<9}", if d.video { "Camera" } else { "Mic" }),
                        Style::new().fg(MUTED),
                    ),
                    Span::styled(d.name.clone(), Style::new().fg(TEXT)),
                ]);
                crate::truncate(line, usize::from(width))
            })
            .collect()
    }

    /// Description size, chapters, last push, overlay address.
    fn session(&self, width: u16) -> Vec<Line<'static>> {
        let view = self.view;
        let row = |label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(format!(" {label:<9}"), Style::new().fg(MUTED))];
            spans.extend(value);
            crate::truncate(Line::from(spans), usize::from(width))
        };
        let readme = match (&view.render_error, &view.description) {
            (Some(e), _) => vec![Span::styled(format!("✖ {e}"), Style::new().fg(ACCENT))],
            (None, Some(d)) => {
                let (bar, color) = crate::gauge(d.chars(), crate::DESCRIPTION_LIMIT, 10);
                vec![
                    Span::styled(bar, Style::new().fg(color)),
                    Span::styled(
                        format!(" {}", crate::thousands(d.chars() as i64)),
                        Style::new().fg(TEXT),
                    ),
                ]
            }
            (None, None) => vec![Span::styled("·", Style::new().fg(FAINT))],
        };
        let published = if ac_template::chapters::block(&view.chapters).is_empty() {
            0
        } else {
            view.chapters.len()
        };
        let push = match view.state.last_push {
            Some((t, _)) => t
                .datetime()
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string(),
            None => "·".into(),
        };
        let overlays = view
            .preflight
            .iter()
            .find(|c| c.label == "Overlay server listening" && c.state == CheckState::Ok)
            .map_or("·".to_owned(), |c| {
                c.detail
                    .trim_start_matches("http://")
                    .trim_end_matches("/overlay/<name>")
                    .to_owned()
            });
        vec![
            row("README", readme),
            row(
                "Chapters",
                vec![
                    Span::styled(
                        view.state.checkpoints.len().to_string(),
                        Style::new().fg(TEXT),
                    ),
                    Span::styled(format!("  {published} live"), Style::new().fg(MUTED)),
                ],
            ),
            row("Pushed", vec![Span::styled(push, Style::new().fg(TEXT))]),
            row(
                "Overlays",
                vec![Span::styled(overlays, Style::new().fg(INFO))],
            ),
        ]
    }
}

/// What Enter (and h/l) do on the focused item, faded in its card's bottom border.
fn hint(item: &Item, view: &View) -> Line<'static> {
    let mut spans = vec![
        Span::styled(" ⏎ ", Style::new().fg(MUTED)),
        Span::styled(what(item, view), Style::new().fg(FAINT)),
    ];
    let extra = match item {
        Item::Audio(_) => Some("h/l vol"),
        Item::Source { .. } => Some("h/l"),
        _ => None,
    };
    if let Some(extra) = extra {
        spans.push(Span::styled(format!(" · {extra}"), Style::new().fg(FAINT)));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
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

/// Checks that are fine, failing, and all.
fn counts(checks: &[Check]) -> (usize, usize, usize) {
    let of = |s| checks.iter().filter(|c| c.state == s).count();
    (of(CheckState::Ok), of(CheckState::Fail), checks.len())
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

/// Footer hints: the same keys whatever is focused, so the line never shifts.
pub fn hints() -> Vec<(String, String)> {
    [
        ("1-3", "go"),
        ("Tab", "card"),
        ("j/k", "move"),
        ("⏎", "do"),
        ("^j/k", "scene"),
        ("v", "layout"),
        (",", "settings"),
        (":", "cmd"),
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
        let mut wide = lines.clone();
        wide[0] = Line::raw("x".repeat(30));
        assert_eq!(scroll_to(&wide, Some(3), 20, 4), 1);
    }

    #[test]
    fn scene_names_are_cut_to_their_shortest_unique_start() {
        let scenes: Vec<String> = [
            "Code",
            "BRB",
            "BRB_Video",
            "Starting Soon",
            "Stream Ending",
            "Ok",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            short_names(&scenes),
            ["Cod", "BRB", "BRB_", "Sta", "Str", "Ok"]
        );
    }
}
