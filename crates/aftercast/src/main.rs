mod cli;

use std::process::ExitCode;

use ac_core::home::InitAction;
use ac_core::{Home, Result, exit};
use clap::Parser;

use cli::{Cli, Command};

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = run(cli).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        e.exit_code()
    });
    ExitCode::from(code)
}

fn run(cli: Cli) -> Result<u8> {
    let home = Home::resolve(cli.home)?;
    match cli.command {
        Some(Command::Init) => init(&home),
        _ => {
            eprintln!("not yet implemented");
            Ok(exit::GENERIC)
        }
    }
}

fn init(home: &Home) -> Result<u8> {
    for (path, action) in home.init()? {
        let label = match action {
            InitAction::Created => "created",
            InitAction::Skipped => "exists, skipped",
        };
        println!("{label:<16} {}", path.display());
    }
    Ok(exit::OK)
}
