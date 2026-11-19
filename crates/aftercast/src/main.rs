mod cli;
mod doctor;
mod error;
mod live;
mod script;

use std::path::PathBuf;
use std::process::ExitCode;

use ac_core::config::Config;
use ac_core::home::InitAction;
use ac_core::time::Clock;
use ac_core::{Home, exit};
use ac_youtube::Client;
use ac_youtube::auth::{self, Auth};
use ac_youtube::quota::QuotaGuard;
use clap::Parser;

use cli::{AuthAction, Cli, Command};
use error::Result;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = run(cli).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        e.exit_code()
    });
    ExitCode::from(code)
}

fn run(cli: Cli) -> Result<u8> {
    let home = Home::resolve(cli.home.clone())?;
    ac_core::logging::init(&home.logs_dir(), cli.verbose);
    let config_path = cli.config.clone().unwrap_or_else(|| home.config_file());
    match cli.command {
        Some(Command::Init) => init(&home),
        Some(Command::Doctor) => Ok(doctor::run(&home, cli.config)),
        Some(Command::Auth { action }) => runtime()?.block_on(auth(&home, &config_path, action)),
        None => runtime()?.block_on(live_session(&home, &config_path, &cli)),
        Some(_) => {
            eprintln!("not yet implemented");
            Ok(exit::GENERIC)
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Runtime::new()?)
}

fn init(home: &Home) -> Result<u8> {
    let results = home.init()?;
    tracing::info!(home = %home.root().display(), "init");
    for (path, action) in results {
        let label = match action {
            InitAction::Created => "created",
            InitAction::Skipped => "exists, skipped",
        };
        println!("{label:<16} {}", path.display());
    }
    Ok(exit::OK)
}

async fn live_session(home: &Home, config_path: &std::path::Path, cli: &Cli) -> Result<u8> {
    let handles = live::start(
        home,
        config_path,
        live::Options {
            link: cli.link.as_deref(),
            mock: cli.mock.as_deref(),
            allow_ended: cli.allow_ended,
        },
    )
    .await?;
    if cli.no_tui {
        script::run(handles, cli.script.as_deref()).await?;
    } else {
        return Err(error::AppError::Usage(
            "the TUI is not built yet; use --no-tui".into(),
        ));
    }
    Ok(exit::OK)
}

async fn auth(home: &Home, config_path: &std::path::Path, action: AuthAction) -> Result<u8> {
    let config = Config::load(config_path)?;
    let profile = &config.youtube.token_profile;
    let secret: PathBuf = home.root().join(&config.youtube.client_secret_file);
    match action {
        AuthAction::Login => {
            auth::login(&secret, profile).await?;
            println!(
                "Signed in; refresh token stored in the OS keyring (aftercast / youtube/{profile})."
            );
        }
        AuthAction::Status => {
            let quota = QuotaGuard::open(
                &home.join("state/quota.json"),
                config.youtube.daily_quota_reserve,
                config.youtube.costs.clone(),
            )?;
            let clock = std::sync::Arc::new(Clock::new());
            let client = Client::rest(Auth::load(&secret, profile)?, quota, clock.clone());
            let channel = client.my_channel().await?;
            println!("profile  {profile}");
            println!("channel  {} ({})", channel.title, channel.id);
            if let Some(secs) = client.token_expires_in() {
                println!("token    access token valid for {} min", secs / 60);
            }
            println!(
                "quota    {} units left today",
                client.quota().remaining(clock.now_ts())
            );
        }
        AuthAction::Logout => {
            if auth::logout(profile).await? {
                println!("Signed out of profile {profile}.");
            } else {
                println!("Profile {profile} was not signed in.");
            }
        }
    }
    Ok(exit::OK)
}
