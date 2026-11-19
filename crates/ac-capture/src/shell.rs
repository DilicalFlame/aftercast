//! Shell command capture (specs/10-capture.md, "Shell commands"): the shell appends to a
//! spool file; a session tails it into `shell.jsonl` with secrets redacted.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use ac_core::config;
use ac_core::time::Timestamp;
use serde::{Deserialize, Serialize};

/// The spool is cut to its last [`SPOOL_KEEP`] bytes once it grows past this.
pub const SPOOL_MAX: u64 = 50 * 1024 * 1024;
pub const SPOOL_KEEP: u64 = 10 * 1024 * 1024;

/// One command, as in the spool and in `shell.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShellLine {
    pub t: Timestamp,
    pub cwd: String,
    pub cmd: String,
}

/// Replaces every secret match with `[REDACTED]`.
pub fn redact<'a>(text: &'a str, secrets: &[config::Pattern]) -> Cow<'a, str> {
    let mut out = Cow::Borrowed(text);
    for p in secrets {
        if p.0.is_match(&out) {
            out = Cow::Owned(p.0.replace_all(&out, "[REDACTED]").into_owned());
        }
    }
    out
}

/// Reads complete new lines from the spool.
pub struct Spool {
    path: PathBuf,
    offset: u64,
}

impl Spool {
    /// Starts at the beginning; [`Spool::read_new`] then returns the whole file once.
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: 0,
        }
    }

    /// Commands appended since the last call. A partly written last line waits for the
    /// next call; lines that are not valid records are skipped.
    pub fn read_new(&mut self) -> io::Result<Vec<ShellLine>> {
        let mut file = match fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        if file.metadata()?.len() < self.offset {
            // Trimmed or replaced: start over.
            self.offset = 0;
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let complete = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        self.offset += complete as u64;
        Ok(String::from_utf8_lossy(&buf[..complete])
            .lines()
            .filter_map(parse)
            .collect())
    }
}

fn parse(line: &str) -> Option<ShellLine> {
    // Windows PowerShell 5.1 writes a BOM when it creates the file.
    let line = line.trim_start_matches('\u{feff}').trim();
    serde_json::from_str(line).ok()
}

/// Cuts the spool to its last [`SPOOL_KEEP`] bytes (at a line start) once it is larger
/// than [`SPOOL_MAX`]. Run before tailing starts.
pub fn trim(path: &Path) -> io::Result<bool> {
    let len = match fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if len <= SPOOL_MAX {
        return Ok(false);
    }
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(len - SPOOL_KEEP))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    drop(file);
    let start = tail
        .iter()
        .position(|b| *b == b'\n')
        .map_or(tail.len(), |i| i + 1);
    fs::write(path, &tail[start..])?;
    Ok(true)
}

/// The `$PROFILE` snippet for PowerShell. It keeps any existing history handler (so
/// PSReadLine still keeps lines that look sensitive out of history) and only spools what
/// would be saved to the history file.
pub fn pwsh_hook(spool: &Path) -> String {
    let spool = spool.display().to_string().replace('\'', "''");
    format!(
        r#"# aftercast shell integration
if (-not $global:AftercastSpool) {{
  $global:AftercastSpool = '{spool}'
  $global:AftercastPrevHistory = (Get-PSReadLineOption).AddToHistoryHandler
  Set-PSReadLineOption -AddToHistoryHandler {{
    param($line)
    $keep = if ($global:AftercastPrevHistory) {{ $global:AftercastPrevHistory.Invoke($line) }} else {{ $true }}
    if ("$keep" -in 'True', 'MemoryAndFile') {{
      try {{
        $rec = @{{ t = [DateTime]::UtcNow.ToString('o'); cwd = (Get-Location).Path; cmd = $line }} | ConvertTo-Json -Compress
        Add-Content -LiteralPath $global:AftercastSpool -Value $rec -Encoding utf8
      }} catch {{ }}
    }}
    return $keep
  }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn redacts_every_secret() {
        let secrets = config::Capture::default().secret_patterns;
        let token = format!("ghp_{}", "x".repeat(36));
        let line =
            format!("git push https://{token}@github.com/me/repo && echo AKIAABCDEFGHIJKLMNOP");
        let out = redact(&line, &secrets);
        assert_eq!(
            out,
            "git push https://[REDACTED]@github.com/me/repo && echo [REDACTED]"
        );
        assert!(matches!(redact("cargo test", &secrets), Cow::Borrowed(_)));
    }

    #[test]
    fn tails_complete_lines_only_and_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spool.jsonl");
        let mut spool = Spool::new(&path);
        assert!(spool.read_new().unwrap().is_empty(), "missing file");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            "\u{feff}{{\"cmd\":\"cargo test\",\"cwd\":\"P:\\\\OSS\",\"t\":\"2026-11-19T10:02:11.4810000Z\"}}\nnot json\n{{\"cmd\":\"git st"
        )
        .unwrap();
        let lines = spool.read_new().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0].cmd.as_str(), lines[0].cwd.as_str()),
            ("cargo test", "P:\\OSS")
        );
        writeln!(
            f,
            "atus\",\"cwd\":\"C:\\\\\",\"t\":\"2026-11-19T10:02:12Z\"}}"
        )
        .unwrap();
        let lines = spool.read_new().unwrap();
        assert_eq!(lines[0].cmd, "git status");
        assert!(spool.read_new().unwrap().is_empty());
    }

    #[test]
    fn trims_big_spools_at_a_line_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("spool.jsonl");
        assert!(!trim(&path).unwrap());
        let line = format!("{}\n", "y".repeat(1023));
        let big = line.repeat((SPOOL_MAX / 1024 + 1) as usize);
        fs::write(&path, big).unwrap();
        assert!(trim(&path).unwrap());
        let kept = fs::read(&path).unwrap();
        assert!(kept.len() as u64 <= SPOOL_KEEP && kept.len() as u64 >= SPOOL_KEEP - 1024);
        assert!(kept.starts_with(b"yyy") && kept.ends_with(b"\n"));
    }

    #[test]
    fn hook_quotes_the_path() {
        let hook = pwsh_hook(Path::new(r"P:\it's\state\shell-spool.jsonl"));
        assert!(hook.contains(r"'P:\it''s\state\shell-spool.jsonl'"));
    }
}
