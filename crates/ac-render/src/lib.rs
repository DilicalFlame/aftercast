//! Rendering (specs/12-ai-handoff-and-render.md): validates `ai/suggestions.json`, snaps its
//! ranges to word edges and pads them, and cuts each item from the master with ffmpeg,
//! with captions and a keystroke overlay.

mod subs;

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ac_core::config::{self, Captions, Encoder, KeysOverlay};
use ac_post::{Ctx, SegmentKind, Segments, Word, Words, overlap, read_json, tool, write_json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Result<T> = std::result::Result<T, String>;

/// A short longer than this is an error, whatever `short_max_s` says.
const SHORT_HARD_MAX_S: f64 = 180.0;
/// How far a range edge moves to meet a word edge.
const SNAP_S: f64 = 0.3;
/// Share of dead air in an item that earns a warning.
const DEAD_WARN: f64 = 0.3;
/// Ranges faster than this are muted and get no captions.
pub const MUTE_ABOVE: f64 = 4.0;

const VIDEO: &str = "clip.mp4";
const OUTPUTS: [&str; 7] = [
    VIDEO,
    "keys.mov",
    "keys.ass",
    "captions.srt",
    "thumb.jpg",
    "meta.json",
    "render.log",
];

/// `ai/suggestions.json` (schemas/suggestions.schema.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suggestions {
    pub schema_version: u32,
    pub session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Short,
    Clip,
    Topic,
    Compilation,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Clip => "clip",
            Self::Topic => "topic",
            Self::Compilation => "compilation",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub ranges: Vec<Range>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default = "yes")]
    pub keys_overlay: bool,
    #[serde(default = "yes")]
    pub captions: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: f64,
    pub end: f64,
    #[serde(default = "one")]
    pub speed: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn yes() -> bool {
    true
}

fn one() -> f64 {
    1.0
}

/// A range after snapping and padding, in master seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cut {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
    /// The range as suggested.
    pub suggested: [f64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Cut {
    /// Length in the output.
    pub fn out_len(&self) -> f64 {
        (self.end - self.start) / self.speed
    }
}

/// What the renderer needs to know about the session's timeline.
pub struct Timeline {
    pub duration: f64,
    pub fps: f64,
    /// Every transcribed word, by start time.
    pub words: Vec<Word>,
    pub redacted: Vec<(f64, f64)>,
    pub dead: Vec<(f64, f64)>,
}

impl Timeline {
    pub fn load(ctx: &Ctx) -> Result<Self> {
        let master = ctx.master()?;
        let segments: Segments = read_json(&ctx.derived("segments.json"))
            .map_err(|e| format!("{e} (run aftercast post first)"))?;
        let of = |kind| {
            segments
                .segments
                .iter()
                .filter(|s| s.kind == kind)
                .map(|s| (s.start, s.end))
                .collect()
        };
        let mut words: Vec<Word> = read_json::<Words>(&ctx.derived("words.json"))
            .map(|w| w.segments.into_iter().flat_map(|s| s.words).collect())
            .unwrap_or_default();
        words.sort_by(|a, b| a.start.total_cmp(&b.start));
        Ok(Self {
            duration: master.duration_s,
            fps: master.fps,
            words,
            redacted: of(SegmentKind::Redacted),
            dead: of(SegmentKind::Dead),
        })
    }

    /// Snaps `r` to the nearest word edges within [`SNAP_S`], then pads it. Padding stops
    /// at the master's ends and at redacted segments.
    fn cut(&self, r: &Range, cfg: &config::Render) -> Cut {
        let snap = |t: f64, edge: fn(&Word) -> f64| {
            self.words
                .iter()
                .map(edge)
                .filter(|e| (e - t).abs() <= SNAP_S)
                .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
                .unwrap_or(t)
        };
        let start = snap(r.start, |w| w.start);
        let end = snap(r.end, |w| w.end);
        let lo = self
            .redacted
            .iter()
            .map(|&(_, e)| e)
            .filter(|&e| e <= start)
            .fold(0.0, f64::max);
        let hi = self
            .redacted
            .iter()
            .map(|&(s, _)| s)
            .filter(|&s| s >= end)
            .fold(self.duration, f64::min);
        Cut {
            start: round_ms((start - f64::from(cfg.pad_start_ms) / 1000.0).max(lo)),
            end: round_ms((end + f64::from(cfg.pad_end_ms) / 1000.0).min(hi)),
            speed: r.speed,
            suggested: [r.start, r.end],
            label: r.label.clone(),
        }
    }
}

pub(crate) fn round_ms(s: f64) -> f64 {
    (s * 1000.0).round() / 1000.0
}

/// An item after validation: its cuts, warnings, and errors (an item with errors is not
/// rendered).
#[derive(Debug, Clone)]
pub struct Checked {
    pub item: Item,
    pub cuts: Vec<Cut>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
}

impl Checked {
    pub fn out_len(&self) -> f64 {
        self.cuts.iter().map(Cut::out_len).sum()
    }
}

/// Validates every item (specs/12, "Validation").
pub fn check(s: &Suggestions, session: &str, tl: &Timeline, cfg: &config::Render) -> Vec<Checked> {
    let mut ids = HashSet::new();
    let mut slugs = HashSet::new();
    s.items
        .iter()
        .map(|item| {
            let mut errors = Vec::new();
            let mut warnings = Vec::new();
            if !ids.insert(item.id.as_str()) {
                errors.push(format!("duplicate id {}", item.id));
            }
            if !slugs.insert(item.slug.as_str()) {
                errors.push(format!("duplicate slug {}", item.slug));
            }
            if item.id.is_empty()
                || item.id.len() > 8
                || !item.id.chars().all(|c| c.is_ascii_alphanumeric())
            {
                errors.push("id must be 1 to 8 letters or digits".into());
            }
            let slug_ok = item.slug.len() <= 60
                && item.slug.split('-').all(|p| {
                    !p.is_empty()
                        && p.chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                });
            if !slug_ok {
                errors.push(format!(
                    "slug {:?} must be lowercase words joined by '-' (max 60)",
                    item.slug
                ));
            }
            if item.title.chars().count() > 100 {
                errors.push("title is longer than 100 characters".into());
            }
            if item.ranges.is_empty() {
                errors.push("no ranges".into());
            }
            let mut cuts = Vec::new();
            for (i, r) in item.ranges.iter().enumerate() {
                let n = i + 1;
                if let Some(other) = r.session.as_deref().filter(|o| *o != session) {
                    // ponytail: one session per render; open the other session's master when
                    // cross-session compilations are needed.
                    errors.push(format!(
                        "range {n}: other sessions ({other}) are not supported yet"
                    ));
                    continue;
                }
                if !(0.5..=16.0).contains(&r.speed) {
                    errors.push(format!("range {n}: speed {} is outside 0.5 to 16", r.speed));
                }
                if !(r.start >= 0.0 && r.start < r.end && r.end <= tl.duration + 0.001) {
                    errors.push(format!(
                        "range {n}: {:.3} to {:.3} is not a range inside the master (0 to {:.3})",
                        r.start, r.end, tl.duration
                    ));
                    continue;
                }
                if overlap(&tl.redacted, r.start, r.end) > 0.0 {
                    errors.push(format!(
                        "range {n}: {:.3} to {:.3} overlaps a redacted segment",
                        r.start, r.end
                    ));
                    continue;
                }
                cuts.push(tl.cut(r, cfg));
            }
            // Back-to-back ranges: padding would repeat footage, so they meet halfway.
            for i in 1..cuts.len() {
                let (prev, next) = (cuts[i - 1].suggested[1], cuts[i].suggested[0]);
                if cuts[i - 1].end > cuts[i].start && prev <= next {
                    let mid = round_ms((prev + next) / 2.0);
                    cuts[i - 1].end = mid;
                    cuts[i].start = mid;
                }
            }
            let total: f64 = cuts.iter().map(|c| c.end - c.start).sum();
            let dead: f64 = cuts.iter().map(|c| overlap(&tl.dead, c.start, c.end)).sum();
            if total > 0.0 && dead / total > DEAD_WARN {
                warnings.push(format!("{:.0}% of it is dead air", dead / total * 100.0));
            }
            let checked = Checked {
                item: item.clone(),
                cuts,
                warnings,
                errors,
            };
            let out = checked.out_len();
            let mut checked = checked;
            if item.kind == Kind::Short {
                if out > SHORT_HARD_MAX_S {
                    checked
                        .errors
                        .push(format!("short is {out:.1} s, over {SHORT_HARD_MAX_S} s"));
                } else if out > f64::from(cfg.short_max_s) {
                    checked.warnings.push(format!(
                        "short is {out:.1} s, over short_max_s ({})",
                        cfg.short_max_s
                    ));
                }
            }
            checked
        })
        .collect()
}

pub struct Options {
    /// Only these ids; empty = all.
    pub ids: Vec<String>,
    pub dry_run: bool,
    pub force: bool,
    pub keys: KeysOverlay,
    pub captions: Captions,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// `--dry-run`: valid, not rendered.
    Valid,
    Rendered {
        duration_s: f64,
    },
    UpToDate,
    Failed(String),
}

/// `meta.json` of a rendered item.
#[derive(Debug, Serialize, Deserialize)]
pub struct Meta {
    pub item: Item,
    pub ranges: Vec<Cut>,
    pub outputs: Vec<String>,
    pub input_hash: String,
    pub keys_overlay: String,
    pub captions: String,
    pub commands: Vec<String>,
    pub expected_duration_s: f64,
    pub duration_s: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Validates the suggestions and renders the selected items. A failing item does not stop
/// the others.
pub fn run(ctx: &mut Ctx, opts: &Options) -> Result<Vec<(Checked, Outcome)>> {
    let path = ctx.dir.join("ai/suggestions.json");
    let suggestions: Suggestions = read_json(&path)?;
    if suggestions.schema_version != 1 {
        return Err(format!(
            "{}: schema_version {} (expected 1)",
            path.display(),
            suggestions.schema_version
        ));
    }
    if let Some(id) = opts
        .ids
        .iter()
        .find(|id| !suggestions.items.iter().any(|i| &i.id == *id))
    {
        return Err(format!("no item {id} in {}", path.display()));
    }
    let tl = Timeline::load(ctx)?;
    let session = ctx.session.name();
    let mut report = Vec::new();
    for checked in check(&suggestions, &session, &tl, &ctx.config.render) {
        if !opts.ids.is_empty() && !opts.ids.contains(&checked.item.id) {
            continue;
        }
        let outcome = if !checked.errors.is_empty() {
            Outcome::Failed(checked.errors.join("; "))
        } else if opts.dry_run {
            Outcome::Valid
        } else {
            render(ctx, &checked, &tl, opts).unwrap_or_else(Outcome::Failed)
        };
        report.push((checked, outcome));
    }
    Ok(report)
}

/// `out/<kind>s/<id>_<slug>/` of an item.
pub fn out_dir(session_dir: &Path, item: &Item) -> PathBuf {
    session_dir
        .join("out")
        .join(format!("{}s", item.kind.name()))
        .join(format!("{}_{}", item.id, item.slug))
}

fn render(ctx: &mut Ctx, c: &Checked, tl: &Timeline, opts: &Options) -> Result<Outcome> {
    let item = &c.item;
    let dir = out_dir(&ctx.dir, item);
    let master = std::path::absolute(ctx.master_video()?).map_err(|e| e.to_string())?;
    let m = ctx.master()?;
    let keys_mode = if item.keys_overlay {
        opts.keys
    } else {
        KeysOverlay::None
    };
    let captions_mode = if item.captions {
        opts.captions
    } else {
        Captions::None
    };
    let cfg = &ctx.config.render;
    let theme = &ctx.config.overlay;
    let hash = {
        let stamp = |p: &Path| {
            fs::metadata(p)
                .ok()
                .map(|m| (m.len(), m.modified().ok().map(|t| format!("{t:?}"))))
        };
        let inputs = serde_json::json!({
            "item": item,
            "cuts": c.cuts,
            "encoder": format!("{:?}", cfg.encoder),
            "cq": cfg.cq,
            "keys": format!("{keys_mode:?}"),
            "captions": format!("{captions_mode:?}"),
            "theme": [&theme.font_mono, &theme.font_sans, &theme.bg, &theme.accent, &theme.fg],
            "master": [master.to_string_lossy().into_owned(), format!("{:?}", stamp(&master))],
            "words": format!("{:?}", stamp(&ctx.derived("words.json"))),
            "keys_file": format!("{:?}", stamp(&ctx.dir.join("keys.jsonl.zst"))),
        });
        Sha256::digest(inputs.to_string())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let meta_path = dir.join("meta.json");
    if !opts.force
        && dir.join(VIDEO).exists()
        && read_json::<Meta>(&meta_path).is_ok_and(|m| m.input_hash == hash)
    {
        return Ok(Outcome::UpToDate);
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for name in OUTPUTS {
        let _ = fs::remove_file(dir.join(name));
    }

    let total = c.out_len();
    let mut outputs = vec![VIDEO.to_owned()];
    let mut warnings = c.warnings.clone();

    let srt = (captions_mode != Captions::None)
        .then(|| subs::srt(&subs::captions(&tl.words, &c.cuts)))
        .filter(|s| !s.is_empty());
    if let Some(srt) = &srt {
        write(&dir.join("captions.srt"), srt)?;
        outputs.push("captions.srt".into());
    }
    let ass = if keys_mode == KeysOverlay::None {
        None
    } else {
        let mut keys: Vec<(f64, ac_capture::keys::KeyRecord)> = ac_post::keys(ctx)
            .into_iter()
            .filter_map(|k| Some((ctx.rec(k.t)?, k)))
            .collect();
        keys.sort_by(|a, b| a.0.total_cmp(&b.0));
        subs::keys_ass(&keys, &c.cuts, theme, m.width, m.height)
    };
    if let Some(ass) = &ass {
        write(&dir.join("keys.ass"), ass)?;
        outputs.push("keys.ass".into());
    } else if keys_mode != KeysOverlay::None {
        warnings.push("no keystrokes in these ranges, so no keys overlay".into());
    }

    ctx.begin_task(
        &format!("{} {}", item.kind.name(), item.id),
        dir.join("render.log"),
    );
    let mut commands = Vec::new();
    let result = encode(
        ctx,
        c,
        &dir,
        &master,
        &m,
        total,
        keys_mode,
        captions_mode,
        srt.is_some(),
        ass.is_some(),
        &mut commands,
    );
    ctx.end();
    result?;

    if keys_mode == KeysOverlay::Alpha && ass.is_some() {
        let args: Vec<String> = vec![
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!(
                "color=c=black@0.0:s={}x{}:r={}:d={total:.3},format=rgba",
                m.width, m.height, m.fps
            ),
            "-vf".into(),
            format!("ass=keys.ass:alpha=1{}", fontsdir()),
            "-c:v".into(),
            "prores_ks".into(),
            "-profile:v".into(),
            "4444".into(),
            "-pix_fmt".into(),
            "yuva444p10le".into(),
            "keys.mov".into(),
        ];
        ctx.begin_task(&format!("keys {}", item.id), dir.join("render.log"));
        let result = ffmpeg(ctx, &dir, &args, total, &mut commands);
        ctx.end();
        result?;
        outputs.push("keys.mov".into());
    }

    let thumb = subs::thumb_time(&tl.words, &c.cuts, total);
    let args: Vec<String> = vec![
        "-ss".into(),
        format!("{thumb:.3}"),
        "-i".into(),
        VIDEO.into(),
        "-frames:v".into(),
        "1".into(),
        "-q:v".into(),
        "2".into(),
        "thumb.jpg".into(),
    ];
    ctx.begin_task(&format!("thumb {}", item.id), dir.join("render.log"));
    let result = ffmpeg(ctx, &dir, &args, 0.0, &mut commands);
    ctx.end();
    result?;
    outputs.push("thumb.jpg".into());

    let probe = tool::probe(ctx, &dir.join(VIDEO))?;
    let duration_s = probe["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok())
        .ok_or("ffprobe gave no duration for the rendered video")?;
    if (duration_s - total).abs() > 1.5 / tl.fps.max(1.0) {
        warnings.push(format!("rendered {duration_s:.3} s, expected {total:.3} s"));
    }
    write_json(
        &meta_path,
        &Meta {
            item: item.clone(),
            ranges: c.cuts.clone(),
            outputs,
            input_hash: hash,
            keys_overlay: format!("{keys_mode:?}").to_lowercase(),
            captions: format!("{captions_mode:?}").to_lowercase(),
            commands,
            expected_duration_s: round_ms(total),
            duration_s,
            warnings,
        },
    )?;
    Ok(Outcome::Rendered { duration_s })
}

/// The main encode: every cut from the master, speed-adjusted, concatenated, with burned-in
/// overlays when asked.
#[allow(clippy::too_many_arguments)]
fn encode(
    ctx: &mut Ctx,
    c: &Checked,
    dir: &Path,
    master: &Path,
    m: &ac_post::Master,
    total: f64,
    keys: KeysOverlay,
    captions: Captions,
    has_srt: bool,
    has_ass: bool,
    commands: &mut Vec<String>,
) -> Result<()> {
    let has_audio = tool::probe(ctx, master)?["streams"]
        .as_array()
        .is_some_and(|s| s.iter().any(|s| s["codec_type"] == "audio"));
    let n = c.cuts.len();
    let master = master.to_string_lossy().into_owned();
    let mut args: Vec<String> = Vec::new();
    for cut in &c.cuts {
        args.extend([
            "-ss".into(),
            format!("{:.3}", cut.start),
            "-t".into(),
            format!("{:.3}", cut.end - cut.start),
            "-i".into(),
            master.clone(),
        ]);
    }
    if !has_audio {
        for cut in &c.cuts {
            args.extend([
                "-f".into(),
                "lavfi".into(),
                "-t".into(),
                format!("{:.3}", cut.end - cut.start),
                "-i".into(),
                "anullsrc=r=48000:cl=stereo".into(),
            ]);
        }
    }
    let mut graph = Vec::new();
    for (i, cut) in c.cuts.iter().enumerate() {
        let s = cut.speed;
        graph.push(format!(
            "[{i}:v]setpts=(PTS-STARTPTS)/{s},fps={}[v{i}]",
            m.fps
        ));
        let a = if has_audio { i } else { n + i };
        let mut chain = vec!["asetpts=PTS-STARTPTS".to_owned()];
        chain.extend(atempo(s).into_iter().map(|f| format!("atempo={f}")));
        if s > MUTE_ABOVE {
            chain.push("volume=0".into());
        }
        graph.push(format!("[{a}:a]{}[a{i}]", chain.join(",")));
    }
    let inputs: String = (0..n).map(|i| format!("[v{i}][a{i}]")).collect();
    graph.push(format!("{inputs}concat=n={n}:v=1:a=1[cv][ca]"));
    let mut burn = Vec::new();
    if keys == KeysOverlay::Burn && has_ass {
        burn.push(format!("ass=keys.ass{}", fontsdir()));
    }
    if captions == Captions::Burn && has_srt {
        burn.push(format!(
            "subtitles=captions.srt:force_style='FontName={}'{}",
            ctx.config.overlay.font_sans,
            fontsdir()
        ));
    }
    if burn.is_empty() {
        burn.push("null".into());
    }
    graph.push(format!("[cv]{}[vout]", burn.join(",")));
    args.extend([
        "-filter_complex".into(),
        graph.join(";"),
        "-map".into(),
        "[vout]".into(),
        "-map".into(),
        "[ca]".into(),
    ]);
    let cq = ctx.config.render.cq.to_string();
    match ctx.config.render.encoder {
        Encoder::H264Nvenc => args.extend(
            [
                "-c:v",
                "h264_nvenc",
                "-preset",
                "p5",
                "-rc",
                "vbr",
                "-cq",
                &cq,
                "-b:v",
                "0",
            ]
            .map(String::from),
        ),
        Encoder::Libx264 => {
            args.extend(["-c:v", "libx264", "-preset", "medium", "-crf", "20"].map(String::from))
        }
    }
    args.extend(
        [
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            VIDEO,
        ]
        .map(String::from),
    );
    ffmpeg(ctx, dir, &args, total, commands)
}

/// `:fontsdir=...` for libass filters when Windows has per-user fonts, which ffmpeg's
/// fontconfig does not search (fonts installed "for me only" live there).
fn fontsdir() -> String {
    std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("Microsoft/Windows/Fonts"))
        .filter(|d| d.is_dir())
        .map_or_else(String::new, |d| {
            let d = d.to_string_lossy().replace('\\', "/").replace(':', "\\:");
            format!(":fontsdir='{d}'")
        })
}

/// `atempo` factors for `speed`, each within the filter's 0.5 to 2.0.
fn atempo(mut speed: f64) -> Vec<f64> {
    let mut out = Vec::new();
    while speed > 2.0 {
        out.push(2.0);
        speed /= 2.0;
    }
    while speed < 0.5 {
        out.push(0.5);
        speed /= 0.5;
    }
    if (speed - 1.0).abs() > 1e-9 {
        out.push(speed);
    }
    out
}

fn ffmpeg(
    ctx: &mut Ctx,
    dir: &Path,
    args: &[String],
    total: f64,
    commands: &mut Vec<String>,
) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(args);
    commands.push(tool::command_line(&cmd));
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    tool::ffmpeg_in(ctx, Some(dir), args, total).map(drop)
}

fn write(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(w: &str, start: f64, end: f64) -> Word {
        Word {
            w: w.into(),
            start,
            end,
            p: Some(0.9),
        }
    }

    fn timeline() -> Timeline {
        Timeline {
            duration: 600.0,
            fps: 30.0,
            words: vec![word("hello", 10.1, 10.5), word("there", 10.6, 19.8)],
            redacted: vec![(100.0, 160.0)],
            dead: vec![(300.0, 400.0)],
        }
    }

    fn item(id: &str, kind: Kind, ranges: &[(f64, f64, f64)]) -> Item {
        Item {
            id: id.into(),
            kind,
            title: "t".into(),
            slug: format!("item-{}", id.to_lowercase()),
            hook: None,
            description: None,
            tags: vec![],
            ranges: ranges
                .iter()
                .map(|&(start, end, speed)| Range {
                    start,
                    end,
                    speed,
                    session: None,
                    label: None,
                })
                .collect(),
            evidence: vec![],
            keys_overlay: true,
            captions: true,
            confidence: None,
            notes: None,
        }
    }

    fn run_check(items: Vec<Item>) -> Vec<Checked> {
        let s = Suggestions {
            schema_version: 1,
            session: "s".into(),
            generated_by: None,
            generated_at: None,
            items,
        };
        check(&s, "s", &timeline(), &config::Render::default())
    }

    #[test]
    fn snaps_to_words_then_pads_but_not_into_redaction() {
        let c = &run_check(vec![item(
            "1",
            Kind::Clip,
            &[(10.0, 20.0, 1.0), (160.1, 170.0, 1.0)],
        )])[0];
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        // 10.0 snaps to 10.1 then pads 250 ms; 20.0 snaps to 19.8 then pads 400 ms.
        assert_eq!((c.cuts[0].start, c.cuts[0].end), (9.85, 20.2));
        // Padding stops at the end of the redacted segment.
        assert_eq!(c.cuts[1].start, 160.0);
        assert_eq!(c.cuts[1].suggested, [160.1, 170.0]);

        // Back-to-back ranges meet at the boundary, not padded over it.
        let c = &run_check(vec![item(
            "2",
            Kind::Topic,
            &[(200.0, 220.0, 1.0), (220.0, 230.0, 4.0)],
        )])[0];
        assert_eq!((c.cuts[0].end, c.cuts[1].start), (220.0, 220.0));
        assert_eq!((c.cuts[0].start, c.cuts[1].end), (199.75, 230.4));
    }

    #[test]
    fn rejects_redaction_bad_ranges_long_shorts_and_duplicates() {
        let checked = run_check(vec![
            item("1", Kind::Clip, &[(90.0, 110.0, 1.0)]),
            item("2", Kind::Clip, &[(50.0, 40.0, 1.0), (590.0, 700.0, 1.0)]),
            item("3", Kind::Short, &[(200.0, 400.0, 1.0)]),
            item("4", Kind::Short, &[(200.0, 270.0, 1.0)]),
            item("4", Kind::Clip, &[(300.0, 400.0, 1.0)]),
        ]);
        assert!(checked[0].errors[0].contains("redacted"));
        assert_eq!(checked[1].errors.len(), 2);
        assert!(checked[2].errors[0].contains("over 180"));
        assert!(checked[3].errors.is_empty() && checked[3].warnings[0].contains("short_max_s"));
        assert!(checked[4].errors[0].contains("duplicate id"));
        assert!(checked[4].warnings[0].contains("dead air"));
    }

    #[test]
    fn atempo_chains_stay_in_range() {
        assert_eq!(atempo(1.0), Vec::<f64>::new());
        assert_eq!(atempo(4.0), vec![2.0, 2.0]);
        assert_eq!(atempo(3.0), vec![2.0, 1.5]);
        assert_eq!(atempo(0.5), vec![0.5]);
    }
}
