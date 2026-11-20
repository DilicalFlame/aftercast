//! The Home page (`0`, and where a session starts).
//!
//! Two areas: the primary card (Start or Resume, Quit, and whether everything is ready)
//! and the cards below it: Checks (what needs you, each with its fix), Status (what is
//! live), Audio (a meter per input) and On air (the scene's sources as switches). The
//! cards are arranged as `tui.home_layout` says. Everything that can be acted on is
//! reached with j/k (Tab jumps card to card) and acted on with Enter.
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
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use crate::theme::{ACCENT, FAINT, INFO, MUTED, OK, RAISED, SELECTED, TEXT, WARN};
use crate::{Check, CheckState, KeysStatus, View};

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

/// The cards an item lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    Primary,
    Checks,
    Status,
    Audio,
    Scene,
}

impl Item {
    pub fn card(&self) -> Card {
        match self {
            Item::Start | Item::Quit | Item::Dashboard => Card::Primary,
            Item::Fix(_) => Card::Checks,
            Item::Audio(_) => Card::Audio,
            Item::Source(_) => Card::Scene,
        }
    }
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
pub const TABS: [&str; 3] = ["Checks", "Audio", "On air"];

/// The cards in reading order for the layout (and, for `tabs`, the open tab).
fn cards(view: &View, tab: usize) -> Vec<Card> {
    match view.config.tui.home_layout {
        // Top row Checks, Status; bottom row On air, Audio.
        HomeLayout::Columns => vec![Card::Checks, Card::Status, Card::Scene, Card::Audio],
        HomeLayout::Tabs => match tab {
            0 => vec![Card::Checks, Card::Status],
            1 => vec![Card::Audio],
            _ => vec![Card::Scene],
        },
    }
}

/// Everything j/k can reach, in reading order: the primary card's buttons, then each
/// card's items. In the `tabs` layout only the open tab's.
pub fn items(view: &View, tab: usize) -> Vec<Item> {
    let mut items = if view.begun {
        vec![Item::Dashboard]
    } else {
        vec![Item::Start, Item::Quit]
    };
    for card in cards(view, tab) {
        match card {
            Card::Checks => items.extend(
                sorted_checks(view)
                    .into_iter()
                    .filter_map(|c| fix_key(c).map(Item::Fix)),
            ),
            Card::Audio => items.extend(
                sorted_audio(view)
                    .into_iter()
                    .map(|a| Item::Audio(a.name.clone())),
            ),
            Card::Scene => {
                let scene = view.state.scene.as_deref().unwrap_or_default();
                items.extend(
                    view.obs
                        .sources_in(scene)
                        .map(|i| Item::Source(i.source.clone())),
                );
            }
            Card::Primary | Card::Status => {}
        }
    }
    items
}

/// The first item of the next card after `pick` (Tab), wrapping to the top.
pub fn next_group(items: &[Item], pick: usize) -> usize {
    let now = items.get(pick).map(Item::card);
    items
        .iter()
        .enumerate()
        .skip(pick)
        .find(|(_, i)| Some(i.card()) != now)
        .map_or(0, |(n, _)| n)
}

/// What Enter does on `item`, for the focused card's border.
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

/// Draws the page over everything but the key hints. `now` is for the live timer, `spin`
/// a spinner frame, `pick` the focused item and `tab` the open tab (`tabs` layout).
pub fn draw(
    frame: &mut Frame,
    view: &View,
    now: Timestamp,
    spin: &'static str,
    pick: usize,
    tab: usize,
) {
    let items = items(view, tab);
    let focus = items.get(pick).cloned();
    let focused = focus.as_ref().map(Item::card);
    let area = frame.area();
    let page = Rect {
        height: area.height.saturating_sub(1),
        ..area
    };
    frame.render_widget(Clear, page);
    let ctx = Ctx {
        view,
        focus: focus.as_ref(),
        spin,
        now,
    };

    let [head, primary, body] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Min(3),
    ])
    .areas(Rect {
        x: page.x + 1,
        width: page.width.saturating_sub(2),
        ..page
    });
    ctx.header(frame, head);
    ctx.primary(frame, primary, focused == Some(Card::Primary));

    // The cards, each sized to its content where the layout allows it.
    let rows =
        |card: Card, width: u16| ctx.content(card, width.saturating_sub(4)).0.len() as u16 + 2;
    let place = |frame: &mut Frame, card: Card, area: Rect| {
        ctx.card(frame, card, area, focused == Some(card));
    };
    match view.config.tui.home_layout {
        HomeLayout::Columns => {
            let [left, right] = halves(body);
            let top = rows(Card::Checks, left.width)
                .max(rows(Card::Status, right.width))
                .min(body.height / 2 + 2);
            let [tl, bl] =
                Layout::vertical([Constraint::Length(top), Constraint::Min(3)]).areas(left);
            let [tr, br] =
                Layout::vertical([Constraint::Length(top), Constraint::Min(3)]).areas(right);
            place(frame, Card::Checks, tl);
            place(frame, Card::Status, tr);
            place(frame, Card::Scene, bl);
            place(frame, Card::Audio, br);
        }
        HomeLayout::Tabs => {
            let [bar, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(body);
            frame.render_widget(tab_bar(view, tab), bar);
            let order = cards(view, tab);
            if order.len() == 2 {
                // Checks beside Status.
                let [l, r] = halves(rest);
                place(frame, order[0], l);
                place(frame, order[1], r);
            } else {
                place(frame, order[0], rest);
            }
        }
    }
}

/// Two columns of `area`, 1 apart.
fn halves(area: Rect) -> [Rect; 2] {
    let [l, r] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .spacing(1)
        .areas(area);
    [l, r]
}

/// The `tabs` layout's tab bar: the open tab lit, each with what it holds.
fn tab_bar(view: &View, tab: usize) -> Line<'static> {
    let open = view
        .preflight
        .iter()
        .filter(|c| c.state != CheckState::Ok)
        .count();
    let scene = view.state.scene.as_deref().unwrap_or_default();
    let counts = [
        if open == 0 {
            "all pass".to_owned()
        } else {
            format!("{open} need you")
        },
        format!("{}", view.obs.audio.len()),
        format!("{}", view.obs.sources_in(scene).count()),
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

/// What every card needs to draw itself.
struct Ctx<'a> {
    view: &'a View,
    focus: Option<&'a Item>,
    spin: &'static str,
    now: Timestamp,
}

impl Ctx<'_> {
    /// The logo beside the episode, its link and the session folder.
    fn header(&self, frame: &mut Frame, area: Rect) {
        let view = self.view;
        let [logo, who] =
            Layout::horizontal([Constraint::Length(11), Constraint::Min(10)]).areas(area);
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
        let state = if !view.begun {
            Span::styled(" DRAFT ", Style::new().fg(Color::Black).bg(WARN).bold())
        } else {
            Span::styled(" STARTED ", Style::new().fg(Color::Black).bg(OK).bold())
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
                Line::from(vec![
                    Span::styled(
                        format!("youtu.be/{}", view.meta.video_id),
                        Style::new().fg(INFO),
                    ),
                    Span::styled(format!("   {}", view.session), Style::new().fg(FAINT)),
                ]),
                Line::from(vec![state]),
            ]),
            who,
        );
    }

    /// The primary card: the decision, kept apart from everything else.
    fn primary(&self, frame: &mut Frame, area: Rect, lit: bool) {
        let view = self.view;
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
            let style = if self.focus == Some(&item) {
                Style::new().fg(Color::Black).bg(color).bold()
            } else {
                Style::new().fg(TEXT).bg(RAISED)
            };
            Span::styled(format!("  {label}  "), style)
        };
        let (title, mut buttons, explain) = if view.begun {
            (
                " Session ",
                vec![button("Dashboard", Item::Dashboard, ACCENT)],
                "Started. 0 brings you back here from the dashboard.",
            )
        } else if view.fresh {
            (
                " Start ",
                vec![
                    button(
                        "Start the session",
                        Item::Start,
                        if ready { OK } else { WARN },
                    ),
                    Span::raw("  "),
                    button("Quit, keep nothing", Item::Quit, ACCENT),
                ],
                "Nothing is kept until you start (session folder, description push, key \
                 capture, bot).",
            )
        } else {
            (
                " Resume ",
                vec![
                    button(
                        "Resume the session",
                        Item::Start,
                        if ready { OK } else { WARN },
                    ),
                    Span::raw("  "),
                    button("Quit", Item::Quit, ACCENT),
                ],
                "This session already exists; resuming continues it where it stopped.",
            )
        };
        buttons.push(Span::raw("    "));
        buttons.push(verdict);
        let border = match (lit, view.begun, ready) {
            (true, ..) => ACCENT,
            (false, true, _) => RAISED,
            (false, false, true) => OK,
            (false, false, false) => WARN,
        };
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .title(Span::styled(title, Style::new().fg(TEXT).bold()))
            .padding(Padding::horizontal(1));
        if lit && let Some(item) = self.focus {
            block = block.title_bottom(hint(item, view));
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(buttons),
                crate::truncate(
                    Line::styled(explain, Style::new().fg(MUTED)),
                    usize::from(inner.width),
                ),
            ]),
            inner,
        );
    }

    /// A card: rounded border (lit when it holds the focus), its title and count, the
    /// content scrolled to the focus, what Enter does and `↓ more` in the bottom border.
    fn card(&self, frame: &mut Frame, card: Card, area: Rect, lit: bool) {
        let view = self.view;
        let (title, right) = match card {
            Card::Checks => {
                let (ok, _, total) = counts(&view.preflight);
                (
                    " Checks ".to_owned(),
                    Some(Span::styled(
                        format!(" ✔ {ok} of {total} pass "),
                        Style::new().fg(OK),
                    )),
                )
            }
            Card::Status => (" Status ".into(), None),
            Card::Audio => (
                " Audio ".into(),
                Some(Span::styled(
                    format!(" {} ", view.obs.audio.len()),
                    Style::new().fg(MUTED),
                )),
            ),
            Card::Scene => {
                let scene = view.state.scene.clone().unwrap_or_default();
                (
                    if scene.is_empty() {
                        " On air ".into()
                    } else {
                        format!(" On air · {scene} ")
                    },
                    None,
                )
            }
            Card::Primary => (String::new(), None),
        };
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(if lit { ACCENT } else { RAISED }))
            .title(Span::styled(
                title,
                Style::new().fg(if lit { TEXT } else { MUTED }).bold(),
            ))
            .padding(Padding::horizontal(1));
        if let Some(right) = right {
            block = block.title(right.into_right_aligned_line());
        }
        let inner = block.inner(area);
        let (lines, focus) = self.content(card, inner.width);
        let scroll = scroll_to(&lines, focus, inner.width, inner.height);
        let above = scroll > 0;
        let below = lines.len() > usize::from(scroll) + usize::from(inner.height);
        if lit && let Some(item) = self.focus {
            block = block.title_bottom(hint(item, view));
        }
        let arrows = match (above, below) {
            (true, true) => " ↑↓ more ",
            (true, false) => " ↑ more ",
            (false, true) => " ↓ more ",
            (false, false) => "",
        };
        if !arrows.is_empty() {
            block = block.title_bottom(
                Span::styled(arrows, Style::new().fg(WARN)).into_right_aligned_line(),
            );
        }
        frame.render_widget(block, area);
        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
    }

    fn content(&self, card: Card, width: u16) -> Section {
        match card {
            Card::Checks => self.checks(width),
            Card::Status => (self.status(width), None),
            Card::Audio => self.audio(width),
            Card::Scene => self.scene(width),
            Card::Primary => (Vec::new(), None),
        }
    }

    /// Checks that are not fine yet, each with how to fix it, wrapped under its text.
    /// Passing checks are only counted (in the card's title).
    fn checks(&self, width: u16) -> Section {
        let w = usize::from(width);
        let open: Vec<&Check> = sorted_checks(self.view)
            .into_iter()
            .filter(|c| c.state != CheckState::Ok)
            .collect();
        if open.is_empty() {
            return (
                vec![Line::styled("✔ Everything passes.", Style::new().fg(OK))],
                None,
            );
        }
        let mut lines = Vec::new();
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
        (lines, focus)
    }

    /// What is live, one thing per row: stream, OBS, services, keys, quota and latency.
    fn status(&self, width: u16) -> Vec<Line<'static>> {
        let view = self.view;
        let state = &view.state;
        let anchors = &view.meta.anchors;
        let service = |name: &str| match state.services.get(name) {
            Some(ServiceStatus::Ok) => Span::styled("● ok", Style::new().fg(OK)),
            Some(ServiceStatus::Retrying) => {
                Span::styled(format!("{} retrying", self.spin), Style::new().fg(WARN))
            }
            Some(ServiceStatus::Down) => Span::styled("✖ down", Style::new().fg(ACCENT).bold()),
            None => Span::styled(format!("{} …", self.spin), Style::new().fg(MUTED)),
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
                    Span::styled(" · ● REC", Style::new().fg(ACCENT).bold())
                }
                Some(OutputState::Paused) => Span::styled(" · ‖ paused", Style::new().fg(WARN)),
                _ => Span::styled(" · not recording", Style::new().fg(WARN)),
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
            KeysStatus::Error => Span::styled("✖ error", Style::new().fg(ACCENT)),
            _ if state.privacy => Span::styled("paused (privacy)", Style::new().fg(WARN)),
            KeysStatus::Starting => Span::styled(self.spin, Style::new().fg(MUTED)),
            KeysStatus::On => {
                Span::styled(crate::sparkline(&view.keys_per_min), Style::new().fg(INFO))
            }
        };
        let latency = if view.latency_running {
            Span::styled(format!("{} measuring", self.spin), Style::new().fg(WARN))
        } else {
            Span::styled(format!("{:.1} s", view.latency_s), Style::new().fg(TEXT))
        };
        let row = |label: &str, value: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(format!("{label:<9}"), Style::new().fg(MUTED))];
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
                    Span::styled("   latency ", Style::new().fg(MUTED)),
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

    /// A meter per audio input, mics first, with level, volume and mute.
    fn audio(&self, width: u16) -> Section {
        let levels = self
            .view
            .levels
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default();
        let audio = sorted_audio(self.view);
        if audio.is_empty() {
            return (
                vec![Line::styled(
                    "OBS is not connected, or has no audio inputs.",
                    Style::new().fg(FAINT).italic(),
                )],
                None,
            );
        }
        let name_width = audio
            .iter()
            .map(|a| a.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(16);
        // " name  " + "▕bar▏" + " -38dB" + "  vol-12" + "  muted": the meter gives way
        // first, so the row fits narrow cards too.
        let bar = usize::from(width)
            .saturating_sub(name_width + 26)
            .clamp(4, 24);
        let mut lines = Vec::new();
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

    /// The scene on air: its sources one per line, each with its switch.
    fn scene(&self, width: u16) -> Section {
        let scene = self.view.state.scene.clone().unwrap_or_default();
        let sources: Vec<_> = self.view.obs.sources_in(&scene).collect();
        if sources.is_empty() {
            let why = if scene.is_empty() {
                "OBS is not connected."
            } else {
                "This scene has no sources."
            };
            return (
                vec![Line::styled(why, Style::new().fg(FAINT).italic())],
                None,
            );
        }
        let mut lines = Vec::new();
        let mut focus = None;
        for item in sources {
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
                Span::styled(
                    if item.enabled { "" } else { "  hidden" },
                    Style::new().fg(FAINT),
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

/// What Enter (and h/l) do on the focused item, faded in its card's bottom border.
fn hint(item: &Item, view: &View) -> Line<'static> {
    let mut pairs = vec![("Enter".to_owned(), what(item, view))];
    match item {
        Item::Audio(_) => pairs.push(("h/l".into(), "volume".into())),
        Item::Source(_) => pairs.push(("h/l".into(), "hide/show".into())),
        Item::Start | Item::Quit => pairs.push(("h/l".into(), "switch".into())),
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

/// Footer hints: the same keys whatever is focused, so the line never shifts (what Enter
/// does on the focused item is in its card's bottom border).
pub fn hints(view: &View) -> Vec<(String, String)> {
    let quit = match (view.begun, view.fresh) {
        (false, true) => "quit, keep nothing",
        _ => "quit",
    };
    let mut pairs = vec![("j/k", "move"), ("Tab", "next card"), ("Enter", "do it")];
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
    fn tab_jumps_to_the_next_card() {
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
