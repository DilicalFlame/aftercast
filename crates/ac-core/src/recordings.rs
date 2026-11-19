//! `media/recordings.json`: OBS output files with their wall-clock spans and pauses
//! (specs/07-obs.md "Recording anchors", schemas/recordings.schema.json).

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::error::{Error, Result};
use crate::time::{Pause, Recording, Timestamp};

/// The recordings list plus the start of a pause that has not ended yet.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recordings {
    pub files: Vec<Recording>,
    paused_at: Option<Timestamp>,
}

/// What [`Recordings::reconcile`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciled {
    Unchanged,
    /// Recording was already running; an entry was opened with a back-computed start.
    Started,
    /// Recording stopped while disconnected; the open entry was closed now.
    Stopped,
}

impl Recordings {
    /// Reads the file; a missing file is an empty list.
    pub fn load(path: &Path) -> Result<Self> {
        let files = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| Error::io(path)(e.into()))?,
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::io(path)(e)),
        };
        Ok(Self {
            files,
            paused_at: None,
        })
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(Error::io(dir))?;
        }
        let json = serde_json::to_string_pretty(&self.files).expect("recordings serialize");
        crate::write_atomic(path, json + "\n")
    }

    /// The file being written, if any.
    pub fn open(&self) -> Option<&Recording> {
        self.files.last().filter(|r| r.stop.is_none())
    }

    pub fn is_recording(&self) -> bool {
        self.open().is_some()
    }

    /// `OBS_WEBSOCKET_OUTPUT_STARTED`. A file still open from a missed stop is closed first.
    pub fn start(&mut self, path: Option<String>, at: Timestamp) {
        self.close(None, at);
        self.files.push(Recording {
            path: path.unwrap_or_default(),
            start: at,
            stop: None,
            pauses: Vec::new(),
        });
    }

    /// `RecordFileChanged`: automatic file splitting.
    pub fn file_changed(&mut self, new_path: String, at: Timestamp) {
        self.start(Some(new_path), at);
    }

    pub fn pause(&mut self, at: Timestamp) {
        if self.is_recording() && self.paused_at.is_none() {
            self.paused_at = Some(at);
        }
    }

    pub fn resume(&mut self, at: Timestamp) {
        if let (Some(from), Some(file)) = (self.paused_at.take(), self.open_mut()) {
            file.pauses.push(Pause { from, to: at });
        }
    }

    /// `OBS_WEBSOCKET_OUTPUT_STOPPED`; `path` is OBS's `outputPath` when given.
    pub fn stop(&mut self, path: Option<String>, at: Timestamp) {
        self.close(path, at);
    }

    /// Applies OBS's `GetRecordStatus` after a (re)connect. `duration_s` is `outputDuration`.
    // ponytail: an OBS restart that also restarted recording inside one reconnect window
    // is not detected (the old entry stays open); post ingest checks durations with ffprobe.
    pub fn reconcile(
        &mut self,
        active: bool,
        paused: bool,
        duration_s: f64,
        now: Timestamp,
    ) -> Reconciled {
        let changed = match (active, self.is_recording()) {
            (true, false) => {
                // Back-compute the start; paused time inside `duration_s` is unknown.
                self.start(None, now.add_secs(-duration_s));
                Reconciled::Started
            }
            (false, true) => {
                self.close(None, now);
                Reconciled::Stopped
            }
            _ => Reconciled::Unchanged,
        };
        if active && paused {
            self.pause(now);
        } else if active {
            self.resume(now);
        }
        changed
    }

    fn open_mut(&mut self) -> Option<&mut Recording> {
        self.files.last_mut().filter(|r| r.stop.is_none())
    }

    fn close(&mut self, path: Option<String>, at: Timestamp) {
        let paused_at = self.paused_at.take();
        let Some(file) = self.open_mut() else { return };
        if let Some(from) = paused_at {
            file.pauses.push(Pause { from, to: at });
        }
        file.stop = Some(at);
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            file.path = path;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: f64) -> Timestamp {
        "2026-11-19T10:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .add_secs(secs)
    }

    fn validate(files: &[Recording]) {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/recordings.schema.json"
        );
        let schema = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        let v = jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .unwrap();
        let json = serde_json::to_value(files).unwrap();
        let errors: Vec<String> = v.iter_errors(&json).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{json}: {errors:?}");
    }

    #[test]
    fn start_pause_split_stop() {
        let mut r = Recordings::default();
        r.start(Some("D:/OBS/a.mkv".into()), t(0.0));
        r.pause(t(10.0));
        r.pause(t(11.0)); // repeated pause keeps the first
        r.resume(t(20.0));
        r.file_changed("D:/OBS/b.mkv".into(), t(60.0));
        r.pause(t(70.0));
        r.stop(Some("D:/OBS/b.mkv".into()), t(80.0)); // stop while paused closes the pause
        assert!(!r.is_recording());
        assert_eq!(
            r.files,
            [
                Recording {
                    path: "D:/OBS/a.mkv".into(),
                    start: t(0.0),
                    stop: Some(t(60.0)),
                    pauses: vec![Pause {
                        from: t(10.0),
                        to: t(20.0)
                    }],
                },
                Recording {
                    path: "D:/OBS/b.mkv".into(),
                    start: t(60.0),
                    stop: Some(t(80.0)),
                    pauses: vec![Pause {
                        from: t(70.0),
                        to: t(80.0)
                    }],
                },
            ]
        );
        assert_eq!(crate::time::rec_t(&r.files, t(65.0)), Some(50.0 + 5.0));
        validate(&r.files);
    }

    #[test]
    fn stop_without_path_keeps_start_path_and_unknown_start_path_is_filled() {
        let mut r = Recordings::default();
        r.start(None, t(0.0));
        r.stop(Some("D:/OBS/x.mp4".into()), t(5.0));
        r.start(Some("D:/OBS/y.mp4".into()), t(10.0));
        r.stop(None, t(15.0));
        assert_eq!(r.files[0].path, "D:/OBS/x.mp4");
        assert_eq!(r.files[1].path, "D:/OBS/y.mp4");
        r.resume(t(16.0)); // nothing open: ignored
        assert_eq!(r.files.len(), 2);
    }

    #[test]
    fn reconcile_back_computes_start_and_closes_missed_stops() {
        let mut r = Recordings::default();
        assert_eq!(
            r.reconcile(true, false, 125.5, t(200.0)),
            Reconciled::Started
        );
        assert_eq!(r.files[0].start, t(74.5));
        assert_eq!(
            r.reconcile(true, true, 130.0, t(205.0)),
            Reconciled::Unchanged
        );
        assert_eq!(
            r.reconcile(false, false, 0.0, t(300.0)),
            Reconciled::Stopped
        );
        assert_eq!(r.files[0].stop, Some(t(300.0)));
        assert_eq!(
            r.files[0].pauses,
            [Pause {
                from: t(205.0),
                to: t(300.0)
            }]
        );
        assert_eq!(
            r.reconcile(false, false, 0.0, t(310.0)),
            Reconciled::Unchanged
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("media/recordings.json");
        assert_eq!(Recordings::load(&path).unwrap(), Recordings::default());
        let mut r = Recordings::default();
        r.start(Some("a.mkv".into()), t(0.0));
        r.save(&path).unwrap();
        let loaded = Recordings::load(&path).unwrap();
        assert_eq!(loaded.files, r.files);
        assert!(loaded.is_recording());
        validate(&loaded.files);
    }
}
