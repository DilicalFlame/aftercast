//! Going back in the recording from the rehearse page's player. OBS records fragmented mp4
//! (a file that is playable while it grows: `ftyp`, `moov`, then a `moof` + `mdat` per few
//! seconds). This reads that file's fragments into a time index and serves bytes of it with
//! range requests; the page feeds them to a media source, from whichever fragment it needs.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::Shared;

/// The newest fragmented mp4 (`.mp4`, `.mov`) in the media folder.
fn latest(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| ["mp4", "mov", "m4v"].contains(&x.to_ascii_lowercase().as_str()))
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(t, _)| *t)
        .map(|(_, p)| p)
}

/// What the page needs to play the file from any point.
#[derive(Debug, Default, PartialEq)]
pub struct Index {
    /// Bytes of `ftyp` + `moov`: the initialization segment.
    pub init_end: u64,
    /// Everything up to here is whole boxes.
    pub end: u64,
    /// For the media source, e.g. `avc1.640034,mp4a.40.2`.
    pub codecs: String,
    /// Video fragments: start time in milliseconds, byte offset of the `moof`.
    pub fragments: Vec<(u64, u64)>,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes inside `range` of `b`: type and the range of the body.
fn children(b: &[u8], range: Range<usize>) -> Vec<([u8; 4], Range<usize>)> {
    let (mut at, mut out) = (range.start, Vec::new());
    while at + 8 <= range.end {
        let Some(size) = u32_at(b, at) else { break };
        let size = size as usize;
        if size < 8 || at + size > range.end {
            break;
        }
        let kind: [u8; 4] = b[at + 4..at + 8].try_into().unwrap_or_default();
        out.push((kind, at + 8..at + size));
        at += size;
    }
    out
}

fn child(b: &[u8], range: Range<usize>, kind: &[u8; 4]) -> Option<Range<usize>> {
    children(b, range)
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, r)| r)
}

/// The video track in a `moov`: its id, its timescale and the codec string.
fn video_track(moov: &[u8]) -> Option<(u32, u32, String)> {
    for (kind, trak) in children(moov, 8..moov.len()) {
        if &kind != b"trak" {
            continue;
        }
        let mdia = child(moov, trak.clone(), b"mdia")?;
        let hdlr = child(moov, mdia.clone(), b"hdlr")?;
        if moov.get(hdlr.start + 8..hdlr.start + 12)? != b"vide" {
            continue;
        }
        let tkhd = child(moov, trak.clone(), b"tkhd")?;
        let id = u32_at(
            moov,
            tkhd.start + if moov[tkhd.start] == 1 { 20 } else { 12 },
        )?;
        let mdhd = child(moov, mdia, b"mdhd")?;
        let scale = u32_at(
            moov,
            mdhd.start + if moov[mdhd.start] == 1 { 20 } else { 12 },
        )?;
        let body = &moov[trak];
        let at = body.windows(4).position(|w| w == b"avcC")?;
        let p = body.get(at + 5..at + 8)?;
        return Some((
            id,
            scale,
            format!("avc1.{:02x}{:02x}{:02x}", p[0], p[1], p[2]),
        ));
    }
    None
}

/// The video track's start time (in its timescale) in a `moof`, if it has one.
fn fragment_time(moof: &[u8], track: u32) -> Option<u64> {
    for (kind, traf) in children(moof, 8..moof.len()) {
        if &kind != b"traf" {
            continue;
        }
        let tfhd = child(moof, traf.clone(), b"tfhd")?;
        if u32_at(moof, tfhd.start + 4)? != track {
            continue;
        }
        let tfdt = child(moof, traf, b"tfdt")?;
        return if moof[tfdt.start] == 1 {
            u64_at(moof, tfdt.start + 4)
        } else {
            u32_at(moof, tfdt.start + 4).map(u64::from)
        };
    }
    None
}

/// Reads the top-level boxes of the file (headers and each `moov`/`moof`, never the media).
pub fn scan(file: &mut File) -> std::io::Result<Index> {
    let len = file.metadata()?.len();
    let (mut at, mut index) = (0u64, Index::default());
    let mut track = None;
    while at + 8 <= len {
        let mut head = [0u8; 16];
        file.seek(SeekFrom::Start(at))?;
        let n = file.read(&mut head)?;
        let size = match u32_at(&head, 0) {
            Some(1) if n >= 16 => u64_at(&head, 8).unwrap_or(0),
            Some(0) => len - at,
            Some(s) => u64::from(s),
            None => break,
        };
        // A box still being written: stop before it.
        if size < 8 || at + size > len {
            break;
        }
        let kind: [u8; 4] = head[4..8].try_into().unwrap_or_default();
        if matches!(&kind, b"moov" | b"moof") && size <= 4 << 20 {
            let mut body = vec![0u8; size as usize];
            file.seek(SeekFrom::Start(at))?;
            file.read_exact(&mut body)?;
            if &kind == b"moov" {
                track = video_track(&body);
                if let Some((_, _, v)) = &track {
                    index.codecs = format!("{v},mp4a.40.2");
                }
                index.init_end = at + size;
            } else if let Some((id, scale, _)) = &track
                && let Some(t) = fragment_time(&body, *id)
            {
                index
                    .fragments
                    .push((t * 1000 / u64::from(*scale).max(1), at));
            }
        }
        at += size;
        index.end = at;
    }
    Ok(index)
}

fn media_dir(s: &Shared) -> Option<&Path> {
    s.rehearse.as_ref()?.media()
}

/// `/rehearse/dvr/index.json`: the recording's fragments, or why there is nothing to scrub.
pub(crate) async fn index(State(s): State<Arc<Shared>>) -> Response {
    let Some(dir) = media_dir(&s).map(Path::to_path_buf) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let found = tokio::task::spawn_blocking(move || {
        let path = latest(&dir)?;
        let index = File::open(&path).and_then(|mut f| scan(&mut f)).ok()?;
        // When OBS began the file: its creation time.
        let started = fs::metadata(&path)
            .and_then(|m| m.created().or_else(|_| m.modified()))
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_millis() as u64);
        Some((path, index, started))
    })
    .await
    .ok()
    .flatten();
    match found {
        Some((path, i, started)) if i.init_end > 0 && !i.fragments.is_empty() => Json(json!({
            "file": path.file_name().map(|n| n.to_string_lossy()),
            "started_ms": started,
            "now_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64),
            "init_end": i.init_end, "end": i.end, "codecs": i.codecs, "fragments": i.fragments,
        }))
        .into_response(),
        _ => (
            StatusCode::NOT_FOUND,
            "no fragmented mp4 recording yet (OBS: Settings, Output, Recording Format: Fragmented MP4)",
        )
            .into_response(),
    }
}

/// What a media source accepts: OBS writes each fragment's data position into `tfhd` as an
/// absolute offset (`base-data-offset`), which the browser refuses. The field comes out and
/// the flag "positions are relative to the `moof`" goes in, which is what OBS meant; an
/// 8-byte `free` box fills the gap, so no size and no other offset in the file changes.
fn patch(buf: &mut [u8]) {
    let mut at = 0;
    while at + 8 <= buf.len() {
        let size = u32_at(buf, at).unwrap_or(0) as usize;
        if size < 8 || at + size > buf.len() {
            return;
        }
        if &buf[at + 4..at + 8] == b"moof" {
            for (kind, traf) in children(&buf[..at + size], at + 8..at + size) {
                if &kind != b"traf" {
                    continue;
                }
                let Some(tfhd) = child(&buf[..at + size], traf.clone(), b"tfhd") else {
                    continue;
                };
                // The box: size, "tfhd", version and flags, track id, then base-data-offset.
                let (head, flags) = (tfhd.start - 8, tfhd.start + 1);
                if buf[flags + 2] & 1 == 0 || tfhd.len() < 16 {
                    continue;
                }
                let end = tfhd.end;
                buf[flags] |= 0x02; // default-base-is-moof (0x020000)
                buf[flags + 2] &= !1;
                buf.copy_within(tfhd.start + 16..end, tfhd.start + 8);
                let shrunk = (end - head - 8) as u32;
                buf[head..head + 4].copy_from_slice(&shrunk.to_be_bytes());
                buf[end - 8..end - 4].copy_from_slice(&8u32.to_be_bytes());
                buf[end - 4..end].copy_from_slice(b"free");
            }
        }
        at += size;
    }
}

/// How much of the file one answer carries, at least (whole boxes).
const BUDGET: u64 = 6 << 20;

/// `/rehearse/dvr/file`: the boxes of the recording from the byte in the `Range` header (a box
/// boundary: the start, or a fragment from the index), as many whole ones as fit about 6 MB,
/// made fit for a media source. `Content-Range` says what came.
pub(crate) async fn file(State(s): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    let Some(dir) = media_dir(&s).map(Path::to_path_buf) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let from = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.split_once('-'))
        .and_then(|(a, _)| a.parse::<u64>().ok())
        .unwrap_or(0);
    let read = tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, u64)> {
        let mut f = File::open(latest(&dir)?).ok()?;
        let len = f.metadata().ok()?.len();
        let (mut at, mut head) = (from, [0u8; 8]);
        while at + 8 <= len {
            f.seek(SeekFrom::Start(at)).ok()?;
            f.read_exact(&mut head).ok()?;
            let size = u64::from(u32::from_be_bytes(head[..4].try_into().ok()?));
            if size < 8 || at + size > len || (at > from && at - from + size > BUDGET) {
                break;
            }
            at += size;
        }
        if at == from {
            return None;
        }
        let mut buf = vec![0u8; (at - from) as usize];
        f.seek(SeekFrom::Start(from)).ok()?;
        f.read_exact(&mut buf).ok()?;
        patch(&mut buf);
        Some((buf, len))
    })
    .await
    .ok()
    .flatten();
    match read {
        Some((buf, len)) => (
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, "video/mp4".to_owned()),
                (
                    header::CONTENT_RANGE,
                    format!("bytes {from}-{}/{len}", from + buf.len() as u64 - 1),
                ),
            ],
            buf,
        )
            .into_response(),
        None => StatusCode::RANGE_NOT_SATISFIABLE.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A tiny fragmented mp4: one video track (id 1, timescale 1000), fragments at 0 s and 3 s.
    fn sample() -> Vec<u8> {
        let tkhd = bx(b"tkhd", &[&[0u8; 12][..], &1u32.to_be_bytes()].concat());
        let mdhd = bx(b"mdhd", &[&[0u8; 12][..], &1000u32.to_be_bytes()].concat());
        let hdlr = bx(b"hdlr", &[&[0u8; 8][..], b"vide"].concat());
        let avcc = bx(b"avcC", &[1, 0x64, 0x00, 0x34]);
        let mdia = bx(b"mdia", &[mdhd, hdlr, bx(b"minf", &avcc)].concat());
        let moov = bx(b"moov", &bx(b"trak", &[tkhd, mdia].concat()));
        let frag = |t: u32| {
            let tfhd = bx(b"tfhd", &[&[0u8; 4][..], &1u32.to_be_bytes()].concat());
            let tfdt = bx(b"tfdt", &[&[0u8; 4][..], &t.to_be_bytes()].concat());
            [
                bx(b"moof", &bx(b"traf", &[tfhd, tfdt].concat())),
                bx(b"mdat", &[7u8; 20]),
            ]
            .concat()
        };
        [bx(b"ftyp", b"isom0000"), moov, frag(0), frag(3000)].concat()
    }

    #[test]
    fn patch_swaps_the_base_data_offset_for_moof_relative_without_changing_sizes() {
        // tfhd flags 0x000001: track id 1, base-data-offset 4096.
        let tfhd = bx(
            b"tfhd",
            &[
                &[0, 0, 0, 1][..],
                &1u32.to_be_bytes(),
                &4096u64.to_be_bytes(),
            ]
            .concat(),
        );
        let tfdt = bx(b"tfdt", &[&[0u8; 4][..], &0u32.to_be_bytes()].concat());
        let moof = bx(b"moof", &bx(b"traf", &[tfhd, tfdt].concat()));
        let mut buf = moof.clone();
        patch(&mut buf);
        assert_eq!(buf.len(), moof.len());
        let tfhd = child(&buf, 8..buf.len(), b"traf")
            .and_then(|t| child(&buf, t, b"tfhd"))
            .unwrap();
        assert_eq!(
            &buf[tfhd.start..tfhd.start + 4],
            &[0, 0x02, 0, 0],
            "default-base-is-moof only"
        );
        assert_eq!(u32_at(&buf, tfhd.start + 4), Some(1), "the track id stays");
        // The gap is a `free` box and the box after it (tfdt) is still found.
        let traf = child(&buf, 8..buf.len(), b"traf").unwrap();
        let kinds: Vec<_> = children(&buf, traf).into_iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [*b"tfhd", *b"free", *b"tfdt"]);
    }

    #[test]
    fn indexes_the_fragments_of_a_growing_file() {
        let dir = std::env::temp_dir().join(format!("ac-dvr-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.mp4");
        let mut bytes = sample();
        // The last box is still being written.
        bytes.extend_from_slice(&bx(b"moof", &[0u8; 40])[..10]);
        File::create(&path).unwrap().write_all(&bytes).unwrap();
        let i = scan(&mut File::open(&path).unwrap()).unwrap();
        assert_eq!(i.codecs, "avc1.640034,mp4a.40.2");
        assert_eq!(
            i.fragments.iter().map(|f| f.0).collect::<Vec<_>>(),
            [0, 3000]
        );
        assert_eq!(
            i.end,
            bytes.len() as u64 - 10,
            "stops before the unfinished box"
        );
        assert_eq!(latest(&dir), Some(path));
        fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod real {
    /// `AC_DVR_SAMPLE=path cargo test -p ac-server real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn scans_a_real_recording() {
        let Ok(p) = std::env::var("AC_DVR_SAMPLE") else {
            return;
        };
        let i = super::scan(&mut std::fs::File::open(p).unwrap()).unwrap();
        println!(
            "{} fragments, init {}, end {}, {}",
            i.fragments.len(),
            i.init_end,
            i.end,
            i.codecs
        );
        println!("{:?}", &i.fragments[..4.min(i.fragments.len())]);
    }
}

#[cfg(test)]
mod patched_copy {
    /// `AC_DVR_IN=a.mp4 AC_DVR_OUT=b.mp4 cargo test -p ac-server patched_copy -- --ignored`
    /// writes the first 100 MB of a recording as the player would receive it.
    #[test]
    #[ignore]
    fn writes_the_patched_file() {
        let (Ok(a), Ok(b)) = (std::env::var("AC_DVR_IN"), std::env::var("AC_DVR_OUT")) else {
            return;
        };
        let mut bytes = std::fs::read(a).unwrap();
        bytes.truncate(100_000_000);
        super::patch(&mut bytes);
        std::fs::write(b, bytes).unwrap();
    }
}
