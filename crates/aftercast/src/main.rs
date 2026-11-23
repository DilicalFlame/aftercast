mod cli;
mod doctor;
mod error;
mod live;
mod plugin_cmd;
mod plugin_pkg;
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

use cli::{AuthAction, Cli, Command, OverlayAction, PluginAction};
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
    let tui = matches!(cli.command, None | Some(Command::Rehearse)) && !cli.no_tui;
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
        Some(Command::Sessions { json }) => sessions(&home, &config_path, json),
        Some(Command::Mcp) => {
            let server = ac_mcp::Server::new(home.clone(), config_path.clone());
            server.serve(std::io::stdin().lock(), std::io::stdout().lock())?;
            Ok(exit::OK)
        }
        Some(Command::Library { action }) => library(&home, &config_path, action),
        Some(Command::Overlay {
            action:
                OverlayAction::New {
                    id,
                    template,
                    dir,
                    name,
                },
        }) => plugin_cmd::overlay_new(&id, template, dir, name),
        Some(Command::Plugin { action }) => match action {
            PluginAction::Add {
                source,
                yes,
                link,
                require_signed,
            } => plugin_cmd::plugin_add(&home, &config_path, &source, yes, link, require_signed),
            PluginAction::Search { query } => {
                plugin_cmd::plugin_search(&config_path, query.as_deref())
            }
            PluginAction::Keygen { name } => plugin_cmd::plugin_keygen(&home, &name),
            PluginAction::Pack { dir, sign, out } => {
                plugin_cmd::plugin_pack(&home, &dir, out, sign.as_deref())
            }
            PluginAction::List => plugin_cmd::plugin_list(&home),
            PluginAction::Remove { id, yes } => {
                plugin_cmd::plugin_remove(&home, &config_path, &id, yes)
            }
            PluginAction::Check { path } => plugin_cmd::plugin_check(&path),
        },
        Some(Command::Rehearse) => {
            runtime()?.block_on(live_session(&home, &config_path, &cli, true))
        }
        None => runtime()?.block_on(live_session(&home, &config_path, &cli, false)),
        Some(_) => {
            eprintln!("not yet implemented");
            Ok(exit::GENERIC)
        }
    }
}

/// Every session, newest first: episode, folder, status, recording length and how far the
/// after-stream steps got.
fn sessions(home: &Home, config_path: &std::path::Path, json: bool) -> Result<u8> {
    let config = Config::load(config_path)?;
    let mut rows = Vec::new();
    for dir in ac_library::session_dirs(home, &config).iter().rev() {
        let Ok(s) = ac_core::session::Session::open(dir) else {
            continue;
        };
        let length = std::fs::read_to_string(dir.join("media/master.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|m| m["duration_s"].as_f64());
        let status = serde_json::to_value(s.meta.status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        rows.push(serde_json::json!({
            "name": name,
            "episode": s.meta.episode,
            "rehearse": s.meta.episode == ac_core::session::REHEARSE_EPISODE,
            "status": status,
            "title": s.meta.title_at_start.clone().unwrap_or_default(),
            "video_id": s.meta.video_id,
            "duration_s": length,
            "processed": dir.join("derived/index.md").exists(),
            "has_suggestions": dir.join("ai/suggestions.json").exists(),
            "rendered": dir.join("out").is_dir(),
            "path": dir,
        }));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return Ok(exit::OK);
    }
    if rows.is_empty() {
        println!("no sessions yet");
    }
    for r in &rows {
        let done: Vec<&str> = [
            ("processed", "post"),
            ("has_suggestions", "clips"),
            ("rendered", "render"),
        ]
        .iter()
        .filter(|(k, _)| r[k] == true)
        .map(|(_, l)| *l)
        .collect();
        println!(
            "{:<20} ep{:03}  {:<9} {:>8}  {:<18} {}",
            r["name"].as_str().unwrap_or_default(),
            r["episode"].as_u64().unwrap_or(0),
            r["status"].as_str().unwrap_or_default(),
            r["duration_s"]
                .as_f64()
                .map_or("-".into(), ac_core::time::hms),
            done.join(" "),
            r["title"].as_str().unwrap_or_default(),
        );
    }
    Ok(exit::OK)
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

async fn live_session(
    home: &Home,
    config_path: &std::path::Path,
    cli: &Cli,
    rehearse: bool,
) -> Result<u8> {
    let handles = live::start(
        home,
        config_path,
        live::Options {
            link: cli.link.as_deref(),
            mock: cli.mock.as_deref(),
            allow_ended: cli.allow_ended,
            rehearse,
            // Headless runs start at once; the TUI starts the session from Home.
            begin: cli.no_tui && !cli.draft,
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
    if rehearse {
        open_rehearse_page(config.overlay.port);
    }
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
    if !last.begun && last.fresh {
        println!("Quit before starting: nothing was kept.");
        return Ok(exit::OK);
    }
    println!("Session {} saved.", last.session);
    if let Some(notice) = last.notice {
        println!("{notice}");
    }
    let post_after = ui.map_err(|e| error::AppError::Usage(format!("TUI task failed: {e}")))??;
    if post_after {
        // The terminal is plain again: the pipeline's progress bars print here.
        return post(
            home,
            config_path,
            Some(&last.session),
            None,
            None,
            false,
            None,
        );
    }
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
        if let Err(e) = ac_library::Library::open(home).and_then(|mut l| l.upsert(&dir)) {
            eprintln!("warning: library not updated: {e}");
        }
    }
    Ok(if failed > 0 { exit::GENERIC } else { exit::OK })
}

fn library(home: &Home, config_path: &std::path::Path, action: cli::LibraryAction) -> Result<u8> {
    use error::AppError;
    let config = Config::load(config_path)?;
    let mut lib = ac_library::Library::open(home).map_err(AppError::Usage)?;
    match action {
        cli::LibraryAction::Rebuild => {
            let n = lib.rebuild(home, &config).map_err(AppError::Usage)?;
            println!(
                "indexed {n} sessions into {}",
                home.join("library/index.sqlite").display()
            );
        }
        cli::LibraryAction::Search {
            query,
            session,
            limit,
        } => {
            lib.sync(home, &config).map_err(AppError::Usage)?;
            let session = session
                .map(|s| session_dir(home, &config, Some(&s)))
                .transpose()?
                .map(|d| {
                    d.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                });
            let hits = lib
                .search(&query, session.as_deref(), limit)
                .map_err(AppError::Usage)?;
            if hits.is_empty() {
                println!("no matches");
            }
            for h in hits {
                println!("{}  {}  {}", h.session, ac_core::time::hms(h.start), h.text);
            }
        }
    }
    Ok(exit::OK)
}

/// Opens the rehearse page in the default browser.
fn open_rehearse_page(port: u16) {
    let url = format!("http://127.0.0.1:{port}/rehearse");
    #[cfg(windows)]
    let opened = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .spawn();
    #[cfg(target_os = "macos")]
    let opened = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let opened = std::process::Command::new("xdg-open").arg(&url).spawn();
    if let Err(e) = opened {
        tracing::warn!("opening {url}: {e}");
    }
}

/// A session folder from a path, a folder name like `2026-11-19_ep042`, an episode (`42`,
/// `ep042`), `rehearse`, or the last real session.
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
    ac_library::find_session(home, config, arg).ok_or_else(|| {
        AppError::Usage(format!(
            "no session {arg:?} (a folder name under {}, an episode like 42, rehearse, or a path)",
            ac_core::session::sessions_dir(home, config).display()
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
