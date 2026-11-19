//! Stages 2 to 4 and 6: VOD download, VOD calibration, 16 kHz audio, silence detection.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use ac_core::time::find_lag;
use serde::Serialize;
use serde_json::json;

use crate::ctx::write_json;
use crate::{Ctx, Result, tool};

/// Calibration rate (spec 05: 8 kHz mono).
const CAL_RATE: u32 = 8000;
/// VOD audio compared: its first 10 minutes.
const VOD_PROBE_S: f64 = 600.0;
/// Master audio searched: 15 minutes around where the VOD should start.
const MASTER_WINDOW_S: f64 = 900.0;
/// Below this peak ratio the calibration is rejected.
const MIN_PEAK_RATIO: f64 = 3.0;

/// Stage 2: downloads the YouTube VOD with yt-dlp.
pub fn vod(ctx: &mut Ctx) -> Result<()> {
    tool::note_version(ctx, "yt-dlp");
    let url = format!(
        "https://www.youtube.com/watch?v={}",
        ctx.session.meta.video_id
    );
    let out = ctx.media("vod.mp4");
    // ponytail: no waiting for YouTube's processing (spec: poll up to 6 h); if the VOD is
    // not ready the stage fails and a later `aftercast post` resumes here.
    tool::run(
        ctx,
        "yt-dlp",
        [
            OsString::from("-f"),
            "bv*[height<=1080]+ba/b".into(),
            "--merge-output-format".into(),
            "mp4".into(),
            "--force-overwrites".into(),
            "-o".into(),
            out.into(),
            url.into(),
        ],
    )
    .map_err(|e| format!("{e} (VOD not ready yet, or private: see docs)"))?;
    Ok(())
}

#[derive(Serialize)]
struct Calibration {
    /// Master seconds where VOD 0:00 lies.
    lag_s: f64,
    peak_ratio: f64,
    accepted: bool,
    vod_offset_ms: i64,
}

/// Stage 3: finds where the VOD starts on the master by audio cross-correlation and stores
/// `vod_offset_ms` (spec 05, "Calibration").
pub fn calibrate(ctx: &mut Ctx) -> Result<()> {
    let start = ctx
        .session
        .meta
        .anchors
        .yt_actual_start
        .ok_or("the session has no YouTube start time (yt_actual_start)")?;
    let master = ctx.master()?;
    let expected = ctx
        .rec(start)
        .ok_or("the stream started outside the recording")?;
    let from = (expected - MASTER_WINDOW_S / 2.0).max(0.0);
    let video = ctx.master_video()?;
    let reference = pcm(ctx, &video, from, MASTER_WINDOW_S, "cal-master.raw")?;
    let probe = pcm(ctx, &ctx.media("vod.mp4"), 0.0, VOD_PROBE_S, "cal-vod.raw")?;
    if reference.len() <= probe.len() {
        return Err(format!(
            "the master ({:.0} s) is too short to calibrate against the VOD",
            master.duration_s
        ));
    }
    let lag = find_lag(&reference, &probe, CAL_RATE);
    let lag_s = from + lag.lag_s;
    let accepted = lag.peak_ratio >= MIN_PEAK_RATIO;
    let anchors = &mut ctx.session.meta.anchors;
    if accepted {
        anchors.vod_offset_ms = ((expected - lag_s) * 1000.0).round() as i64;
        ctx.session.save().map_err(|e| e.to_string())?;
    } else {
        tracing::warn!(
            "calibration rejected (peak ratio {:.2} < {MIN_PEAK_RATIO})",
            lag.peak_ratio
        );
    }
    write_json(
        &ctx.derived("calibration.json"),
        &Calibration {
            lag_s,
            peak_ratio: lag.peak_ratio,
            accepted,
            vod_offset_ms: ctx.session.meta.anchors.vod_offset_ms,
        },
    )
}

/// Mono f32 samples at [`CAL_RATE`] of `len` seconds from `from`.
fn pcm(ctx: &mut Ctx, input: &Path, from: f64, len: f64, name: &str) -> Result<Vec<f32>> {
    let out = ctx.derived(name);
    let args: Vec<OsString> = vec![
        "-ss".into(),
        format!("{from:.3}").into(),
        "-t".into(),
        format!("{len:.3}").into(),
        "-i".into(),
        input.into(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        CAL_RATE.to_string().into(),
        "-f".into(),
        "f32le".into(),
        out.clone().into(),
    ];
    tool::ffmpeg(ctx, args, len)?;
    let bytes = fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let _ = fs::remove_file(&out);
    Ok(bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect())
}

/// Stage 4: 16 kHz mono WAV for transcription and silence detection.
pub fn audio(ctx: &mut Ctx) -> Result<()> {
    let master = ctx.master()?;
    let video = ctx.master_video()?;
    let args: Vec<OsString> = vec![
        "-i".into(),
        video.into(),
        "-vn".into(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        "16000".into(),
        "-c:a".into(),
        "pcm_s16le".into(),
        ctx.media("audio16k.wav").into(),
    ];
    tool::ffmpeg(ctx, args, master.duration_s)?;
    Ok(())
}

/// Stage 6: silent ranges from ffmpeg's `silencedetect`.
pub fn silence(ctx: &mut Ctx) -> Result<()> {
    let master = ctx.master()?;
    let p = &ctx.config.post;
    let filter = format!(
        "silencedetect=noise={}dB:d={}",
        p.silence_db, p.silence_min_s
    );
    let args: Vec<OsString> = vec![
        "-i".into(),
        ctx.media("audio16k.wav").into(),
        "-af".into(),
        filter.into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    let stderr = tool::ffmpeg(ctx, args, master.duration_s)?;
    let ranges = parse_silence(&stderr, master.duration_s);
    let list: Vec<_> = ranges
        .iter()
        .map(|(s, e)| json!({ "start": round_ms(*s), "end": round_ms(*e) }))
        .collect();
    write_json(&ctx.derived("silence.json"), &list)
}

/// `silence_start: 12.3` / `silence_end: 18.9 | silence_duration: ...` pairs; an open start
/// runs to `duration`.
fn parse_silence(stderr: &str, duration: f64) -> Vec<(f64, f64)> {
    let value = |line: &str, key: &str| -> Option<f64> {
        let rest = &line[line.find(key)? + key.len()..];
        rest.split_whitespace().next()?.parse().ok()
    };
    let mut out = Vec::new();
    let mut open = None;
    for line in stderr.lines() {
        if let Some(s) = value(line, "silence_start: ") {
            open = Some(s.max(0.0));
        } else if let Some(e) = value(line, "silence_end: ")
            && let Some(s) = open.take()
        {
            out.push((s, e.min(duration)));
        }
    }
    if let Some(s) = open {
        out.push((s, duration));
    }
    out
}

pub fn round_ms(s: f64) -> f64 {
    (s * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_silencedetect_output() {
        let stderr = "[silencedetect @ 0x1] silence_start: -0.01\n\
                      [silencedetect @ 0x1] silence_end: 4.5 | silence_duration: 4.5\n\
                      size=N/A time=00:00:10.00\n\
                      [silencedetect @ 0x1] silence_start: 52.25\n";
        assert_eq!(parse_silence(stderr, 60.0), [(0.0, 4.5), (52.25, 60.0)]);
    }
}
