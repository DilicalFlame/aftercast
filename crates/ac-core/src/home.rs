//! Home folder resolution and `aftercast init` (specs/04-session-and-folders.md).

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::state::State;

pub const HOME_ENV: &str = "AFTERCAST_HOME";

/// Folders created by `init`.
const DIRS: [&str; 6] = [
    "templates/partials",
    "overlays",
    "state",
    "logs",
    "library",
    "sessions",
];

/// Default files written by `init` (copies of specs/examples, embedded at build time).
const DEFAULT_FILES: [(&str, &str); 4] = [
    ("aftercast.toml", include_str!("../defaults/aftercast.toml")),
    (
        "templates/session-readme.md",
        include_str!("../defaults/session-readme.md"),
    ),
    (
        "templates/partials/links.md",
        include_str!("../defaults/partials/links.md"),
    ),
    (
        "templates/partials/support.md",
        include_str!("../defaults/partials/support.md"),
    ),
];

/// The default `aftercast.toml`, as written by `init`.
pub const DEFAULT_CONFIG: &str = DEFAULT_FILES[0].1;

/// The global streaming folder (`AFTERCAST_HOME`).
#[derive(Debug, Clone)]
pub struct Home {
    root: PathBuf,
}

/// Outcome of writing one default file in `init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitAction {
    Created,
    Skipped,
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolves the home: `--home` flag, then `AFTERCAST_HOME`, then `~/aftercast`.
    pub fn resolve(flag: Option<PathBuf>) -> Result<Self> {
        pick_root(flag, std::env::var_os(HOME_ENV), std::env::home_dir()).map(Self::new)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Joins a `/`-separated relative path with native separators.
    pub fn join(&self, rel: &str) -> PathBuf {
        rel.split('/').fold(self.root.clone(), |p, c| p.join(c))
    }

    pub fn config_file(&self) -> PathBuf {
        self.join("aftercast.toml")
    }

    pub fn state_file(&self) -> PathBuf {
        self.join("state/state.toml")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.join("logs")
    }

    /// Creates the home tree and default files. Existing files are never overwritten.
    pub fn init(&self) -> Result<Vec<(PathBuf, InitAction)>> {
        for dir in DIRS {
            let path = self.join(dir);
            fs::create_dir_all(&path).map_err(Error::io(&path))?;
        }
        let state = State::default().to_toml();
        DEFAULT_FILES
            .into_iter()
            .chain([("state/state.toml", state.as_str())])
            .map(|(rel, content)| {
                let path = self.join(rel);
                write_new(&path, content).map(|action| (path, action))
            })
            .collect()
    }
}

fn pick_root(
    flag: Option<PathBuf>,
    env: Option<OsString>,
    user_home: Option<PathBuf>,
) -> Result<PathBuf> {
    flag.or_else(|| env.filter(|v| !v.is_empty()).map(PathBuf::from))
        .or_else(|| user_home.map(|h| h.join("aftercast")))
        .ok_or(Error::NoHome)
}

/// Writes `content` to `path` unless the file already exists.
fn write_new(path: &Path, content: &str) -> Result<InitAction> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(content.as_bytes())
                .map_err(Error::io(path))?;
            Ok(InitAction::Created)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(InitAction::Skipped),
        Err(e) => Err(Error::io(path)(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_precedence_is_flag_then_env_then_user_home() {
        let flag = Some(PathBuf::from("F"));
        let env = Some(OsString::from("E"));
        let user = Some(PathBuf::from("U"));
        assert_eq!(
            pick_root(flag, env.clone(), user.clone()).unwrap(),
            PathBuf::from("F")
        );
        assert_eq!(
            pick_root(None, env, user.clone()).unwrap(),
            PathBuf::from("E")
        );
        assert_eq!(
            pick_root(None, Some(OsString::new()), user).unwrap(),
            Path::new("U").join("aftercast")
        );
        assert!(matches!(pick_root(None, None, None), Err(Error::NoHome)));
    }

    #[test]
    fn init_creates_tree_and_skips_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().join("Streams"));

        let first = home.init().unwrap();
        assert!(first.iter().all(|(_, a)| *a == InitAction::Created));
        for d in DIRS {
            assert!(home.join(d).is_dir(), "{d}");
        }
        assert_eq!(
            fs::read_to_string(home.config_file()).unwrap(),
            DEFAULT_CONFIG
        );
        assert_eq!(State::load(&home.state_file()).unwrap(), State::default());

        fs::write(home.config_file(), "# mine").unwrap();
        let second = home.init().unwrap();
        assert_eq!(second.len(), first.len());
        assert!(second.iter().all(|(_, a)| *a == InitAction::Skipped));
        assert_eq!(fs::read_to_string(home.config_file()).unwrap(), "# mine");
    }
}
