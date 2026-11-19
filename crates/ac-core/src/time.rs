//! Clocks, timestamps and timelines (specs/05-time-sync.md).
//!
//! Every event time comes from one [`Clock`]: the wall time read once at start plus the
//! monotonic time elapsed since, so NTP jumps during a stream never move timestamps.

use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A UTC wall time with millisecond precision, written as `2026-11-19T09:31:12.481Z`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    pub fn from_datetime(dt: DateTime<Utc>) -> Self {
        let ms = dt.timestamp_millis();
        Self(DateTime::from_timestamp_millis(ms).expect("in range"))
    }

    pub fn from_unix_ms(ms: i64) -> Self {
        Self(DateTime::from_timestamp_millis(ms).expect("in range"))
    }

    /// The system clock now; live sessions use a [`Clock`] instead.
    pub fn now() -> Self {
        Self::from_datetime(Utc::now())
    }

    pub fn unix_ms(self) -> i64 {
        self.0.timestamp_millis()
    }

    pub fn datetime(self) -> DateTime<Utc> {
        self.0
    }

    /// Seconds from `earlier` to `self` (negative when `self` is earlier).
    pub fn secs_since(self, earlier: Timestamp) -> f64 {
        (self.unix_ms() - earlier.unix_ms()) as f64 / 1000.0
    }

    /// `self` shifted by a signed number of seconds (rounded to ms).
    pub fn add_secs(self, secs: f64) -> Self {
        Self::from_unix_ms(self.unix_ms() + (secs * 1000.0).round() as i64)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::Millis, true))
    }
}

impl FromStr for Timestamp {
    type Err = chrono::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DateTime::parse_from_rfc3339(s).map(|dt| Self::from_datetime(dt.to_utc()))
    }
}

impl Serialize for Timestamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// The process clock: `wall0 + (now - mono0)`, plus a virtual offset used by scripted runs.
#[derive(Debug)]
pub struct Clock {
    wall0: Timestamp,
    mono0: Instant,
    /// Virtual time added by [`Clock::advance`] (scripted/mock sessions only).
    offset_ms: AtomicU64,
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock {
    pub fn new() -> Self {
        Self::starting_at(Timestamp::from_datetime(Utc::now()))
    }

    pub fn starting_at(wall0: Timestamp) -> Self {
        Self {
            wall0,
            mono0: Instant::now(),
            offset_ms: AtomicU64::new(0),
        }
    }

    /// Current wall time and ms since the clock started.
    pub fn now(&self) -> (Timestamp, u64) {
        let mono_ms =
            self.mono0.elapsed().as_millis() as u64 + self.offset_ms.load(Ordering::Relaxed);
        (
            Timestamp::from_unix_ms(self.wall0.unix_ms() + mono_ms as i64),
            mono_ms,
        )
    }

    pub fn now_ts(&self) -> Timestamp {
        self.now().0
    }

    /// Moves the clock forward without waiting (headless `--script` runs).
    pub fn advance(&self, by: Duration) {
        self.offset_ms
            .fetch_add(by.as_millis() as u64, Ordering::Relaxed);
    }
}

/// One OBS output file with its wall-clock span and pauses (`media/recordings.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub path: String,
    pub start: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pauses: Vec<Pause>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pause {
    pub from: Timestamp,
    pub to: Timestamp,
}

impl Recording {
    /// Seconds of recorded media in this file up to `t` (pauses excluded).
    fn recorded_until(&self, t: Timestamp) -> f64 {
        let paused: f64 = self
            .pauses
            .iter()
            .filter(|p| p.from < t)
            .map(|p| p.to.min(t).secs_since(p.from))
            .sum();
        t.secs_since(self.start) - paused
    }
}

/// Offset of wall time `t` in the concatenated master recording (`rec_t`).
///
/// `t` inside a pause maps to the pause point; `t` outside every file is `None`.
/// `recordings` must be sorted by start.
pub fn rec_t(recordings: &[Recording], t: Timestamp) -> Option<f64> {
    let mut before = 0.0;
    for r in recordings {
        if t < r.start {
            return None;
        }
        match r.stop {
            Some(stop) if t > stop => before += r.recorded_until(stop),
            _ => return Some(before + r.recorded_until(t)),
        }
    }
    None
}

/// Offset of wall time `t` in the YouTube VOD (`vod_t`).
pub fn vod_t(t: Timestamp, yt_actual_start: Timestamp, vod_offset_ms: i64) -> f64 {
    t.secs_since(yt_actual_start) + vod_offset_ms as f64 / 1000.0
}

/// Chapter timestamp: `M:SS`, or `H:MM:SS` when `long` (the VOD is an hour or more).
pub fn chapter_ts(secs: f64, long: bool) -> String {
    let total = secs.max(0.0) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if long {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{}:{s:02}", total / 60)
    }
}

/// `HH:MM:SS` for logs and `index.md`.
pub fn hms(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total / 60 % 60,
        total % 60
    )
}

/// Result of [`find_lag`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lag {
    /// Seconds into `reference` where `probe` starts.
    pub lag_s: f64,
    /// Correlation peak divided by the best peak more than 0.5 s away (confidence).
    pub peak_ratio: f64,
}

/// Finds where `probe` occurs inside `reference` by FFT cross-correlation.
/// Both are mono samples at `rate` Hz; only non-negative lags are searched.
pub fn find_lag(reference: &[f32], probe: &[f32], rate: u32) -> Lag {
    let n = (reference.len() + probe.len()).next_power_of_two();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let spectrum = |samples: &[f32]| {
        let mut buf: Vec<Complex<f32>> = samples.iter().map(|&x| Complex::new(x, 0.0)).collect();
        buf.resize(n, Complex::default());
        fft.process(&mut buf);
        buf
    };
    let a = spectrum(reference);
    let b = spectrum(probe);
    let mut corr: Vec<Complex<f32>> = a.iter().zip(&b).map(|(x, y)| x * y.conj()).collect();
    planner.plan_fft_inverse(n).process(&mut corr);

    let lags = reference.len().saturating_sub(probe.len()) + 1;
    let values: Vec<f32> = corr[..lags].iter().map(|c| c.re).collect();
    let (best, &peak) = values
        .iter()
        .enumerate()
        .max_by(|x, y| x.1.total_cmp(y.1))
        .unwrap_or((0, &0.0));
    let guard = rate as usize / 2;
    let second = values
        .iter()
        .enumerate()
        .filter(|(i, _)| i.abs_diff(best) > guard)
        .map(|(_, &v)| v)
        .fold(f32::MIN_POSITIVE, f32::max);
    Lag {
        lag_s: best as f64 / f64::from(rate),
        peak_ratio: f64::from(peak / second),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn timestamp_roundtrips_with_ms() {
        let t = ts("2026-11-19T09:31:12.481Z");
        assert_eq!(t.to_string(), "2026-11-19T09:31:12.481Z");
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            "\"2026-11-19T09:31:12.481Z\""
        );
        assert_eq!(ts("2026-11-19T10:31:12.481+01:00"), t);
        assert_eq!(
            ts("2026-11-19T09:31:12Z").to_string(),
            "2026-11-19T09:31:12.000Z"
        );
        assert_eq!(t.add_secs(-1.5).to_string(), "2026-11-19T09:31:10.981Z");
    }

    fn two_files_one_pause() -> Vec<Recording> {
        vec![
            Recording {
                path: "a.mkv".into(),
                start: ts("2026-11-19T10:00:00Z"),
                stop: Some(ts("2026-11-19T10:10:00Z")),
                pauses: vec![Pause {
                    from: ts("2026-11-19T10:02:00Z"),
                    to: ts("2026-11-19T10:03:00Z"),
                }],
            },
            Recording {
                path: "b.mkv".into(),
                start: ts("2026-11-19T10:10:00.500Z"),
                stop: None,
                pauses: vec![],
            },
        ]
    }

    #[test]
    fn rec_t_maps_across_files_and_pauses() {
        let r = two_files_one_pause();
        assert_eq!(rec_t(&r, ts("2026-11-19T10:00:30Z")), Some(30.0));
        assert_eq!(rec_t(&r, ts("2026-11-19T10:05:00Z")), Some(240.0));
        // First file holds 9 min of media; second file starts after it.
        assert_eq!(rec_t(&r, ts("2026-11-19T10:10:10.500Z")), Some(550.0));
    }

    #[test]
    fn rec_t_in_pause_maps_to_pause_point() {
        let r = two_files_one_pause();
        assert_eq!(rec_t(&r, ts("2026-11-19T10:02:40Z")), Some(120.0));
    }

    #[test]
    fn rec_t_outside_files_is_none() {
        let r = two_files_one_pause();
        assert_eq!(rec_t(&r, ts("2026-11-19T09:59:59Z")), None);
        assert_eq!(rec_t(&r, ts("2026-11-19T10:10:00.200Z")), None);
        assert_eq!(rec_t(&[], ts("2026-11-19T10:00:00Z")), None);
    }

    #[test]
    fn vod_t_applies_offset() {
        let start = ts("2026-11-19T09:28:55Z");
        assert_eq!(vod_t(ts("2026-11-19T09:30:55Z"), start, 0), 120.0);
        assert_eq!(vod_t(ts("2026-11-19T09:30:55Z"), start, -2310), 117.69);
    }

    #[test]
    fn chapter_ts_both_regimes() {
        assert_eq!(chapter_ts(0.0, false), "0:00");
        assert_eq!(chapter_ts(45.9, false), "0:45");
        assert_eq!(chapter_ts(59.0 * 60.0 + 7.0, false), "59:07");
        assert_eq!(chapter_ts(0.0, true), "0:00:00");
        assert_eq!(chapter_ts(3600.0 + 3.0 * 60.0 + 15.0, true), "1:03:15");
        assert_eq!(hms(3930.25), "01:05:30");
    }

    #[test]
    fn clock_is_monotonic_across_a_million_reads() {
        // The wall clock is read once, so a system clock (NTP) jump after start cannot
        // move event times backwards; only the monotonic clock advances them.
        let clock = Clock::starting_at(ts("2026-11-19T09:00:00Z"));
        let mut last = clock.now();
        for i in 0..1_000_000 {
            if i == 500_000 {
                clock.advance(Duration::from_secs(5));
            }
            let now = clock.now();
            assert!(now >= last, "{now:?} < {last:?}");
            last = now;
        }
        assert!(last.1 >= 5000);
    }

    #[test]
    fn find_lag_recovers_synthetic_offset() {
        let rate = 8000;
        let mut seed = 0x2545_f491_u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32 - 0.5
        };
        let reference: Vec<f32> = (0..rate * 20).map(|_| noise()).collect();
        let start = (1.234 * f64::from(rate)) as usize;
        let probe: Vec<f32> = reference[start..start + rate as usize * 5]
            .iter()
            .map(|x| x * 0.7 + noise() * 0.2)
            .collect();
        let lag = find_lag(&reference, &probe, rate);
        assert!((lag.lag_s - 1.234).abs() <= 0.010, "{lag:?}");
        assert!(lag.peak_ratio > 3.0, "{lag:?}");
    }
}
