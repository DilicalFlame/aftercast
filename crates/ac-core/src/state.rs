//! `state/state.toml`: machine state owned by aftercast (specs/03-config.md).

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, parse_toml};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub next_episode: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_session: Option<PathBuf>,
    pub schema_version: u32,
}

impl Default for State {
    fn default() -> Self {
        Self {
            next_episode: 1,
            last_session: None,
            schema_version: 1,
        }
    }
}

impl State {
    /// Loads the state file; a missing file yields the default state.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => parse_toml(path, &text),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(path)(e)),
        }
    }

    /// Writes the state atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::write_atomic(path, self.to_toml())
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).expect("state serializes to TOML")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_default_and_save_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.toml");
        assert_eq!(State::load(&path).unwrap(), State::default());

        let state = State {
            next_episode: 43,
            last_session: Some("sessions/2026/2026-11-19_ep042".into()),
            schema_version: 1,
        };
        state.save(&path).unwrap();
        state.save(&path).unwrap(); // overwriting an existing file works too
        assert_eq!(State::load(&path).unwrap(), state);
    }
}
