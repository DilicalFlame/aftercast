//! `aftercast doctor`: config, external tools, transcriber/CUDA, clock skew and keyring.

use std::ffi::OsStr;
use std::fmt::Display;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use ac_core::config::{Config, Transcriber, is_secret_ref};
use ac_core::{Home, exit};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ok,
    Warn,
    /// Failed; carries the exit code this failure maps to.
    Fail(u8),
}

#[derive(Debug)]
struct Check {
    name: String,
    status: Status,
    detail: String,
}

impl Check {
    fn new(name: impl Into<String>, status: Status, detail: impl Display) -> Self {
        Self {
            name: name.into(),
            status,
            detail: detail.to_string(),
        }
    }
}

/// Runs all checks, prints a table and returns the process exit code.
pub fn run(home: &Home, config_path: Option<PathBuf>) -> u8 {
    let path = config_path.unwrap_or_else(|| home.config_file());
    let mut checks = Vec::new();
    let config = match Config::load(&path) {
        Ok(config) => {
            checks.push(Check::new("config", Status::Ok, path.display()));
            config
        }
        Err(e) => {
            checks.push(Check::new("config", Status::Fail(e.exit_code()), &e));
            Config::default()
        }
    };
    if !is_secret_ref(&config.obs.password) {
        checks.push(Check::new(
            "secrets",
            Status::Warn,
            "obs.password is plain text; use \"keyring:obs\" or \"env:<NAME>\"",
        ));
    }

    let paths = &config.paths;
    checks.push(tool("ffmpeg", &paths.ffmpeg, &["-version"], true));
    checks.push(tool("ffprobe", &paths.ffprobe, &["-version"], true));
    checks.push(tool(
        "yt-dlp",
        &paths.yt_dlp,
        &["--version"],
        config.post.use_vod,
    ));
    checks.push(tool(
        "tesseract",
        &paths.tesseract,
        &["--version"],
        config.post.ocr,
    ));
    checks.extend(transcriber(&config));
    checks.push(clock());
    checks.push(keyring());
    checks.push(obs(&config));

    print_table(&checks);
    exit_code(&checks)
}

/// OBS reachability and configured scenes. Only a warning: OBS may simply not be running.
fn obs(config: &Config) -> Check {
    let hint = "start OBS (30.2+), enable Tools > WebSocket Server Settings";
    let password = crate::secret::resolve(&config.obs.password).unwrap_or(None);
    let probe = ac_obs::Target::parse(&config.obs.url, password).and_then(|target| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), ac_obs::probe(&target))
                .await
                .unwrap_or_else(|_| Err("timed out".into()))
        })
    });
    match probe {
        Ok(snap) => {
            let missing: Vec<&str> = config
                .obs
                .scenes
                .all()
                .into_iter()
                .filter(|s| !snap.scenes.iter().any(|have| have == s))
                .collect();
            if missing.is_empty() {
                Check::new("obs", Status::Ok, format!("{} scenes", snap.scenes.len()))
            } else {
                let detail = format!("missing scenes: {}", missing.join(", "));
                Check::new("obs", Status::Warn, detail)
            }
        }
        Err(e) => Check::new("obs", Status::Warn, format!("{e} ({hint})")),
    }
}

/// Overall exit code: the highest-severity failure (config > missing tool > auth > other).
/// Warnings never fail the run.
fn exit_code(checks: &[Check]) -> u8 {
    fn severity(code: u8) -> u8 {
        match code {
            exit::CONFIG => 4,
            exit::TOOL_MISSING => 3,
            exit::AUTH => 2,
            _ => 1,
        }
    }
    checks
        .iter()
        .filter_map(|c| match c.status {
            Status::Fail(code) => Some(code),
            Status::Ok | Status::Warn => None,
        })
        .max_by_key(|&code| severity(code))
        .unwrap_or(exit::OK)
}

fn print_table(checks: &[Check]) {
    let width = checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for c in checks {
        let status = match c.status {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail(_) => "FAIL",
        };
        println!("{status:<4}  {:<width$}  {}", c.name, c.detail);
    }
}

/// Checks that an external tool runs; missing required tools fail with exit code 4.
fn tool(name: &str, program: &Path, args: &[&str], required: bool) -> Check {
    match first_line(program, args) {
        Ok(line) => Check::new(name, Status::Ok, line),
        Err(e) if required => Check::new(name, Status::Fail(exit::TOOL_MISSING), e),
        Err(e) => Check::new(name, Status::Warn, format!("{e} (optional)")),
    }
}

fn transcriber(config: &Config) -> Vec<Check> {
    let paths = &config.paths;
    let package = match config.post.transcriber {
        Transcriber::FasterWhisper => "faster-whisper",
        Transcriber::Whisperx => "whisperx",
        Transcriber::WhisperCpp => {
            // whisper.cpp also runs on CPU, so a missing GPU is only a warning.
            let gpu = match first_line("nvidia-smi", &["--query-gpu=name", "--format=csv,noheader"])
            {
                Ok(name) => Check::new("cuda", Status::Ok, name),
                Err(e) => Check::new("cuda", Status::Warn, e),
            };
            return vec![
                tool("whisper-cli", &paths.whisper_cli, &["--help"], true),
                gpu,
            ];
        }
    };
    let python = &paths.python;
    let version =
        format!("import importlib.metadata as m; print('{package}', m.version('{package}'))");
    let cuda = match first_line(
        python,
        &[
            "-c",
            "import ctranslate2; print(ctranslate2.get_cuda_device_count())",
        ],
    ) {
        Ok(n) if n.parse::<u32>().is_ok_and(|n| n > 0) => Check::new(
            "cuda",
            Status::Ok,
            format!("{n} CUDA device(s) visible to ctranslate2"),
        ),
        Ok(_) => Check::new(
            "cuda",
            Status::Fail(exit::GENERIC),
            "no CUDA device visible to ctranslate2 (needs CUDA 12 + cuDNN)",
        ),
        Err(e) => Check::new("cuda", Status::Fail(exit::GENERIC), e),
    };
    vec![
        tool("python", python, &["--version"], true),
        tool(package, python, &["-c", &version], true),
        cuda,
    ]
}

/// Runs `program args`; returns the first output line, or on failure the last one.
fn first_line(program: impl AsRef<OsStr>, args: &[&str]) -> Result<String, String> {
    let program = program.as_ref();
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let text = String::from_utf8_lossy(if out.stdout.is_empty() {
        &out.stderr
    } else {
        &out.stdout
    });
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if out.status.success() {
        Ok(lines.next().unwrap_or_default().to_owned())
    } else {
        // The last line names the problem (e.g. a Python traceback's exception).
        let last = lines.next_back().unwrap_or_default();
        Err(format!("{} ({}): {last}", program.display(), out.status))
    }
}

fn clock() -> Check {
    match clock_skew() {
        Ok(skew) if skew.abs() <= 1.0 => Check::new(
            "clock",
            Status::Ok,
            format!("{skew:+.1} s vs googleapis.com"),
        ),
        Ok(skew) => Check::new(
            "clock",
            Status::Warn,
            format!("{skew:+.1} s vs googleapis.com; sync the system clock"),
        ),
        Err(e) => Check::new("clock", Status::Warn, format!("not checked: {e}")),
    }
}

/// Seconds the local clock is ahead of googleapis.com's `Date` header.
///
/// Plain HTTP is enough: only the header's time is used, nothing is trusted.
/// ponytail: `Date` has 1 s resolution, so this is +-0.5 s; fine for a 1 s threshold.
fn clock_skew() -> io::Result<f64> {
    const HOST: &str = "www.googleapis.com";
    let timeout = Duration::from_secs(3);
    let addr = (HOST, 80)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::other("no address"))?;
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    let sent = SystemTime::now();
    write!(
        stream,
        "HEAD / HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let received = SystemTime::now();

    let date = response
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("date"))
        .map(|(_, v)| v.trim())
        .ok_or_else(|| io::Error::other("no Date header"))?;
    // The header truncates to whole seconds; the true server time is half a second later on average.
    let server =
        httpdate::parse_http_date(date).map_err(io::Error::other)? + Duration::from_millis(500);
    let local = sent + received.duration_since(sent).unwrap_or_default() / 2;
    Ok(match local.duration_since(server) {
        Ok(ahead) => ahead.as_secs_f64(),
        Err(behind) => -behind.duration().as_secs_f64(),
    })
}

fn keyring() -> Check {
    match keyring::Entry::new("aftercast", "doctor-probe").and_then(|e| e.get_password()) {
        Ok(_) | Err(keyring::Error::NoEntry) => {
            Check::new("keyring", Status::Ok, "OS credential store reachable")
        }
        Err(e) => Check::new("keyring", Status::Fail(exit::GENERIC), e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks(statuses: &[Status]) -> Vec<Check> {
        statuses.iter().map(|&s| Check::new("x", s, "")).collect()
    }

    #[test]
    fn exit_code_is_highest_severity_failure() {
        use Status::{Fail, Ok, Warn};
        assert_eq!(exit_code(&checks(&[Ok, Warn, Ok])), exit::OK);
        assert_eq!(
            exit_code(&checks(&[Warn, Fail(exit::GENERIC)])),
            exit::GENERIC
        );
        assert_eq!(
            exit_code(&checks(&[Fail(exit::TOOL_MISSING), Fail(exit::GENERIC)])),
            exit::TOOL_MISSING
        );
        let failing = [
            Fail(exit::CONFIG),
            Fail(exit::TOOL_MISSING),
            Fail(exit::GENERIC),
        ];
        for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0], [2, 0, 1]] {
            let rows: Vec<_> = order.iter().map(|&i| failing[i]).collect();
            assert_eq!(exit_code(&checks(&rows)), exit::CONFIG, "{order:?}");
        }
    }
}
