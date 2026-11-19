//! Stage 8, frames (`derived/frames/*.jpg`, `derived/frames.json`), and stage 9, ocr.

use std::ffi::OsString;
use std::fs;

use image_hasher::{HashAlg, HasherConfig, ImageHash};
use serde::{Deserialize, Serialize};

use crate::ctx::{inside, read_json, write_json};
use crate::media::round_ms;
use crate::{Ctx, Result, tool};

/// A scene change scoring above this is kept even when it looks like the previous frame.
const KEEP_SCENE_SCORE: f64 = 0.6;
/// Scene changes closer together than this are one change.
const BURST_S: f64 = 2.0;
/// Scene detection looks at this many frames per second (decoding stays the cost).
const SCENE_FPS: u32 = 5;
/// OCR text kept per frame.
const OCR_MAX: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frames {
    pub interval_s: f64,
    pub width: u32,
    pub frames: Vec<Frame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub t: f64,
    pub file: String,
    pub reason: Reason,
    pub scene_score: Option<f64>,
    pub phash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocr: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Interval,
    Scene,
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let master = ctx.master()?;
    let video = ctx.master_video()?;
    let p = ctx.config.post.clone();
    let duration = master.duration_s;

    ctx.set_message("finding scene changes");
    let filter = format!(
        "fps={SCENE_FPS},scale=640:-1,select='gt(scene,{})',metadata=print:key=lavfi.scene_score",
        p.scene_threshold
    );
    let args: Vec<OsString> = vec![
        "-i".into(),
        video.clone().into(),
        "-an".into(),
        "-vf".into(),
        filter.into(),
        "-fps_mode".into(),
        "vfr".into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    let stderr = tool::ffmpeg(ctx, args, duration)?;
    let step = f64::from(p.frame_interval_s.max(1));
    let mut candidates = merge(parse_scenes(&stderr), step, duration);
    let privacy = ctx.privacy(duration);
    candidates.retain(|(t, _, _)| !inside(&privacy, *t));

    let dir = ctx.derived("frames");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let hasher = HasherConfig::new()
        .hash_alg(HashAlg::DoubleGradient)
        .hash_size(16, 16)
        .to_hasher();
    let mut kept: Vec<Frame> = Vec::new();
    let mut last: Option<ImageHash> = None;
    let total = candidates.len();
    for (i, (t, reason, score)) in candidates.into_iter().enumerate() {
        ctx.set_message(format!("frame {}/{total}", i + 1));
        let mut name = file_name(t, false);
        if kept.iter().any(|f| f.file == name) {
            name = file_name(t, true);
        }
        let path = dir.join(&name);
        let args: Vec<OsString> = vec![
            "-ss".into(),
            format!("{t:.3}").into(),
            "-i".into(),
            video.clone().into(),
            "-frames:v".into(),
            "1".into(),
            "-vf".into(),
            format!("scale={}:-2", p.frame_width).into(),
            "-q:v".into(),
            "4".into(),
            path.clone().into(),
        ];
        if let Err(e) = tool::ffmpeg(ctx, args, 0.0) {
            tracing::warn!("frame at {t:.3} s: {e}");
        }
        ctx.set_progress((i + 1) as f64 / total.max(1) as f64);
        let Ok(img) = image::open(&path) else {
            // Past the last decodable frame.
            let _ = fs::remove_file(&path);
            continue;
        };
        let hash = hasher.hash_image(&img);
        let similar = last
            .as_ref()
            .is_some_and(|l| l.dist(&hash) <= p.phash_max_distance);
        if similar && score.is_none_or(|s| s <= KEEP_SCENE_SCORE) {
            let _ = fs::remove_file(&path);
            continue;
        }
        kept.push(Frame {
            t: round_ms(t),
            file: name,
            reason,
            scene_score: score.map(round_ms),
            phash: hash.to_base64(),
            ocr: None,
        });
        last = Some(hash);
    }
    write_json(
        &ctx.derived("frames.json"),
        &Frames {
            interval_s: step,
            width: p.frame_width,
            frames: kept,
        },
    )
}

/// Stage 9: tesseract text per frame, merged into `frames.json`.
pub fn ocr(ctx: &mut Ctx) -> Result<()> {
    let path = ctx.derived("frames.json");
    let mut frames: Frames = read_json(&path)?;
    let dir = ctx.derived("ocr");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    tool::note_version(ctx, "tesseract");
    let total = frames.frames.len();
    for (i, f) in frames.frames.iter_mut().enumerate() {
        let image = ctx.derived("frames").join(&f.file);
        let text = tool::run(
            ctx,
            "tesseract",
            [
                image.into_os_string(),
                "-".into(),
                "--psm".into(),
                "6".into(),
            ],
        )?;
        let text: String = text.trim().chars().take(OCR_MAX).collect();
        let txt = dir.join(f.file.replace(".jpg", ".txt"));
        fs::write(&txt, &text).map_err(|e| format!("{}: {e}", txt.display()))?;
        f.ocr = Some(text).filter(|t| !t.is_empty());
        ctx.set_progress((i + 1) as f64 / total.max(1) as f64);
    }
    write_json(&path, &frames)
}

/// Interval candidates every `step` seconds plus scene changes. A burst of scene changes (an
/// OBS transition animates for a second or so) becomes one candidate at its last change,
/// where the picture has settled, scored with the burst's highest score; interval
/// candidates that close to a scene change are dropped.
fn merge(scenes: Vec<(f64, f64)>, step: f64, duration: f64) -> Vec<(f64, Reason, Option<f64>)> {
    let mut bursts: Vec<(f64, f64)> = Vec::new();
    for (t, score) in scenes {
        match bursts.last_mut() {
            Some(last) if t - last.0 < BURST_S => *last = (t, last.1.max(score)),
            _ => bursts.push((t, score)),
        }
    }
    let mut out: Vec<(f64, Reason, Option<f64>)> = bursts
        .iter()
        .map(|(t, s)| (*t, Reason::Scene, Some(*s)))
        .collect();
    let mut t = 0.0;
    while t < duration {
        if !bursts.iter().any(|(s, _)| (s - t).abs() < BURST_S) {
            out.push((t, Reason::Interval, None));
        }
        t += step;
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// `HHhMMmSSs.jpg`, or `HHhMMmSSs_mmm.jpg` to tell apart frames in the same second.
fn file_name(t: f64, millis: bool) -> String {
    let ms = (t * 1000.0).round() as u64;
    let s = ms / 1000;
    let base = format!("{:02}h{:02}m{:02}s", s / 3600, s / 60 % 60, s % 60);
    if millis {
        format!("{base}_{:03}.jpg", ms % 1000)
    } else {
        format!("{base}.jpg")
    }
}

/// `pts_time` and `lavfi.scene_score` pairs from `metadata=print`.
fn parse_scenes(stderr: &str) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    let mut pts = None;
    for line in stderr.lines() {
        if let Some(i) = line.find("pts_time:") {
            pts = line[i + 9..]
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok());
        } else if let Some(i) = line.find("lavfi.scene_score=")
            && let (Some(t), Ok(score)) = (pts.take(), line[i + 18..].trim().parse::<f64>())
        {
            out.push((t, score));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_bursts_collapse_and_replace_nearby_intervals() {
        let scenes = vec![(245.2, 0.4), (245.55, 0.9), (246.05, 0.5), (400.0, 0.7)];
        let got = merge(scenes, 30.0, 420.0);
        let scene: Vec<(f64, Option<f64>)> = got
            .iter()
            .filter(|c| c.1 == Reason::Scene)
            .map(|c| (c.0, c.2))
            .collect();
        assert_eq!(scene, [(246.05, Some(0.9)), (400.0, Some(0.7))]);
        let intervals: Vec<f64> = got
            .iter()
            .filter(|c| c.1 == Reason::Interval)
            .map(|c| c.0)
            .collect();
        assert_eq!(intervals.len(), 14);
        assert!(intervals.contains(&240.0) && intervals.contains(&390.0));
    }

    #[test]
    fn names_sort_and_disambiguate() {
        assert_eq!(file_name(3930.25, false), "01h05m30s.jpg");
        assert_eq!(file_name(3930.25, true), "01h05m30s_250.jpg");
    }

    #[test]
    fn parses_scene_scores() {
        let stderr = "[Parsed_metadata_2 @ 0x1] frame:0    pts:1200  pts_time:12.5\n\
                      [Parsed_metadata_2 @ 0x1] lavfi.scene_score=0.452\n\
                      [Parsed_metadata_2 @ 0x1] frame:1    pts:4000  pts_time:40\n\
                      [Parsed_metadata_2 @ 0x1] lavfi.scene_score=0.9\n";
        assert_eq!(parse_scenes(stderr), [(12.5, 0.452), (40.0, 0.9)]);
    }
}
