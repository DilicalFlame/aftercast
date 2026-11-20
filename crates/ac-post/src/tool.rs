//! Running external tools: the full command line goes to the stage log, stderr is appended
//! to it, and ffmpeg's `-progress` output drives the progress bar.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use ac_core::Home;
use std::process::{Command, Stdio};

use crate::{Ctx, Result};

const HELPERS: [(&str, &str); 2] = [
    (
        "transcribe.py",
        include_str!("../../../python/transcribe.py"),
    ),
    ("ocr.py", include_str!("../../../python/ocr.py")),
];

/// Writes the bundled Python helpers to `<home>/state` and returns that folder.
pub fn write_helpers(home: &Home) -> Result<PathBuf> {
    let dir = home.join("state");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for (name, text) in HELPERS {
        let path = dir.join(name);
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(dir)
}

/// Runs `program` and returns its stdout; stderr goes to the stage log.
pub fn run<I, S>(ctx: &Ctx, program: &str, args: I) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(program);
    cmd.args(args);
    let output = spawn_logged(ctx, &mut cmd)?
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{program} exited with {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs `program`, calling `on_line` for each stdout line as it arrives.
pub fn run_lines<I, S>(
    ctx: &Ctx,
    program: &str,
    args: I,
    mut on_line: impl FnMut(&str),
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut cmd = Command::new(program);
    cmd.args(args);
    let mut child = spawn_logged(ctx, &mut cmd)?;
    let stdout = child.stdout.take().expect("piped stdout");
    for line in BufReader::new(stdout)
        .lines()
        .map_while(std::result::Result::ok)
    {
        on_line(&line);
    }
    let status = child.wait().map_err(|e| format!("{program}: {e}"))?;
    if !status.success() {
        let log = fs::read_to_string(ctx.log_file()).unwrap_or_default();
        let last = log
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!("{program} exited with {status}: {last}"));
    }
    Ok(())
}

/// Runs ffmpeg with `-progress pipe:1`, reporting progress against `total_s` seconds of
/// output. Returns the stderr text written during this call (for filters like
/// `silencedetect` that report there).
pub fn ffmpeg<I, S>(ctx: &mut Ctx, args: I, total_s: f64) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    note_version(ctx, "ffmpeg");
    let log = ctx.log_file();
    let before = fs::metadata(&log).map(|m| m.len()).unwrap_or(0);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostats", "-y", "-progress", "pipe:1"]);
    cmd.args(args);
    let mut child = spawn_logged(ctx, &mut cmd)?;
    let stdout = child.stdout.take().expect("piped stdout");
    for line in BufReader::new(stdout)
        .lines()
        .map_while(std::result::Result::ok)
    {
        if let Some(us) = line.strip_prefix("out_time_us=")
            && let Ok(us) = us.trim().parse::<f64>()
            && total_s > 0.0
        {
            ctx.set_progress(us / 1e6 / total_s);
        }
    }
    let status = child.wait().map_err(|e| format!("ffmpeg: {e}"))?;
    let text = fs::read(&log).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&text[(before as usize).min(text.len())..]).into_owned();
    if !status.success() {
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!("ffmpeg exited with {status}: {last}"));
    }
    ctx.set_progress(1.0);
    Ok(stderr)
}

fn spawn_logged(ctx: &Ctx, cmd: &mut Command) -> Result<std::process::Child> {
    let log_path = ctx.log_file();
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("{}: {e}", log_path.display()))?;
    let line = std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ");
    tracing::info!("{line}");
    let _ = writeln!(log, "$ {line}");
    let _ = log.flush();
    let program = cmd.get_program().to_string_lossy().into_owned();
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log))
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{program} not found on PATH (aftercast doctor checks the tools)")
            } else {
                format!("{program}: {e}")
            }
        })
}

fn quote(arg: &OsStr) -> String {
    let s = arg.to_string_lossy();
    if s.is_empty() || s.contains([' ', '"', '\'']) {
        format!("\"{}\"", s.replace('"', "\\\""))
    } else {
        s.into_owned()
    }
}

/// Records the first line of `<tool> -version` for `.stages.json`.
pub fn note_version(ctx: &mut Ctx, tool: &str) {
    if ctx.has_tool(tool) {
        return;
    }
    if let Ok(out) = Command::new(tool).arg("-version").output() {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(first) = text.lines().next() {
            ctx.note_tool(tool, first.trim().to_owned());
        }
    }
}

/// `ffprobe` JSON for `path`.
pub fn probe(ctx: &Ctx, path: &Path) -> Result<serde_json::Value> {
    let out = run(
        ctx,
        "ffprobe",
        [
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-print_format"),
            OsStr::new("json"),
            OsStr::new("-show_format"),
            OsStr::new("-show_streams"),
            path.as_os_str(),
        ],
    )?;
    serde_json::from_str(&out).map_err(|e| format!("ffprobe output for {}: {e}", path.display()))
}
