//! Config, paths, session, events, state, time, quota ledger and errors.

pub mod chat;
pub mod command;
pub mod config;
pub mod error;
pub mod event;
pub mod home;
pub mod logging;
pub mod obs;
pub mod recordings;
pub mod session;
pub mod session_state;
pub mod state;
pub mod time;
pub mod youtube;

pub use config::Config;
pub use error::{Error, Result, exit};
pub use home::Home;

use std::path::Path;

/// Writes a file atomically: temp file in the same folder, then rename over the target.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    std::fs::write(tmp, contents).map_err(Error::io(tmp))?;
    std::fs::rename(tmp, path).map_err(Error::io(path))
}
