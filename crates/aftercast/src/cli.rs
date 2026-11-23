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

    /// With --no-tui, keep a new session a draft until a `begin` script line, as the TUI
    /// does until Enter on Home (for tests)
    #[arg(long, hide = true, requires = "no_tui")]
    pub draft: bool,

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
    /// Practice with real OBS and overlays: no YouTube broadcast, a demo chat, and overlay
    /// files reload in OBS when saved
    Rehearse,
    /// Install the transcriber (Python, faster-whisper, CUDA libraries, model) under <home>/tools with uv
    Setup,
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
    /// Render the items of ai/suggestions.json (default: latest session)
    Render {
        session: Option<String>,
        /// Comma-separated suggestion ids, e.g. 001,T01
        #[arg(long, value_delimiter = ',')]
        ids: Vec<String>,
        /// Validate and show the adjusted ranges without rendering
        #[arg(long)]
        dry_run: bool,
        /// Re-render items even when their inputs are unchanged
        #[arg(long)]
        force: bool,
        /// Keystroke overlay (default: render.keys_overlay)
        #[arg(long)]
        keys: Option<KeysMode>,
        /// Captions (default: render.captions)
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
    /// Make overlays (specs/2026-11-23-overlay-plugins.md)
    Overlay {
        #[command(subcommand)]
        action: OverlayAction,
    },
    /// Install, list and remove overlay plugins
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum OverlayAction {
    /// Create a new overlay project, ready for `plugin add`
    New {
        /// The overlay's id: 1 to 32 of a-z 0-9 - _
        id: String,
        /// plain: HTML, CSS and JS with no build; svelte: Svelte 5 and Vite
        #[arg(long, value_enum, default_value_t = Template::Plain)]
        template: Template,
        /// Where to create it (default: ./<id>)
        #[arg(long, value_name = "PATH")]
        dir: Option<PathBuf>,
        /// The name shown in the editor (default: the id as words)
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum PluginAction {
    /// Install an overlay from a folder, a .zip file or a git URL
    Add {
        source: String,
        /// Do not ask
        #[arg(long, short = 'y')]
        yes: bool,
        /// Use the folder in place instead of copying it (for developing a plugin)
        #[arg(long)]
        link: bool,
        /// Refuse a package that is not signed by a trusted key
        #[arg(long)]
        require_signed: bool,
    },
    /// Search the registries in [plugin_registry]
    Search { query: Option<String> },
    /// Make a signing key for packages (the secret stays in <home>/state/plugin-keys)
    Keygen { name: String },
    /// Zip a plugin folder for sharing; --sign adds a signature file next to it
    Pack {
        dir: PathBuf,
        /// A key made with `plugin keygen`
        #[arg(long)]
        sign: Option<String>,
        /// The zip to write (default: <id>-<version>.zip next to the folder)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// List installed plugins
    List,
    /// Remove a plugin and its settings
    Remove {
        id: String,
        /// Do not ask
        #[arg(long, short = 'y')]
        yes: bool,
    },
    /// Check a plugin folder's manifest without installing
    Check { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Template {
    Plain,
    Svelte,
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
    /// Index every session again from its files
    Rebuild,
    /// Search the transcripts of all sessions
    Search {
        query: String,
        /// Only this session
        #[arg(long)]
        session: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
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
