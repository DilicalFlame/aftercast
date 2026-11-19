//! Stage 1, ingest: one `media/master.mp4` from the session's recordings, and
//! `media/master.json` with how each file maps onto it.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::ctx::{Master, MasterFile, write_json};
use crate::{Ctx, Result, tool};

/// Recorded length differing from the anchors by more than this is logged.
const DRIFT_WARN_S: f64 = 0.5;

pub fn run(ctx: &mut Ctx) -> Result<()> {
    if ctx.recordings.is_empty() {
        return Err(
            "no recordings in media/recordings.json (was OBS recording during the session?)".into(),
        );
    }
    let mut files = Vec::new();
    let mut offset = 0.0;
    let mut video = (0.0, 0, 0);
    for r in ctx.recordings.clone() {
        let path = PathBuf::from(&r.path);
        if !path.exists() {
            return Err(format!("recording {} is missing", path.display()));
        }
        let info = tool::probe(ctx, &path)?;
        let duration = number(&info["format"]["duration"])
            .ok_or_else(|| format!("ffprobe gave no duration for {}", path.display()))?;
        if video.1 == 0 {
            video = video_stream(&info);
        }
        let expected = r.stop.map(|stop| {
            let paused: f64 = r.pauses.iter().map(|p| p.to.secs_since(p.from)).sum();
            stop.secs_since(r.start) - paused
        });
        if let Some(e) = expected
            && (e - duration).abs() > DRIFT_WARN_S
        {
            // ponytail: ffprobe's duration is trusted as is; scaling times linearly within the
            // file (spec 05) waits until a real recording shows drift.
            tracing::warn!(
                "{}: {duration:.3} s on disk, {e:.3} s from the anchors",
                path.display()
            );
        }
        files.push(MasterFile {
            path: r.path.clone(),
            offset_s: offset,
            duration_s: duration,
            expected_s: expected,
        });
        offset += duration;
    }

    let master = ctx.media("master.mp4");
    let _ = fs::remove_file(&master);
    let single_mp4 = files.len() == 1 && is_mp4(Path::new(&files[0].path));
    let path = if single_mp4 {
        // Same volume: a hard link costs nothing; otherwise use the file where it is.
        match fs::hard_link(&files[0].path, &master) {
            Ok(()) => "media/master.mp4".to_owned(),
            Err(_) => files[0].path.clone(),
        }
    } else {
        ctx.set_message(format!("joining {} file(s)", files.len()));
        let list = ctx.media("concat.txt");
        let lines: String = files
            .iter()
            .map(|f| format!("file '{}'\n", f.path.replace('\'', r"'\''")))
            .collect();
        fs::write(&list, lines).map_err(|e| format!("{}: {e}", list.display()))?;
        let args: Vec<OsString> = vec![
            "-f".into(),
            "concat".into(),
            "-safe".into(),
            "0".into(),
            "-i".into(),
            list.clone().into(),
            "-map".into(),
            "0".into(),
            "-c".into(),
            "copy".into(),
            "-movflags".into(),
            "+faststart".into(),
            master.clone().into(),
        ];
        tool::ffmpeg(ctx, args, offset)?;
        let _ = fs::remove_file(&list);
        "media/master.mp4".to_owned()
    };
    let (fps, width, height) = video;
    write_json(
        &ctx.media("master.json"),
        &Master {
            path,
            duration_s: offset,
            fps,
            width,
            height,
            files,
        },
    )
}

fn is_mp4(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4"))
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

/// (fps, width, height) of the first video stream.
fn video_stream(info: &Value) -> (f64, u32, u32) {
    let Some(s) = info["streams"]
        .as_array()
        .and_then(|a| a.iter().find(|s| s["codec_type"] == "video"))
    else {
        return (0.0, 0, 0);
    };
    let fps = s["avg_frame_rate"]
        .as_str()
        .and_then(|r| r.split_once('/'))
        .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?))
        .filter(|f| f.is_finite())
        .unwrap_or(0.0);
    let dim = |k: &str| s[k].as_u64().unwrap_or(0) as u32;
    (fps, dim("width"), dim("height"))
}
