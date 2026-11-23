//! Automatic latency measurement (`stream.auto_latency`): the `latency` overlay keeps a small
//! counter on the stream; every few minutes the public HLS stream is fetched with yt-dlp, a
//! frame of its newest segment is cut to the overlay's frame, GLM-OCR reads the counter, and
//! the latency is the fetch time minus the time the counter showed that number.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use ac_core::Home;
use ac_core::config::Config;
use ac_core::time::{Clock, Timestamp};
use tokio::time::Instant;

/// Readings outside 0 to this many seconds are OCR mistakes (a lost or extra digit).
pub(super) const MAX_S: f64 = 900.0;
/// First retry delay after a failed measurement; doubles up to the interval.
pub(super) const RETRY: Duration = Duration::from_secs(60);

/// The running automatic measurement: the counter counts tenths of a second from `start`.
pub(super) struct Auto {
    pub start: Timestamp,
    /// No measurement before this.
    pub next: Instant,
    pub delay: Duration,
    pub busy: bool,
}

/// What one measurement needs; owned, so it can run on a blocking thread.
pub(super) struct Job {
    pub home: Home,
    pub config: Arc<Config>,
    pub clock: Arc<Clock>,
    pub video_id: String,
    pub start: Timestamp,
}

/// The number after "LAT" in what the OCR read (`LAT 1234`, `lat: 1234`).
pub(super) fn parse_counter(text: &str) -> Option<u64> {
    let text = text.to_ascii_lowercase();
    let rest = &text[text.find("lat")? + 3..];
    // The number must follow "lat" closely, not be some later digits.
    let gap = rest
        .find(|c: char| c.is_ascii_digit())
        .filter(|&g| g <= 4)?;
    let digits: String = rest[gap..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn run(program: &str, args: &[String]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err.lines().rev().find(|l| !l.trim().is_empty());
        return Err(format!("{program}: {}", last.unwrap_or("failed")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Seconds the stream is behind, read off the live stream now. Blocks for some seconds.
pub(super) fn measure(job: &Job) -> Result<f64, String> {
    let (home, config) = (&job.home, &job.config);
    if !home.llama_server().exists() {
        return Err("GLM-OCR is not installed (run aftercast setup)".into());
    }
    let url = format!("https://www.youtube.com/watch?v={}", job.video_id);
    let manifest = run(
        &config.paths.yt_dlp.to_string_lossy(),
        &[
            "-g".into(),
            "--no-warnings".into(),
            "-f".into(),
            "best[height<=720]/worst".into(),
            url,
        ],
    )?;
    let manifest = manifest
        .lines()
        .next()
        .ok_or("yt-dlp returned no stream URL")?;
    let work = home.join("state");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let frame = work.join("latency-frame.jpg");
    // The overlay's frame on the 1920x1080 canvas, as fractions of the video.
    let f = &config.overlay.latency;
    let (x, y) = (i32::from(f.x).max(0), i32::from(f.y).max(0));
    let (w, h) = (
        (1920 - x).min(i32::from(f.width)),
        (1080 - y).min(i32::from(f.height)),
    );
    let crop = format!("crop=iw*{w}/1920:ih*{h}/1080:iw*{x}/1920:ih*{y}/1080,scale=iw*2:ih*2");
    // ponytail: `at` is the fetch start, so the reading is off by up to one segment (2 s);
    // fine against minutes of latency, tighten with the segment's own timestamp if needed.
    let at = job.clock.now_ts();
    run(
        &config.paths.ffmpeg.to_string_lossy(),
        &[
            "-v".into(),
            "error".into(),
            "-y".into(),
            "-live_start_index".into(),
            "-1".into(),
            "-i".into(),
            manifest.into(),
            "-t".into(),
            "2".into(),
            "-vf".into(),
            crop,
            "-update".into(),
            "1".into(),
            "-q:v".into(),
            "2".into(),
            frame.to_string_lossy().into_owned(),
        ],
    )?;
    let text = ocr(job, &work, &frame)?;
    let counter = parse_counter(&text).ok_or_else(|| format!("no counter in {text:?}"))?;
    let shown = job.start.add_secs(counter as f64 / 10.0);
    Ok(at.secs_since(shown))
}

/// GLM-OCR on one frame, through the same helper the post pipeline uses.
fn ocr(job: &Job, work: &std::path::Path, frame: &std::path::Path) -> Result<String, String> {
    let (home, config) = (&job.home, &job.config);
    let script = ac_post::tool::write_helpers(home)?.join("ocr.py");
    let (list, out): (PathBuf, PathBuf) =
        (work.join("latency-ocr.txt"), work.join("latency-ocr.json"));
    std::fs::write(&list, frame.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let python = home.python(&config.paths.python);
    let result = run(
        &python.to_string_lossy(),
        &[
            script.to_string_lossy().into_owned(),
            "--engine".into(),
            "glm".into(),
            "--llama".into(),
            home.llama_dir().to_string_lossy().into_owned(),
            "--models".into(),
            home.models_dir().to_string_lossy().into_owned(),
            "--parallel".into(),
            "1".into(),
            "--list".into(),
            list.to_string_lossy().into_owned(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
        ],
    )
    .and_then(|_| std::fs::read_to_string(&out).map_err(|e| e.to_string()))
    .and_then(|json| {
        serde_json::from_str::<std::collections::HashMap<String, String>>(&json)
            .map_err(|e| e.to_string())
    });
    let _ = (
        std::fs::remove_file(&list),
        std::fs::remove_file(&out),
        std::fs::remove_file(frame),
    );
    result?
        .into_values()
        .next()
        .ok_or_else(|| "ocr returned nothing".into())
}

#[cfg(test)]
mod tests {
    use super::parse_counter;

    #[test]
    fn counter_follows_lat() {
        assert_eq!(parse_counter("LAT 1234"), Some(1234));
        assert_eq!(parse_counter("Lat: 77\nfn main() 2024"), Some(77));
        assert_eq!(parse_counter("lat12"), Some(12));
        assert_eq!(parse_counter("no counter 99"), None);
        assert_eq!(parse_counter("LAT then much later 55"), None);
    }
}
