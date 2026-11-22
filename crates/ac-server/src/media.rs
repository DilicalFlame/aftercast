//! What the BRB screen shows: music playlists (folders of audio files, with their titles and
//! album art read from the files' tags) and the UPI QR code with the logo in its middle.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use lofty::file::TaggedFileExt;
use lofty::tag::Accessor;
use qrcode::{Color, EcLevel, QrCode};
use serde_json::json;

use crate::Shared;
use ac_core::config::{self, Config};

const AUDIO: [&str; 6] = ["mp3", "ogg", "opus", "wav", "flac", "m4a"];
const COVER_NAMES: [&str; 3] = ["cover", "folder", "front"];

/// The BRB's playlist is the folder `<dir>/sounds/brb/`.
const PLAYLIST: &str = "brb";

/// How many tracks are in the BRB's playlist folder.
pub fn tracks(dir: &Path) -> usize {
    fs::read_dir(dir.join("sounds").join(PLAYLIST))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| AUDIO.contains(&ext(n).as_str()))
        })
        .count()
}

fn ext(file: &str) -> String {
    file.rsplit_once('.')
        .map_or(String::new(), |(_, e)| e.to_ascii_lowercase())
}

fn safe_name(file: &str) -> bool {
    !file.is_empty()
        && file.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b' ' | b'(' | b')')
        })
        && !file.contains("..")
}

/// A track's title, artist and album from its tags (the file's name when it has none).
fn tags(path: &Path) -> (String, String, String) {
    let stem = path
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    let tagged = lofty::probe::Probe::open(path)
        .ok()
        .and_then(|p| p.read().ok());
    let Some(tag) = tagged
        .as_ref()
        .and_then(|f| f.primary_tag().or_else(|| f.first_tag()))
    else {
        return (stem, String::new(), String::new());
    };
    let text = |v: Option<std::borrow::Cow<'_, str>>| v.map_or(String::new(), |t| t.into_owned());
    let title = text(tag.title());
    (
        if title.is_empty() { stem } else { title },
        text(tag.artist()),
        text(tag.album()),
    )
}

/// `/overlay/music/{playlist}/index.json`: the tracks of a playlist, in the order of their
/// file names.
pub(crate) async fn music_index(
    State(s): State<Arc<Shared>>,
    UrlPath((playlist, file)): UrlPath<(String, String)>,
) -> Response {
    if !config::skin_name_ok(&playlist) || file != "index.json" {
        return music_file(State(s), UrlPath((playlist, file))).await;
    }
    let folder = s.dir.join("sounds").join(&playlist);
    let mut files: Vec<String> = fs::read_dir(&folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| AUDIO.contains(&ext(n).as_str()))
        .collect();
    files.sort();
    let tracks: Vec<_> = files
        .into_iter()
        .map(|f| {
            let (title, artist, album) = tags(&folder.join(&f));
            json!({ "file": f, "title": title, "artist": artist, "album": album })
        })
        .collect();
    Json(tracks).into_response()
}

/// `/overlay/music/{playlist}/{file}`: a track.
pub(crate) async fn music_file(
    State(s): State<Arc<Shared>>,
    UrlPath((playlist, file)): UrlPath<(String, String)>,
) -> Response {
    let mime = match ext(&file).as_str() {
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if !config::skin_name_ok(&playlist) || !safe_name(&file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match fs::read(s.dir.join("sounds").join(&playlist).join(&file)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `/overlay/cover/{playlist}/{file}`: a track's album art: the picture in its tags, else a
/// `<track>.jpg`/`.png` beside it, else the folder's `cover.jpg`/`.png`.
pub(crate) async fn music_cover(
    State(s): State<Arc<Shared>>,
    UrlPath((playlist, file)): UrlPath<(String, String)>,
) -> Response {
    if !config::skin_name_ok(&playlist) || !safe_name(&file) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let folder = s.dir.join("sounds").join(&playlist);
    let path = folder.join(&file);
    // Embedded in the file.
    let embedded = lofty::probe::Probe::open(&path)
        .ok()
        .and_then(|p| p.read().ok())
        .and_then(|f| {
            let tag = f.primary_tag().or_else(|| f.first_tag())?;
            let pic = tag.pictures().first()?;
            let mime = pic
                .mime_type()
                .map_or("image/jpeg", lofty::picture::MimeType::as_str);
            Some((mime.to_owned(), pic.data().to_vec()))
        });
    if let Some((mime, bytes)) = embedded {
        return ([(header::CONTENT_TYPE, mime)], bytes).into_response();
    }
    // A picture beside it.
    let stem = Path::new(&file)
        .file_stem()
        .map_or(String::new(), |s| s.to_string_lossy().into_owned());
    for name in std::iter::once(stem.as_str()).chain(COVER_NAMES) {
        for (e, mime) in [
            ("jpg", "image/jpeg"),
            ("jpeg", "image/jpeg"),
            ("png", "image/png"),
            ("webp", "image/webp"),
        ] {
            if let Ok(bytes) = fs::read(folder.join(format!("{name}.{e}"))) {
                return ([(header::CONTENT_TYPE, mime)], bytes).into_response();
            }
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The QR code of `data` as an SVG: round dots, rounded finder eyes in `eye`, on a rounded
/// `bg` card, with `logo` (mime and bytes) in a clear round window in the middle (the code's
/// highest error correction lets it cover that much).
pub fn qr_svg(
    data: &str,
    fg: &str,
    eye: &str,
    bg: &str,
    logo: Option<(&str, &[u8])>,
) -> Option<String> {
    let code = QrCode::with_error_correction_level(data, EcLevel::H).ok()?;
    let n = code.width();
    let (q, size) = (3.0_f64, n as f64 + 6.0);
    let mid = size / 2.0;
    let clear = if logo.is_some() { n as f64 * 0.17 } else { 0.0 };
    let in_eye = |x: usize, y: usize| {
        let near = |a: usize, b: usize| a < 8 && b < 8;
        near(x, y) || near(n - 1 - x, y) || near(x, n - 1 - y)
    };
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {size} {size}" width="640" height="640"><rect width="{size}" height="{size}" rx="3.2" fill="{bg}"/>"#
    );
    for y in 0..n {
        for x in 0..n {
            if code[(x, y)] != Color::Dark || in_eye(x, y) {
                continue;
            }
            let (cx, cy) = (x as f64 + 0.5 + q, y as f64 + 0.5 + q);
            if (cx - mid).hypot(cy - mid) < clear {
                continue;
            }
            svg.push_str(&format!(
                r#"<circle cx="{cx}" cy="{cy}" r="0.43" fill="{fg}"/>"#
            ));
        }
    }
    // The three finder eyes: a rounded ring and a rounded dot.
    for (ex, ey) in [(0.0, 0.0), (n as f64 - 7.0, 0.0), (0.0, n as f64 - 7.0)] {
        let (x, y) = (ex + q, ey + q);
        svg.push_str(&format!(
            r#"<rect x="{}" y="{}" width="6" height="6" rx="1.9" fill="none" stroke="{eye}" stroke-width="1"/><rect x="{}" y="{}" width="3" height="3" rx="1" fill="{eye}"/>"#,
            x + 0.5,
            y + 0.5,
            x + 2.0,
            y + 2.0
        ));
    }
    if let Some((mime, bytes)) = logo {
        let r = n as f64 * 0.125;
        svg.push_str(&format!(
            r##"<clipPath id="l"><circle cx="{mid}" cy="{mid}" r="{r}"/></clipPath><circle cx="{mid}" cy="{mid}" r="{}" fill="{bg}" stroke="{eye}" stroke-width="0.45"/><image href="data:{mime};base64,{}" x="{}" y="{}" width="{}" height="{}" preserveAspectRatio="xMidYMid slice" clip-path="url(#l)"/>"##,
            r + 0.7,
            base64(bytes),
            mid - r,
            mid - r,
            r * 2.0,
            r * 2.0
        ));
    }
    svg.push_str("</svg>");
    Some(svg)
}

/// `/overlay/brb.json`: the BRB screen's options, and the address of the picture.
pub(crate) async fn brb_json(State(s): State<Arc<Shared>>, uri: axum::http::Uri) -> Response {
    match Config::load(&s.config) {
        Ok(c) => {
            let skin = uri
                .query()
                .and_then(|q| q.split('&').find_map(|p| p.strip_prefix("skin=")))
                .unwrap_or("");
            let screen = c.overlay.brb_screen.effective(skin);
            Json(json!({ "screen": screen, "avatar": c.overlay.avatar })).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `/overlay/upi.svg`: the QR code for `[overlay.brb_screen] qr_link`, else the UPI id in
/// `[vars.support] upi` (404 without either), with the overlay picture in its middle and the
/// overlay's accent color on its eyes.
pub(crate) async fn upi_svg(State(s): State<Arc<Shared>>) -> Response {
    let Ok(c) = Config::load(&s.config) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let link = c.overlay.brb_screen.qr_link.trim();
    let data = if link.is_empty() {
        let Some(upi) = c
            .vars
            .get("support")
            .and_then(|v| v.get("upi"))
            .and_then(|v| v.as_str())
            .filter(|u| !u.is_empty())
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        format!(
            "upi://pay?pa={upi}&pn={}&cu=INR",
            percent(&c.channel.series)
        )
    } else {
        link.to_owned()
    };
    let avatar = s.dir.join(&c.overlay.avatar);
    let logo = fs::read(&avatar).ok().map(|bytes| {
        let mime = match ext(&c.overlay.avatar).as_str() {
            "png" => "image/png",
            "webp" => "image/webp",
            "svg" => "image/svg+xml",
            _ => "image/jpeg",
        };
        (mime, bytes)
    });
    let accent = if config::rgb(&c.overlay.accent).is_some() {
        c.overlay.accent.clone()
    } else {
        "#c0359b".into()
    };
    match qr_svg(
        &data,
        "#1d1230",
        &accent,
        "#ffffff",
        logo.as_ref().map(|(m, b)| (*m, b.as_slice())),
    ) {
        Some(svg) => ([(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_pads_like_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_qr_has_eyes_dots_and_a_logo_window() {
        let svg = qr_svg(
            "upi://pay?pa=a@b&cu=INR",
            "#111",
            "#c0359b",
            "#fff",
            Some(("image/png", b"x")),
        )
        .unwrap();
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert_eq!(
            svg.matches("stroke-width=\"1\"").count(),
            3,
            "three finder eyes"
        );
        assert!(svg.contains("<circle cx=") && svg.contains("data:image/png;base64,eA=="));
        // Without a logo there is no window and no image.
        let plain = qr_svg("upi://pay?pa=a@b&cu=INR", "#111", "#c0359b", "#fff", None).unwrap();
        assert!(!plain.contains("<image"));
        assert!(plain.matches("<circle").count() > svg.matches("<circle").count());
    }

    /// Writes the QR code of the home in `AFTERCAST_HOME` to `AFTERCAST_QR_OUT`, to look at:
    /// `cargo test -p ac-server dump_the_qr -- --ignored`.
    #[test]
    #[ignore]
    fn dump_the_qr() {
        let home = std::path::PathBuf::from(std::env::var("AFTERCAST_HOME").unwrap());
        let c = Config::load(&home.join("aftercast.toml")).unwrap();
        let upi = c.vars["support"]["upi"].as_str().unwrap();
        let data = format!(
            "upi://pay?pa={upi}&pn={}&cu=INR",
            percent(&c.channel.series)
        );
        let logo = fs::read(home.join("overlays").join(&c.overlay.avatar)).unwrap();
        let svg = qr_svg(
            &data,
            "#1d1230",
            &c.overlay.accent,
            "#ffffff",
            Some(("image/jpeg", &logo)),
        )
        .unwrap();
        fs::write(std::env::var("AFTERCAST_QR_OUT").unwrap(), svg).unwrap();
    }

    #[test]
    fn names_are_percent_encoded() {
        assert_eq!(percent("Insomnia Driven Dev"), "Insomnia%20Driven%20Dev");
    }
}
