//! Command line interface (specs/02-cli-and-tui.md).

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "aftercast",
    version,
    about = "TUI companion for live coding streams on YouTube"
)]
pub struct Cli {
    /// Stream link or 11-char video id; omit to resume the last session
    pub link: Option<String>,

    /// Home folder (overrides AFTERCAST_HOME)
    #[arg(long, global = true, value_name = "PATH")]
    pub home: Option<PathBuf>,

    /// Config file (default: <home>/aftercast.toml)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Increase log verbosity (-v, -vv)
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Use canned YouTube/OBS/chat fixtures instead of the network
    #[arg(long, global = true, value_name = "FIXTURE_DIR")]
    pub mock: Option<PathBuf>,

    /// Run the live session without the TUI (commands from --script or stdin)
    #[arg(long)]
    pub no_tui: bool,

    /// Command script for --no-tui, e.g. lines like `+5s checkpoint "Parser"`
    #[arg(long, value_name = "FILE", requires = "no_tui")]
    pub script: Option<PathBuf>,

    /// Accept a broadcast that already ended (to finalize after the fact)
    #[arg(long)]
    pub allow_ended: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create the home folder with default config and templates
    Init,
    /// YouTube OAuth
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Check tools, config, clock and keyring
    Doctor,
    /// List sessions
    Sessions {
        #[arg(long)]
        json: bool,
    },
    /// Run the post-stream pipeline (default: latest session)
    Post {
        session: Option<String>,
        /// Re-run from this stage onward
        #[arg(long, value_name = "STAGE")]
        from: Option<String>,
        /// Run only this stage
        #[arg(long, value_name = "STAGE", conflicts_with = "from")]
        only: Option<String>,
        /// Re-run stages even when their inputs are unchanged
        #[arg(long)]
        force: bool,
        /// Transcription language for this run (en, hi, auto)
        #[arg(long)]
        language: Option<String>,
    },
    /// Render suggested clips and topic videos
    Render {
        session: Option<String>,
        /// Comma-separated suggestion ids, e.g. 001,T01
        #[arg(long, value_delimiter = ',')]
        ids: Vec<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        keys: Option<KeysMode>,
        #[arg(long)]
        captions: Option<CaptionsMode>,
    },
    /// Export derived data for review
    Export {
        #[command(subcommand)]
        what: ExportWhat,
    },
    /// Render the description; --push uploads it
    Desc {
        session: Option<String>,
        #[arg(long)]
        push: bool,
    },
    /// Cross-session index
    Library {
        #[command(subcommand)]
        action: LibraryAction,
    },
    /// Run the MCP stdio server
    Mcp,
    /// Print the shell integration snippet
    ShellHook { shell: Shell },
    /// Measure stream latency (needs a live session)
    Latency,
}

#[derive(Debug, Subcommand)]
pub enum AuthAction {
    Login,
    Status,
    Logout,
    /// Store the obs-websocket password (read from stdin) in the OS keyring
    Obs,
}

#[derive(Debug, Subcommand)]
pub enum ExportWhat {
    /// Materialise dead segments for review
    Silences { session: Option<String> },
}

#[derive(Debug, Subcommand)]
pub enum LibraryAction {
    Rebuild,
    Search { query: String },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum KeysMode {
    Burn,
    Alpha,
    None,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CaptionsMode {
    Burn,
    Srt,
    None,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    Pwsh,
    Bash,
    Zsh,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn link_and_subcommands_parse() {
        let cli = Cli::parse_from(["aftercast", "https://youtu.be/abcdefghijk"]);
        assert!(cli.command.is_none());
        let cli = Cli::parse_from(["aftercast", "render", "ep042", "--ids", "001,T01"]);
        assert!(matches!(cli.command, Some(Command::Render { ids, .. }) if ids == ["001", "T01"]));
        for args in [["init", "--home", "X"], ["--home", "X", "init"]] {
            let cli = Cli::parse_from(std::iter::once("aftercast").chain(args));
            assert!(matches!(cli.command, Some(Command::Init)));
            assert_eq!(cli.home, Some(PathBuf::from("X")));
        }
    }
}
