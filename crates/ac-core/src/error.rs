//! Error types and process exit codes.

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

/// Process exit codes (specs/02-cli-and-tui.md).
pub mod exit {
    pub const OK: u8 = 0;
    pub const GENERIC: u8 = 1;
    pub const CONFIG: u8 = 2;
    pub const TOOL_MISSING: u8 = 4;
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Toml(#[from] TomlError),
    #[error("{}: not found (run `aftercast init` first)", .0.display())]
    ConfigMissing(PathBuf),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot determine the home folder; set AFTERCAST_HOME or pass --home")]
    NoHome,
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Toml(_) | Self::ConfigMissing(_) => exit::CONFIG,
            Self::Io { .. } | Self::NoHome => exit::GENERIC,
        }
    }

    /// Adapter for `map_err` that attaches the path to an I/O error.
    pub fn io(path: &Path) -> impl FnOnce(io::Error) -> Self + '_ {
        move |source| Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// A TOML file that failed to parse or validate, located as `file:line:col`.
#[derive(Debug, thiserror::Error)]
#[error("{}:{line}:{col}: {message}", path.display())]
pub struct TomlError {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub message: String,
}

/// Parses `text` (read from `path`) into `T`, mapping errors to their location.
pub(crate) fn parse_toml<T: DeserializeOwned>(path: &Path, text: &str) -> Result<T> {
    toml::from_str(text).map_err(|e| {
        let offset = e.span().map_or(0, |s| s.start);
        let (line, col) = line_col(text, offset);
        let message = e.message().trim().replace('\n', "; ");
        Error::Toml(TomlError {
            path: path.to_path_buf(),
            line,
            col,
            message,
        })
    })
}

/// 1-based line and column (in chars) of a byte offset.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_lines_and_chars() {
        let text = "a = 1\nbé = x\n";
        assert_eq!(line_col(text, 0), (1, 1));
        assert_eq!(line_col(text, 6), (2, 1));
        assert_eq!(line_col(text, text.find('x').unwrap()), (2, 6));
    }

    #[test]
    fn parse_error_has_location() {
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct T {
            a: u32,
        }
        let err = parse_toml::<T>(Path::new("f.toml"), "\na = \"x\"\n").unwrap_err();
        assert_eq!(err.exit_code(), exit::CONFIG);
        assert!(err.to_string().starts_with("f.toml:2:5: "), "{err}");
    }
}
