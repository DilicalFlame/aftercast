//! Daily quota ledger (`state/quota.json`) and the guard every API call goes through.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ac_core::time::Timestamp;
use chrono_tz::America::Los_Angeles;
use serde::{Deserialize, Serialize};

use crate::Error;

/// Default YouTube Data API project quota per day.
pub const DAILY_QUOTA: u32 = 10_000;
/// The quota day resets at midnight Pacific Time.
const QUOTA_TZ: &str = "America/Los_Angeles";

/// YouTube API methods aftercast calls, with their default unit costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    VideosList,
    ChannelsList,
    LiveBroadcastsList,
    VideosUpdate,
    ThumbnailsSet,
    ChatStreamList,
    ChatList,
    ChatInsert,
    ChatDelete,
    BansInsert,
}

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Self::VideosList => "videos.list",
            Self::ChannelsList => "channels.list",
            Self::LiveBroadcastsList => "liveBroadcasts.list",
            Self::VideosUpdate => "videos.update",
            Self::ThumbnailsSet => "thumbnails.set",
            Self::ChatStreamList => "liveChatMessages.streamList",
            Self::ChatList => "liveChatMessages.list",
            Self::ChatInsert => "liveChatMessages.insert",
            Self::ChatDelete => "liveChatMessages.delete",
            Self::BansInsert => "liveChatBans.insert",
        }
    }

    fn default_cost(self) -> u32 {
        match self {
            Self::VideosList | Self::ChannelsList | Self::LiveBroadcastsList => 1,
            Self::ChatStreamList | Self::ChatList => 5,
            Self::ChatInsert => 20,
            Self::VideosUpdate | Self::ThumbnailsSet | Self::ChatDelete => 50,
            Self::BansInsert => 200,
        }
    }

    /// Reads keep working after `quotaExceeded`; writes stop for the day.
    fn is_read(self) -> bool {
        matches!(
            self,
            Self::VideosList
                | Self::ChannelsList
                | Self::LiveBroadcastsList
                | Self::ChatStreamList
                | Self::ChatList
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub t: Timestamp,
    pub method: String,
    pub units: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Ledger {
    day: String,
    tz: String,
    spent: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    exhausted: bool,
    entries: Vec<Entry>,
}

impl Ledger {
    fn new(day: String) -> Self {
        Self {
            day,
            tz: QUOTA_TZ.to_owned(),
            spent: 0,
            exhausted: false,
            entries: Vec::new(),
        }
    }
}

/// One recorded spend, reported to the caller (it emits `quota.spent`).
#[derive(Debug, Clone, PartialEq)]
pub struct Spent {
    pub method: &'static str,
    pub units: u32,
    pub remaining: i64,
}

/// Enforces the reserve and records every call in the ledger file.
#[derive(Debug)]
pub struct QuotaGuard {
    path: PathBuf,
    reserve: u32,
    costs: BTreeMap<String, u32>,
    session: Option<String>,
    ledger: Mutex<Ledger>,
}

impl QuotaGuard {
    pub fn open(path: &Path, reserve: u32, costs: BTreeMap<String, u32>) -> Result<Self, Error> {
        let ledger = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| Error::Ledger(format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == ErrorKind::NotFound => Ledger::new(String::new()),
            Err(e) => return Err(Error::Ledger(format!("{}: {e}", path.display()))),
        };
        Ok(Self {
            path: path.to_path_buf(),
            reserve,
            costs,
            session: None,
            ledger: Mutex::new(ledger),
        })
    }

    /// Session name recorded with each entry.
    pub fn set_session(&mut self, name: Option<String>) {
        self.session = name;
    }

    pub fn cost(&self, method: Method) -> u32 {
        self.costs
            .get(method.name())
            .copied()
            .unwrap_or(method.default_cost())
    }

    /// Units left today (may be negative after overspending).
    pub fn remaining(&self, now: Timestamp) -> i64 {
        let mut ledger = self.ledger.lock().expect("ledger lock");
        roll(&mut ledger, now);
        i64::from(DAILY_QUOTA) - i64::from(ledger.spent)
    }

    pub fn exhausted(&self, now: Timestamp) -> bool {
        let mut ledger = self.ledger.lock().expect("ledger lock");
        roll(&mut ledger, now);
        ledger.exhausted
    }

    /// Checks that `method` may run now. Critical calls may dip into the reserve.
    pub fn check(&self, method: Method, critical: bool, now: Timestamp) -> Result<(), Error> {
        let cost = i64::from(self.cost(method));
        let mut ledger = self.ledger.lock().expect("ledger lock");
        roll(&mut ledger, now);
        if ledger.exhausted && !method.is_read() {
            return Err(Error::QuotaExhausted);
        }
        let remaining = i64::from(DAILY_QUOTA) - i64::from(ledger.spent);
        if !critical && remaining - cost < i64::from(self.reserve) {
            return Err(Error::QuotaReserve {
                method: method.name(),
                remaining,
                reserve: self.reserve,
            });
        }
        Ok(())
    }

    /// Records a finished call (failed calls cost quota too) and saves the ledger.
    pub fn record(&self, method: Method, ok: bool, now: Timestamp) -> Spent {
        let units = self.cost(method);
        let mut ledger = self.ledger.lock().expect("ledger lock");
        roll(&mut ledger, now);
        ledger.spent += units;
        ledger.entries.push(Entry {
            t: now,
            method: method.name().to_owned(),
            units,
            session: self.session.clone(),
            ok,
        });
        self.save(&ledger);
        Spent {
            method: method.name(),
            units,
            remaining: i64::from(DAILY_QUOTA) - i64::from(ledger.spent),
        }
    }

    /// The API answered `quotaExceeded`: stop writes until the quota day rolls over.
    pub fn mark_exhausted(&self, now: Timestamp) {
        let mut ledger = self.ledger.lock().expect("ledger lock");
        roll(&mut ledger, now);
        ledger.exhausted = true;
        self.save(&ledger);
    }

    fn save(&self, ledger: &Ledger) {
        let text = serde_json::to_string_pretty(ledger).expect("ledger serializes");
        if let Err(e) = ac_core::write_atomic(&self.path, text) {
            tracing::error!("saving quota ledger: {e}");
        }
    }
}

/// Starts a fresh ledger when the Pacific quota day changed.
fn roll(ledger: &mut Ledger, now: Timestamp) {
    let day = quota_day(now);
    if ledger.day != day {
        *ledger = Ledger::new(day);
    }
}

/// The quota day (`YYYY-MM-DD` in Pacific Time) containing `t`.
pub fn quota_day(t: Timestamp) -> String {
    t.datetime()
        .with_timezone(&Los_Angeles)
        .format("%Y-%m-%d")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn quota_day_is_pacific() {
        // 07:59 UTC is still the previous day in Los Angeles (UTC-8 in November).
        assert_eq!(quota_day(ts("2026-11-19T07:59:00Z")), "2026-11-18");
        assert_eq!(quota_day(ts("2026-11-19T08:00:00Z")), "2026-11-19");
        // Summer time: UTC-7.
        assert_eq!(quota_day(ts("2026-07-01T06:59:00Z")), "2026-06-30");
    }

    #[test]
    fn reserve_blocks_non_critical_calls_and_day_rolls_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quota.json");
        let costs = BTreeMap::from([("videos.update".to_owned(), 4_000)]);
        let mut guard = QuotaGuard::open(&path, 1_500, costs).unwrap();
        guard.set_session(Some("2026-11-19_ep042".into()));
        let now = ts("2026-11-19T12:00:00Z");

        guard.check(Method::VideosUpdate, false, now).unwrap();
        let spent = guard.record(Method::VideosUpdate, true, now);
        assert_eq!((spent.units, spent.remaining), (4_000, 6_000));
        guard.record(Method::VideosUpdate, false, now); // failures cost too
        assert_eq!(guard.remaining(now), 2_000);

        // 2,000 - 4,000 < 1,500 reserve: only critical calls may proceed.
        assert!(matches!(
            guard.check(Method::VideosUpdate, false, now),
            Err(Error::QuotaReserve { .. })
        ));
        guard.check(Method::VideosUpdate, true, now).unwrap();
        guard.check(Method::VideosList, false, now).unwrap();

        // The ledger persists, then resets at Pacific midnight.
        let reopened = QuotaGuard::open(&path, 1_500, BTreeMap::new()).unwrap();
        assert_eq!(reopened.remaining(now), 2_000);
        assert_eq!(
            reopened.remaining(ts("2026-11-20T08:00:00Z")),
            i64::from(DAILY_QUOTA)
        );
    }

    #[test]
    fn exhausted_stops_writes_but_not_reads() {
        let dir = tempfile::tempdir().unwrap();
        let guard = QuotaGuard::open(&dir.path().join("quota.json"), 0, BTreeMap::new()).unwrap();
        let now = ts("2026-11-19T12:00:00Z");
        guard.mark_exhausted(now);
        assert!(matches!(
            guard.check(Method::VideosUpdate, true, now),
            Err(Error::QuotaExhausted)
        ));
        guard.check(Method::VideosList, false, now).unwrap();
        assert!(!guard.exhausted(ts("2026-11-20T09:00:00Z")));
    }
}
