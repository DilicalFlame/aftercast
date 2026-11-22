//! What aftercast knows about OBS's audio and sources, shared by the OBS service, the
//! controller and the TUI.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Live audio peaks in dBFS per input, written by the controller as OBS reports them
/// (about 10 times a second, only while a page with meters asks) and read by the TUI each
/// frame. Shared rather than published, so meters never rebuild the whole view.
pub type Levels = Arc<Mutex<HashMap<String, f32>>>;

/// dBFS of a linear amplitude, floored at -60.
pub fn db(mul: f32) -> f32 {
    if mul <= 0.001 {
        -60.0
    } else {
        (20.0 * mul.log10()).max(-60.0)
    }
}

/// An input with audio: a mic, desktop audio, a media source.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub name: String,
    /// OBS input kind, e.g. `wasapi_input_capture` for a mic.
    pub kind: String,
    pub muted: bool,
    pub volume_db: f64,
}

impl Audio {
    /// A microphone or line in (an audio *input* capture), not desktop audio or media.
    pub fn is_mic(&self) -> bool {
        self.kind.contains("input_capture")
    }
}

/// One source placed in a scene (a camera, a window capture, a nested scene).
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub scene: String,
    pub source: String,
    pub id: i64,
    pub enabled: bool,
    /// Which aftercast overlay a Browser source shows (`keys` for `.../overlay/keys`).
    pub overlay: Option<String>,
}

/// The overlay a Browser source's URL shows: `http://127.0.0.1:7878/overlay/keys?x` is `keys`.
pub fn overlay_name(url: &str) -> Option<String> {
    let rest = url.split_once("/overlay/")?.1;
    let name: String = rest.chars().take_while(char::is_ascii_lowercase).collect();
    let ends = rest[name.len()..]
        .chars()
        .next()
        .is_none_or(|c| "?#/".contains(c));
    // `assets` and `skins` are folders of the overlay server, not overlays.
    (ends && !name.is_empty() && name != "assets" && name != "skins").then_some(name)
}

#[cfg(test)]
mod overlay_name_tests {
    use super::overlay_name;

    #[test]
    fn reads_the_overlay_from_a_url() {
        assert_eq!(
            overlay_name("http://127.0.0.1:7878/overlay/keys").as_deref(),
            Some("keys")
        );
        assert_eq!(
            overlay_name("http://localhost:7878/overlay/status?x=1").as_deref(),
            Some("status")
        );
        assert_eq!(
            overlay_name("http://localhost:7878/overlay/assets/overlay.js"),
            None
        );
        assert_eq!(overlay_name("https://example.com/page"), None);
    }
}

/// The hardware behind a capture input: the mic's or camera's device name.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    /// The OBS input using it.
    pub input: String,
    /// A camera (video capture); otherwise a mic (audio input capture).
    pub video: bool,
    /// The device's name, as OBS lists it.
    pub name: String,
}

/// Scenes, audio and sources, as listed after connecting or a change in OBS.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Media {
    /// In OBS's order.
    pub scenes: Vec<String>,
    pub audio: Vec<Audio>,
    pub items: Vec<Item>,
    /// Mics and cameras with their device names.
    pub devices: Vec<Device>,
}

impl Media {
    /// The audio input called `name` (case-insensitive).
    pub fn audio(&self, name: &str) -> Option<&Audio> {
        self.audio
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// The item for `source` in `scene` (case-insensitive).
    pub fn item(&self, scene: &str, source: &str) -> Option<&Item> {
        self.items
            .iter()
            .find(|i| i.scene.eq_ignore_ascii_case(scene) && i.source.eq_ignore_ascii_case(source))
    }

    /// Sources placed in `scene`, in OBS's order.
    pub fn sources_in<'a>(&'a self, scene: &'a str) -> impl Iterator<Item = &'a Item> + 'a {
        self.items.iter().filter(move |i| i.scene == scene)
    }
}

/// Whether the facecam is hidden for the pointer: hides at once when the pointer arrives,
/// returns only once it has been away for a while.
#[derive(Debug, Default)]
pub struct Hover {
    hidden: bool,
    left_at: Option<Instant>,
}

impl Hover {
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    /// Feeds whether the pointer is over the camera; `Some(true)` says hide now,
    /// `Some(false)` says show again.
    pub fn step(&mut self, over: bool, now: Instant, back_after: Duration) -> Option<bool> {
        if over {
            self.left_at = None;
            return (!self.hidden).then(|| {
                self.hidden = true;
                true
            });
        }
        if !self.hidden {
            return None;
        }
        let left = *self.left_at.get_or_insert(now);
        (now.duration_since(left) >= back_after).then(|| {
            self.hidden = false;
            self.left_at = None;
            false
        })
    }
}

#[cfg(test)]
mod hover_tests {
    use super::*;

    #[test]
    fn hides_at_once_and_returns_after_the_delay() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut h = Hover::default();
        assert_eq!(h.step(false, t0, ms(600)), None);
        assert_eq!(h.step(true, t0, ms(600)), Some(true));
        assert_eq!(h.step(true, t0 + ms(100), ms(600)), None);
        assert_eq!(h.step(false, t0 + ms(200), ms(600)), None);
        // Back over it before the delay: stays hidden, the wait starts over.
        assert_eq!(h.step(true, t0 + ms(500), ms(600)), None);
        assert_eq!(h.step(false, t0 + ms(600), ms(600)), None);
        assert_eq!(h.step(false, t0 + ms(1100), ms(600)), None);
        assert_eq!(h.step(false, t0 + ms(1200), ms(600)), Some(false));
        assert!(!h.hidden());
    }
}

/// Where an item shows on the canvas as OBS reports it: its alignment point `pos`, its
/// `size` and OBS's alignment bits (1 left, 2 right, 4 top, 8 bottom, none is centered).
/// Returns left, top, width, height.
pub fn placed(pos: (f32, f32), size: (f32, f32), align: u8) -> [f32; 4] {
    let at = |lo: u8, hi: u8| match (align & lo != 0, align & hi != 0) {
        (true, false) => 0.0,
        (false, true) => 1.0,
        _ => 0.5,
    };
    let (x, y) = (pos.0 - size.0 * at(1, 2), pos.1 - size.1 * at(4, 8));
    // A flipped (mirrored) item has a negative size: it spreads the other way from its point.
    let (x, w) = if size.0 < 0.0 {
        (x + size.0, -size.0)
    } else {
        (x, size.0)
    };
    let (y, h) = if size.1 < 0.0 {
        (y + size.1, -size.1)
    } else {
        (y, size.1)
    };
    [x, y, w, h]
}

#[cfg(test)]
mod placed_tests {
    use super::placed;

    #[test]
    fn the_alignment_point_decides_the_corner() {
        assert_eq!(
            placed((1360.0, 680.0), (560.0, 315.0), 5),
            [1360.0, 680.0, 560.0, 315.0]
        );
        assert_eq!(
            placed((960.0, 540.0), (200.0, 100.0), 0),
            [860.0, 490.0, 200.0, 100.0]
        );
        assert_eq!(
            placed((1920.0, 1080.0), (400.0, 300.0), 10),
            [1520.0, 780.0, 400.0, 300.0]
        );
        // A mirrored camera (negative width) still gives a box with a positive size.
        assert_eq!(
            placed((2892.0, 941.0), (-800.0, 600.0), 5),
            [2092.0, 941.0, 800.0, 600.0]
        );
        assert_eq!(
            placed((1000.0, 500.0), (-200.0, -100.0), 0),
            [900.0, 450.0, 200.0, 100.0]
        );
    }
}
