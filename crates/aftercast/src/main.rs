mod cli;
mod doctor;
mod error;
mod live;
mod script;
mod secret;
mod setup;

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
    let tui = cli.command.is_none() && !cli.no_tui;
    ac_core::logging::init(&home.logs_dir(), cli.verbose, !tui);
    let config_path = cli.config.clone().unwrap_or_else(|| home.config_file());
    match cli.command {
        Some(Command::Init) => init(&home),
        Some(Command::Doctor) => Ok(doctor::run(&home, cli.config)),
        Some(Command::Setup) => setup::run(&home, &config_path),
        Some(Command::Auth { action }) => runtime()?.block_on(auth(&home, &config_path, action)),
        Some(Command::ShellHook { shell }) => shell_hook(&home, &config_path, shell),
        Some(Command::Post {
            session,
            from,
            only,
            force,
            language,
        }) => post(
            &home,
            &config_path,
            session.as_deref(),
            from,
            only,
            force,
            language,
        ),
        Some(Command::Render {
            session,
            ids,
            dry_run,
            force,
            keys,
            captions,
        }) => {
            use ac_core::config::{Captions, KeysOverlay};
            let config = Config::load(&config_path)?;
            let opts = ac_render::Options {
                ids,
                dry_run,
                force,
                keys: keys.map_or(config.render.keys_overlay, |k| match k {
                    cli::KeysMode::Burn => KeysOverlay::Burn,
                    cli::KeysMode::Alpha => KeysOverlay::Alpha,
                    cli::KeysMode::None => KeysOverlay::None,
                }),
                captions: captions.map_or(config.render.captions, |c| match c {
                    cli::CaptionsMode::Burn => Captions::Burn,
                    cli::CaptionsMode::Srt => Captions::Srt,
                    cli::CaptionsMode::None => Captions::None,
                }),
            };
            render(&home, config, session.as_deref(), &opts)
        }
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
        return Ok(exit::OK);
    }
    let live::Handles {
        clock,
        commands,
        msgs,
        mut view,
        controller,
        ..
    } = handles;
    let (external_tx, external) = std::sync::mpsc::channel();
    let config = ac_core::Config::load(config_path)?;
    if let Err(e) = hotkeys(
        &config.hotkeys,
        clock.clone(),
        commands.clone(),
        external_tx.clone(),
    ) {
        tracing::warn!("global hotkeys: {e}");
        let _ = external_tx.send(ac_tui::External::Message(format!(
            "global hotkeys unavailable: {e}"
        )));
    }
    if config.capture.keys {
        let (keys, errors) = (msgs.clone(), msgs.clone());
        let started = ac_capture::hook::spawn(
            clock.clone(),
            config.capture.keys_trusted_apps.clone(),
            move |raw| {
                let _ = keys.send(live::Msg::Key(raw));
            },
            move |e| {
                let _ = errors.send(live::Msg::KeysHook(Err(e)));
            },
        );
        let _ = msgs.send(live::Msg::KeysHook(started));
    }
    drop(msgs);
    let ui_commands = commands.clone();
    let ui_view = view.clone();
    let ui =
        tokio::task::spawn_blocking(move || ac_tui::run(clock, ui_view, ui_commands, external))
            .await;
    // However the UI ended, end the session cleanly (ignored if it already ended).
    let _ = commands.send(ac_core::command::Command::Quit);
    drop(commands);
    controller
        .await
        .map_err(|e| error::AppError::Usage(format!("session task failed: {e}")))?;
    let last = view.borrow_and_update().clone();
    println!("Session {} saved.", last.session);
    if let Some(notice) = last.notice {
        println!("{notice}");
    }
    ui.map_err(|e| error::AppError::Usage(format!("TUI task failed: {e}")))??;
    Ok(exit::OK)
}

/// Registers the global hotkeys. Time is captured at the press; a checkpoint's title
/// prompt opens in the TUI.
fn hotkeys(
    keys: &ac_core::config::Hotkeys,
    clock: std::sync::Arc<Clock>,
    commands: tokio::sync::mpsc::UnboundedSender<ac_core::command::Command>,
    external: std::sync::mpsc::Sender<ac_tui::External>,
) -> std::result::Result<(), String> {
    use ac_capture::HotkeyAction as A;
    use ac_core::command::Command as C;
    use ac_core::event::By;
    let parsed = ac_capture::parse(&[
        (A::Mark, &keys.mark),
        (A::Checkpoint, &keys.checkpoint),
        (A::Privacy, &keys.privacy),
        (A::Brb, &keys.brb),
    ])?;
    ac_capture::spawn(parsed, move |action| {
        let pressed_at = clock.now_ts();
        let by = By::Hotkey;
        let _ = match action {
            A::Checkpoint => {
                let _ = external.send(ac_tui::External::Checkpoint(pressed_at));
                return;
            }
            A::Mark => commands.send(C::Mark {
                label: None,
                pressed_at,
                by,
            }),
            A::Brb => commands.send(C::Brb { on: None, by }),
            A::Privacy => commands.send(C::Privacy { on: None, by }),
        };
    })
}

/// Runs the post-stream pipeline on a session (the last one when none is given).
fn post(
    home: &Home,
    config_path: &std::path::Path,
    session: Option<&str>,
    from: Option<String>,
    only: Option<String>,
    force: bool,
    language: Option<String>,
) -> Result<u8> {
    use error::AppError;
    let config = Config::load(config_path)?;
    let dir = session_dir(home, &config, session)?;
    let stage = |s: Option<String>| {
        s.map(|s| ac_post::Stage::parse(&s))
            .transpose()
            .map_err(AppError::Usage)
    };
    let opts = ac_post::Options {
        from: stage(from)?,
        only: stage(only)?,
        force,
    };
    let mut ctx = ac_post::Ctx::open(home.clone(), config, &dir).map_err(AppError::Usage)?;
    ctx.language_override = language;
    println!("post {}", dir.display());
    let report = ac_post::run(&mut ctx, &opts).map_err(AppError::Usage)?;
    for (stage, outcome) in report {
        let what = match outcome {
            ac_post::Outcome::Ran => "done".to_owned(),
            ac_post::Outcome::UpToDate => "up to date".to_owned(),
            ac_post::Outcome::Disabled(why) => format!("skipped ({why})"),
        };
        println!("  {:<11} {what}", stage.name());
    }
    for note in &ctx.notes {
        println!("warning: {note}");
    }
    println!("index: {}", dir.join("derived").join("index.md").display());
    Ok(exit::OK)
}

fn render(
    home: &Home,
    config: Config,
    session: Option<&str>,
    opts: &ac_render::Options,
) -> Result<u8> {
    use ac_render::Outcome;
    use error::AppError;
    let dir = session_dir(home, &config, session)?;
    let mut ctx = ac_post::Ctx::open(home.clone(), config, &dir).map_err(AppError::Usage)?;
    println!("render {}", dir.display());
    let report = ac_render::run(&mut ctx, opts).map_err(AppError::Usage)?;
    let hms_ms = |t: f64| {
        let ms = (t * 1000.0).round() as u64;
        format!(
            "{}.{:03}",
            ac_core::time::hms((ms / 1000) as f64),
            ms % 1000
        )
    };
    let (mut rendered, mut skipped, mut failed) = (0, 0, 0);
    for (c, outcome) in &report {
        let item = &c.item;
        let what = match outcome {
            Outcome::Valid => format!("valid, {:.1} s", c.out_len()),
            Outcome::Rendered { duration_s } => {
                rendered += 1;
                format!("rendered, {duration_s:.1} s")
            }
            Outcome::UpToDate => {
                skipped += 1;
                "up to date".to_owned()
            }
            Outcome::Failed(why) => {
                failed += 1;
                format!("FAILED: {why}")
            }
        };
        println!(
            "  {} {:<5} {}  {what}",
            item.id,
            item.kind.name(),
            item.title
        );
        for cut in &c.cuts {
            let [a, b] = cut.suggested;
            let speed = if cut.speed == 1.0 {
                String::new()
            } else {
                format!("  x{}", cut.speed)
            };
            println!(
                "      {} to {}  (suggested {} to {}){speed}",
                hms_ms(cut.start),
                hms_ms(cut.end),
                hms_ms(a),
                hms_ms(b)
            );
        }
        for w in &c.warnings {
            println!("      warning: {w}");
        }
        if matches!(outcome, Outcome::Rendered { .. } | Outcome::UpToDate) {
            println!("      {}", ac_render::out_dir(&dir, item).display());
        }
    }
    if !opts.dry_run {
        println!("{rendered} rendered, {skipped} up to date, {failed} failed");
    }
    Ok(if failed > 0 { exit::GENERIC } else { exit::OK })
}

/// A session folder from a path, a folder name like `2026-11-19_ep042`, or the last session.
fn session_dir(home: &Home, config: &Config, session: Option<&str>) -> Result<PathBuf> {
    use error::AppError;
    let Some(arg) = session else {
        let state = ac_core::state::State::load(&home.state_file())?;
        let rel = state
            .last_session
            .ok_or_else(|| AppError::Usage("no previous session; pass one".into()))?;
        return Ok(home.root().join(rel));
    };
    let path = PathBuf::from(arg);
    if path.join("session.toml").exists() {
        return Ok(path);
    }
    let sessions = ac_core::session::sessions_dir(home, config);
    let found = std::fs::read_dir(&sessions)
        .into_iter()
        .flatten()
        .flatten()
        .map(|year| year.path().join(arg))
        .find(|p| p.join("session.toml").exists());
    found.ok_or_else(|| {
        AppError::Usage(format!(
            "no session {arg:?} (a folder name under {}, or a path)",
            sessions.display()
        ))
    })
}

/// Prints the shell snippet that spools commands for capture.
fn shell_hook(home: &Home, config_path: &std::path::Path, shell: cli::Shell) -> Result<u8> {
    let config = Config::load(config_path)?;
    match shell {
        cli::Shell::Pwsh => {
            let spool = home.path(&config.capture.shell_spool);
            print!("{}", ac_capture::shell::pwsh_hook(&spool));
            Ok(exit::OK)
        }
        cli::Shell::Bash | cli::Shell::Zsh => Err(error::AppError::Usage(
            "only PowerShell is supported for now: aftercast shell-hook pwsh".into(),
        )),
    }
}

async fn auth(home: &Home, config_path: &std::path::Path, action: AuthAction) -> Result<u8> {
    let config = Config::load(config_path)?;
    if let AuthAction::Obs = action {
        return obs_password(&config.obs.password);
    }
    let profile = &config.youtube.token_profile;
    let secret: PathBuf = home.path(&config.youtube.client_secret_file);
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
        AuthAction::Obs => unreachable!("handled above"),
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

/// `auth obs`: reads the obs-websocket password from stdin into the keyring entry that
/// `[obs].password = "keyring:<name>"` names.
fn obs_password(reference: &str) -> Result<u8> {
    let Some(name) = reference.strip_prefix("keyring:") else {
        eprintln!("[obs].password is {reference:?}, not a keyring reference; nothing to store.");
        return Ok(exit::CONFIG);
    };
    eprint!("OBS websocket password (Tools > WebSocket Server Settings): ");
    let mut password = String::new();
    std::io::stdin().read_line(&mut password)?;
    secret::store(name, password.trim_end_matches(['\r', '\n'])).map_err(error::AppError::Usage)?;
    println!("Stored in the OS keyring ({} / {name}).", secret::SERVICE);
    Ok(exit::OK)
}
