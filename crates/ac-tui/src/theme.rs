//! Colors, per-user name colors and the small animations the TUI uses.

use std::collections::HashMap;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

pub const ACCENT: Color = Color::Rgb(0xFF, 0x55, 0x55);
/// The accent at low brightness: pulses fade towards it.
pub const ACCENT_DIM: Color = Color::Rgb(0x5A, 0x22, 0x22);
pub const MUTED: Color = Color::Rgb(0x9A, 0x9A, 0x9A);
pub const FAINT: Color = Color::Rgb(0x55, 0x55, 0x55);
pub const TEXT: Color = Color::Rgb(0xE8, 0xE8, 0xE8);
pub const OK: Color = Color::Rgb(0x4C, 0xD9, 0x7B);
pub const WARN: Color = Color::Rgb(0xFF, 0xC1, 0x4D);
pub const INFO: Color = Color::Rgb(0x5E, 0xA8, 0xFF);
/// Borders of cards and panes without the focus (the focused one is lit in the accent).
pub const BORDER: Color = Color::Rgb(0x6A, 0x6A, 0x72);
/// Background of the selected row and of key chips.
pub const RAISED: Color = Color::Rgb(0x2E, 0x2E, 0x34);
pub const SELECTED: Color = Color::Rgb(0x42, 0x24, 0x26);

/// A key chip for hint bars: ` c ` on a raised background.
pub fn key(k: &str) -> Span<'static> {
    Span::styled(
        format!(" {k} "),
        Style::new()
            .fg(TEXT)
            .bg(RAISED)
            .add_modifier(Modifier::BOLD),
    )
}

/// A key chip followed by its label.
pub fn hint(k: &str, label: &str) -> [Span<'static>; 2] {
    [
        key(k),
        Span::styled(format!(" {label}  "), Style::new().fg(MUTED)),
    ]
}

/// Name colors: every chat author gets their own shade, in the order they first speak.
///
/// Hues step by the golden angle, so neighbours in time are far apart on the color wheel
/// and no two authors ever share a hue; lightness cycles too, which keeps close hues
/// apart once the wheel fills up.
#[derive(Debug, Default)]
pub struct UserColors(HashMap<String, Color>);

impl UserColors {
    pub fn get(&mut self, id: &str) -> Color {
        let next = self.0.len();
        *self.0.entry(id.to_owned()).or_insert_with(|| nth(next))
    }

    /// The color already handed to `id` (gray for someone not seen yet).
    pub fn peek(&self, id: &str) -> Color {
        self.0.get(id).copied().unwrap_or(MUTED)
    }
}

fn nth(i: usize) -> Color {
    const GOLDEN_ANGLE: f64 = 137.507_764;
    // Start at blue so the first viewer does not look like the red accent.
    let hue = (210.0 + i as f64 * GOLDEN_ANGLE) % 360.0;
    let lightness = [0.70, 0.60, 0.80][i % 3];
    hsl(hue, 0.75, lightness)
}

fn hsl(h: f64, s: f64, l: f64) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let byte = |v: f64| ((v + m) * 255.0).round() as u8;
    Color::Rgb(byte(r), byte(g), byte(b))
}

/// Linear blend of two RGB colors; `t` 0 gives `a`, 1 gives `b`.
pub fn mix(a: Color, b: Color, t: f64) -> Color {
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let t = t.clamp(0.0, 1.0);
            let f = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
            Color::Rgb(f(r1, r2), f(g1, g2), f(b1, b2))
        }
        _ => a,
    }
}

/// 0..1..0 over `period`, a soft breathing pulse.
pub fn pulse(t: Duration, period: Duration) -> f64 {
    let phase = t.as_secs_f64() / period.as_secs_f64() * std::f64::consts::TAU;
    (1.0 - phase.cos()) / 2.0
}

/// A braille spinner frame (10 frames per second).
pub fn spinner(t: Duration) -> &'static str {
    const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(t.as_millis() / 100) as usize % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_colors_are_stable_and_distinct() {
        let mut colors = UserColors::default();
        let all: Vec<Color> = (0..64).map(|i| colors.get(&format!("UC{i}"))).collect();
        let mut unique = all.clone();
        unique.sort_by_key(|c| format!("{c:?}"));
        unique.dedup();
        assert_eq!(unique.len(), all.len());
        assert_eq!(colors.get("UC3"), all[3]);
        // Neighbours in time are far apart in hue.
        assert_ne!(all[0], all[1]);
    }

    #[test]
    fn mix_and_pulse() {
        let (a, b) = (Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 50));
        assert_eq!(mix(a, b, 0.5), Color::Rgb(100, 50, 25));
        assert_eq!(mix(a, b, 2.0), b);
        assert!(pulse(Duration::ZERO, Duration::from_secs(2)) < 1e-9);
        assert!((pulse(Duration::from_secs(1), Duration::from_secs(2)) - 1.0).abs() < 1e-9);
    }
}
