//! What aftercast knows about OBS's audio and sources, shared by the OBS service, the
//! controller and the TUI.

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
}

/// Scenes, audio and sources, as listed after connecting or a change in OBS.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Media {
    /// In OBS's order.
    pub scenes: Vec<String>,
    pub audio: Vec<Audio>,
    pub items: Vec<Item>,
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
