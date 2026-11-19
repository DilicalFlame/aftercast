//! Errors surfaced by CLI commands, with their exit codes (specs/02-cli-and-tui.md).

use ac_core::exit;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] ac_core::Error),
    #[error(transparent)]
    Youtube(#[from] ac_youtube::Error),
    #[error(transparent)]
    Render(#[from] ac_template::RenderError),
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl AppError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Core(e) => e.exit_code(),
            Self::Youtube(e) => e.exit_code(),
            Self::Render(_) => exit::CONFIG,
            Self::Usage(_) | Self::Io(_) => exit::GENERIC,
        }
    }
}

pub type Result<T, E = AppError> = std::result::Result<T, E>;
