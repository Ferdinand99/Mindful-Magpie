//! Magpie's own history of every number a service reports, so that a card can
//! draw a curve without the remote app keeping any history of its own.
//!
//! **What is kept.** After each good reading (the status is known and the
//! fetch gave no error) every finite number in it is recorded: text values and
//! `NaN` or infinite numbers are skipped. Values are rounded to 4 significant
//! digits when they are recorded, so a curve never carries more precision than
//! it can show.
//!
//! * The last 24 hours, at most [`MAX_POINTS`] (288) points per metric. A
//!   service polled more often than every 5 minutes is thinned: it has one point
//!   per 5-minute slot, its time is the start of the slot (a multiple of 300 s)
//!   and the latest reading of the slot wins (the highest, for a metric the
//!   kind marks as a peak, [`MetricDef::peak`]: a count of things that are
//!   down keeps a short outage). A service polled every 5 minutes or
//!   slower has a point per reading, at the time of the reading.
//! * A metric the kind declares as a moment (`Format::Since`) is not recorded,
//!   and a service keeps at most [`MAX_METRICS`] (64) distinct series: a new
//!   name over that is dropped until an old series has aged out.
//! * One row per metric and UTC day for [`DAILY_DAYS`] (30) days: the lowest,
//!   highest and mean reading of the day. Every reading counts, also those the
//!   24-hour series thins away.
//!
//! **Where it lives.** The series are held in memory ([`History`], owned by the
//! poller) and written to `service_history` and `service_history_daily` as a
//! reading arrives, in one transaction per reading ([`persist`]). A restart
//! loads the last 24 hours back. Pages read the memory, never the table; only
//! the daily rows are read from the table, and only for the metrics a document
//! sends in full.
//!
//! **Bounds.** Every write deletes the service's rows older than the window
//! (all its metrics, also those the service no longer reports) and, per metric,
//! anything beyond the newest 288 points; the daily rows older than 30 days go
//! the same way. A metric that no card draws keeps being recorded and pruned
//! like the rest, so nothing grows past these bounds. The
//! rows go with their service (`ON DELETE CASCADE`).
//!
//! **What reaches a page.** See [`crate::values`]: `history`, `history_daily`
//! and `history_append` per service entry, for the curve metrics of the
//! services bound on the user's cards only.
//!
//! **A card asks for a curve** with a spark block naming one metric of its
//! service ([`Block::Spark`](crate::integrations::Block::Spark), written by the
//! kind into the reading's details); the renderer then draws the metric's
//! `history` as an inline SVG, a line or a strip of up and down cells. It
//! treats every value as untrusted (a `NaN` never arrives, but a huge number
//! may).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::Mutex;

use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Serialize, Serializer, ser::SerializeTuple};
use vole_kit::db::{self, DbError};
use vole_kit::state::Core;

use crate::integrations::{Format, MetricDef, Snapshot, Status};

/// How far back the series reach: 24 hours.
pub const WINDOW_S: i64 = 86_400;
/// The most points of one metric in the series.
pub const MAX_POINTS: usize = 288;
/// A service polled faster than this (seconds) is thinned to one point per slot
/// of this length.
pub const SLOT_S: i64 = 300;
/// How many days the daily rows reach, today included.
pub const DAILY_DAYS: i64 = 30;
/// The most distinct metrics kept for one service (a kind has a few; this
/// keeps a wild one from growing memory and tables). A new name over the cap is
/// dropped until an old series has aged out of the window.
pub const MAX_METRICS: usize = 64;

/// One point of a series: Unix seconds and the value. Serialised as the pair
/// `[t, v]`; a whole number goes out without a decimal point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub at: i64,
    pub value: f64,
}

struct Number(f64);

impl Serialize for Number {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.0.fract() == 0.0 && self.0.abs() < 1e15 {
            s.serialize_i64(self.0 as i64)
        } else {
            s.serialize_f64(self.0)
        }
    }
}

impl Serialize for Point {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut pair = s.serialize_tuple(2)?;
        pair.serialize_element(&self.at)?;
        pair.serialize_element(&Number(self.value))?;
        pair.end()
    }
}

/// One day of a metric: `["2026-11-07", min, max, avg]`.
#[derive(Debug, Clone, PartialEq)]
pub struct Day {
    pub day: String,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
}

impl Serialize for Day {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut row = s.serialize_tuple(4)?;
        row.serialize_element(&self.day)?;
        row.serialize_element(&Number(round4(self.min)))?;
        row.serialize_element(&Number(round4(self.max)))?;
        row.serialize_element(&Number(round4(self.avg)))?;
        row.end()
    }
}

/// `value` rounded to 4 significant digits (`123456.0` is `123500.0`,
/// `0.000123456` is `0.0001235`). A number that is not finite is returned as
/// it is.
pub fn round4(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    format!("{value:.3e}").parse().unwrap_or(value)
}

/// The numbers of a reading worth keeping: none for a reading that is not
/// good (status unknown, or an error), else every finite number, rounded, by
/// metric name (at most [`MAX_METRICS`]). A metric the kind declares in
/// `metrics` as a moment (`Format::Since`, Unix seconds) is left out: a
/// timestamp makes no curve.
pub fn numbers(snapshot: &Snapshot, metrics: &[MetricDef]) -> Vec<(String, f64)> {
    if snapshot.status == Status::Unknown || snapshot.error.is_some() {
        return Vec::new();
    }
    snapshot
        .values
        .iter()
        .filter(|(metric, _)| {
            !metrics
                .iter()
                .any(|m| m.id == metric.as_str() && m.format == Format::Since)
        })
        .filter_map(|(metric, v)| {
            let n = v.num.filter(|n| n.is_finite())?;
            Some((metric.clone(), round4(n)))
        })
        .take(MAX_METRICS)
        .collect()
}

/// The UTC day (`YYYY-MM-DD`) of a Unix time.
pub fn day_of(at: i64) -> String {
    jiff::Timestamp::from_second(at)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::UTC).date().to_string())
        .unwrap_or_else(|_| "1970-01-01".to_string())
}

/// The first day the daily rows of a reading at `at` still keep.
fn first_day(at: i64) -> String {
    day_of(at - (DAILY_DAYS - 1) * 86_400)
}

type Series = VecDeque<Point>;

/// The in-memory series of every service. Cheap to share: one lock, held for
/// a copy or an update only.
#[derive(Default)]
pub struct History {
    inner: Mutex<HashMap<i64, HashMap<String, Series>>>,
}

/// How a series stands against the point a page last had (see
/// [`History::tail`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tail {
    /// The newest point of the series.
    pub last: Point,
    /// How many points are newer than the one the page has.
    pub newer: usize,
    /// The value the series has now at the time of the point the page has
    /// (`None` when that point is gone), to tell a rewritten point from an
    /// unchanged one.
    pub known_value: Option<f64>,
}

/// What one reading adds to the tables, made by [`History::record`].
#[derive(Debug, Clone, PartialEq)]
pub struct Write {
    pub service_id: i64,
    /// The time of the reading (Unix seconds): the clock the pruning goes by.
    pub at: i64,
    /// The points to put in `service_history`, by metric.
    pub points: Vec<(String, Point)>,
    /// The metrics of `points` whose stored point keeps the higher value
    /// when the slot is already there.
    pub peaks: BTreeSet<String>,
    /// Every number of the reading, for the daily rows.
    pub readings: Vec<(String, f64)>,
}

impl History {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<i64, HashMap<String, Series>>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records the `readings` taken at `at` from service `id`, whose interval
    /// is `interval_s`, and says what to write to the tables. Prunes the
    /// service's series (all its metrics) to the window and the point cap.
    /// The latest reading of a slot wins.
    pub fn record(&self, id: i64, interval_s: i64, at: i64, readings: Vec<(String, f64)>) -> Write {
        self.record_with_peaks(id, interval_s, at, readings, &BTreeSet::new())
    }

    /// [`History::record`], where a metric in `peaks` keeps the highest
    /// reading of a slot instead of the latest.
    pub fn record_with_peaks(
        &self,
        id: i64,
        interval_s: i64,
        at: i64,
        readings: Vec<(String, f64)>,
        peaks: &BTreeSet<String>,
    ) -> Write {
        let slot_at = if interval_s < SLOT_S {
            at - at.rem_euclid(SLOT_S)
        } else {
            at
        };
        let cutoff = at - WINDOW_S;
        let mut all = self.lock();
        let service = all.entry(id).or_default();
        // Old series go first, so that their names make room for new ones.
        service.retain(|_, series| {
            while series.front().is_some_and(|p| p.at < cutoff) {
                series.pop_front();
            }
            !series.is_empty()
        });
        let mut points = Vec::new();
        let mut kept = Vec::new();
        for (metric, value) in readings {
            if !service.contains_key(&metric) && service.len() >= MAX_METRICS {
                continue;
            }
            let series = service.entry(metric.clone()).or_default();
            kept.push((metric.clone(), value));
            let peak = peaks.contains(&metric);
            let mut point = Point { at: slot_at, value };
            match series.back_mut() {
                // The slot's latest reading wins, or its highest for a peak.
                Some(last) if last.at == slot_at => {
                    if peak {
                        point.value = last.value.max(value);
                    }
                    last.value = point.value;
                }
                // A reading from before the newest point (the clock went
                // back): nothing to add.
                Some(last) if last.at > slot_at => continue,
                _ => series.push_back(point),
            }
            while series.len() > MAX_POINTS {
                series.pop_front();
            }
            points.push((metric, point));
        }
        // A series that only holds the new point and no old one stays; one
        // the window emptied was removed above.
        let peaks = points
            .iter()
            .filter(|(metric, _)| peaks.contains(metric))
            .map(|(metric, _)| metric.clone())
            .collect();
        Write {
            service_id: id,
            at,
            points,
            peaks,
            readings: kept,
        }
    }

    /// The series of `metrics` of service `id` within the 24 hours before
    /// `now`, oldest first. A metric with no point in the window is left out.
    pub fn series(
        &self,
        id: i64,
        metrics: &BTreeSet<String>,
        now: i64,
    ) -> BTreeMap<String, Vec<Point>> {
        let cutoff = now - WINDOW_S;
        let all = self.lock();
        let Some(service) = all.get(&id) else {
            return BTreeMap::new();
        };
        metrics
            .iter()
            .filter_map(|metric| {
                let points: Vec<Point> = service
                    .get(metric)?
                    .iter()
                    .filter(|p| p.at >= cutoff)
                    .copied()
                    .collect();
                (!points.is_empty()).then(|| (metric.clone(), points))
            })
            .collect()
    }

    /// What changed in the series of `metrics` of service `id` since the page
    /// last had the point `known` of each (metrics it has no point of are left
    /// out: ask for their [`History::series`]). Looks only at the points after
    /// `known`, so it holds the lock for a few steps, not a copy of the series.
    pub fn tail(
        &self,
        id: i64,
        known: &BTreeMap<String, Point>,
        metrics: &BTreeSet<String>,
        now: i64,
    ) -> BTreeMap<String, Tail> {
        let cutoff = now - WINDOW_S;
        let all = self.lock();
        let Some(service) = all.get(&id) else {
            return BTreeMap::new();
        };
        let mut out = BTreeMap::new();
        for metric in metrics {
            let (Some(sent), Some(series)) = (known.get(metric), service.get(metric)) else {
                continue;
            };
            let Some(last) = series.back().copied().filter(|p| p.at >= cutoff) else {
                continue;
            };
            let newer = series.iter().rev().take_while(|p| p.at > sent.at).count();
            let known_value = series
                .iter()
                .rev()
                .nth(newer)
                .filter(|p| p.at == sent.at)
                .map(|p| p.value);
            out.insert(
                metric.clone(),
                Tail {
                    last,
                    newer,
                    known_value,
                },
            );
        }
        out
    }

    /// Forgets a service (it was deleted).
    pub fn forget(&self, id: i64) {
        self.lock().remove(&id);
    }

    /// Puts stored rows (service, metric, time, value) in, as after a
    /// restart. Rows may come in any order; the cap and the order are kept.
    pub fn load(&self, mut rows: Vec<(i64, String, i64, f64)>) {
        rows.sort_by_key(|(_, _, at, _)| *at);
        let mut all = self.lock();
        for (id, metric, at, value) in rows {
            if !value.is_finite() {
                continue;
            }
            let service = all.entry(id).or_default();
            if !service.contains_key(&metric) && service.len() >= MAX_METRICS {
                continue;
            }
            let series = service.entry(metric).or_default();
            series.push_back(Point { at, value });
            while series.len() > MAX_POINTS {
                series.pop_front();
            }
        }
    }
}

/// The stored points of the last 24 hours before `now`, for [`History::load`].
pub fn load_rows(conn: &Connection, now: i64) -> rusqlite::Result<Vec<(i64, String, i64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT service_id, metric, at, value FROM service_history WHERE at >= ?1 ORDER BY at",
    )?;
    stmt.query_map([now - WINDOW_S], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?
    .collect()
}

/// Writes what a reading adds, and prunes, in one transaction. A service that
/// is gone gets nothing, whatever a late reading brings.
pub fn write(conn: &mut Connection, w: &Write) -> rusqlite::Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = w.service_id;
    let cutoff = w.at - WINDOW_S;
    {
        // A slot that is there already takes the new value, or the higher
        // one for a peak metric (?5).
        let mut put = tx.prepare_cached(
            "INSERT INTO service_history (service_id, metric, at, value)
             SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (SELECT 1 FROM services WHERE id = ?1)
             ON CONFLICT (service_id, metric, at) DO UPDATE SET
               value = CASE WHEN ?5 THEN MAX(value, excluded.value) ELSE excluded.value END",
        )?;
        let mut cap = tx.prepare_cached(
            "DELETE FROM service_history WHERE service_id = ?1 AND metric = ?2 AND at <
               (SELECT at FROM service_history WHERE service_id = ?1 AND metric = ?2
                ORDER BY at DESC LIMIT 1 OFFSET ?3)",
        )?;
        for (metric, point) in &w.points {
            put.execute(params![
                id,
                metric,
                point.at,
                point.value,
                w.peaks.contains(metric)
            ])?;
            cap.execute(params![id, metric, MAX_POINTS as i64 - 1])?;
        }
        let day = day_of(w.at);
        let mut roll = tx.prepare_cached(
            "INSERT INTO service_history_daily (service_id, metric, day, min, max, avg, n)
             SELECT ?1, ?2, ?3, ?4, ?4, ?4, 1 WHERE EXISTS (SELECT 1 FROM services WHERE id = ?1)
             ON CONFLICT (service_id, metric, day) DO UPDATE SET
               min = MIN(min, excluded.min),
               max = MAX(max, excluded.max),
               avg = (avg * n + excluded.avg) / (n + 1),
               n = n + 1",
        )?;
        for (metric, value) in &w.readings {
            roll.execute(params![id, metric, day, value])?;
        }
    }
    tx.execute(
        "DELETE FROM service_history WHERE service_id = ?1 AND at < ?2",
        params![id, cutoff],
    )?;
    tx.execute(
        "DELETE FROM service_history_daily WHERE service_id = ?1 AND day < ?2",
        params![id, first_day(w.at)],
    )?;
    tx.commit()
}

/// [`write`] on the database's own thread.
pub async fn persist(core: &Core, w: Write) -> Result<(), DbError> {
    db::interact(&core.db, move |c| write(c, &w)).await
}

/// The daily rows of the `wanted` metrics (by service id) for the 30 days
/// before `now`, oldest day first.
pub async fn daily(
    core: &Core,
    wanted: BTreeMap<i64, BTreeSet<String>>,
    now: i64,
) -> Result<BTreeMap<i64, BTreeMap<String, Vec<Day>>>, DbError> {
    if wanted.is_empty() {
        return Ok(BTreeMap::new());
    }
    let first = first_day(now);
    db::interact(&core.db, move |c| {
        let mut stmt = c.prepare(
            "SELECT metric, day, min, max, avg FROM service_history_daily
             WHERE service_id = ?1 AND day >= ?2 ORDER BY metric, day",
        )?;
        let mut out: BTreeMap<i64, BTreeMap<String, Vec<Day>>> = BTreeMap::new();
        for (id, metrics) in &wanted {
            let rows = stmt.query_map(params![id, first], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    Day {
                        day: r.get(1)?,
                        min: r.get(2)?,
                        max: r.get(3)?,
                        avg: r.get(4)?,
                    },
                ))
            })?;
            for row in rows {
                let (metric, day) = row?;
                if metrics.contains(&metric) {
                    out.entry(*id)
                        .or_default()
                        .entry(metric)
                        .or_default()
                        .push(day);
                }
            }
        }
        Ok(out)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(metric: &str, v: f64) -> Vec<(String, f64)> {
        vec![(metric.to_string(), v)]
    }

    #[test]
    fn a_peak_keeps_the_highest_reading_of_its_slot_and_the_others_the_latest() {
        let h = History::default();
        let base = 1_760_000_100 - 1_760_000_100 % SLOT_S;
        let peaks: BTreeSet<String> = ["down".to_string()].into();
        let both =
            |down: f64, other: f64| vec![("down".to_string(), down), ("other".to_string(), other)];
        let w = h.record_with_peaks(1, 60, base + 10, both(1.0, 1.0), &peaks);
        assert_eq!(w.peaks, peaks);
        let w = h.record_with_peaks(1, 60, base + 70, both(0.0, 0.0), &peaks);
        // What is written is the slot's value: the peak for `down`.
        assert_eq!(
            w.points,
            vec![
                (
                    "down".to_string(),
                    Point {
                        at: base,
                        value: 1.0
                    }
                ),
                (
                    "other".to_string(),
                    Point {
                        at: base,
                        value: 0.0
                    }
                ),
            ]
        );
        let w = h.record_with_peaks(1, 60, base + 130, both(3.0, 3.0), &peaks);
        assert_eq!(w.points[0].1.value, 3.0);
        // A service polled every 5 minutes or slower has a point per reading.
        let w = h.record_with_peaks(2, 300, base, both(1.0, 1.0), &peaks);
        let w2 = h.record_with_peaks(2, 300, base + 300, both(0.0, 0.0), &peaks);
        assert_eq!((w.points[0].1.value, w2.points[0].1.value), (1.0, 0.0));
        // `record` keeps no peak.
        h.record(3, 60, base + 10, r("down", 1.0));
        let w = h.record(3, 60, base + 70, r("down", 0.0));
        assert!(w.peaks.is_empty());
        assert_eq!(w.points[0].1.value, 0.0);
    }

    #[test]
    fn four_significant_digits() {
        assert_eq!(round4(123_456.0), 123_500.0);
        assert_eq!(round4(0.000_123_456), 0.000_123_5);
        assert_eq!(round4(0.0), 0.0);
        assert_eq!(round4(-98.765), -98.77);
        assert_eq!(round4(1e300), 1e300);
        assert!(round4(f64::NAN).is_nan());
        // A denormal does not turn into NaN.
        assert!(round4(5e-324).is_finite());
    }

    #[test]
    fn a_whole_number_has_no_decimal_point() {
        let p = Point {
            at: 1_760_000_100,
            value: 200.0,
        };
        assert_eq!(serde_json::to_string(&p).unwrap(), "[1760000100,200]");
        let p = Point {
            at: 1_760_000_100,
            value: 12.5,
        };
        assert_eq!(serde_json::to_string(&p).unwrap(), "[1760000100,12.5]");
    }

    #[test]
    fn a_fast_service_has_one_point_per_slot_and_the_latest_wins() {
        let h = History::default();
        let base = 1_760_000_100 - 1_760_000_100 % SLOT_S;
        h.record(1, 15, base + 10, r("m", 1.0));
        h.record(1, 15, base + 25, r("m", 2.0));
        h.record(1, 15, base + 299, r("m", 3.0));
        h.record(1, 15, base + 300, r("m", 4.0));
        let s = h.series(1, &["m".to_string()].into(), base + 300);
        assert_eq!(
            s["m"],
            vec![
                Point {
                    at: base,
                    value: 3.0
                },
                Point {
                    at: base + 300,
                    value: 4.0
                }
            ]
        );
    }

    #[test]
    fn a_slow_service_has_a_point_per_reading() {
        let h = History::default();
        let base = 1_760_000_000;
        h.record(1, 300, base, r("m", 1.0));
        h.record(1, 300, base + 270, r("m", 2.0));
        let s = h.series(1, &["m".to_string()].into(), base + 270);
        assert_eq!(s["m"].len(), 2);
        assert_eq!(s["m"][1].at, base + 270);
    }

    #[test]
    fn the_window_and_the_cap_hold() {
        let h = History::default();
        let base = 1_760_000_000;
        for i in 0..1000 {
            h.record(1, 300, base + i * 300, r("m", i as f64));
        }
        let now = base + 999 * 300;
        let s = h.series(1, &["m".to_string()].into(), now);
        assert_eq!(s["m"].len(), MAX_POINTS);
        assert_eq!(s["m"].last().unwrap().value, 999.0);
        // Nothing older than 24 hours, even when the reading is far later.
        assert!(
            h.series(1, &["m".to_string()].into(), now + WINDOW_S + 1)
                .is_empty()
        );
    }

    #[test]
    fn a_metric_the_service_stops_reporting_ages_out() {
        let h = History::default();
        let base = 1_760_000_000;
        h.record(1, 60, base, r("old", 1.0));
        h.record(1, 60, base + WINDOW_S + 600, r("new", 1.0));
        let all: BTreeSet<String> = ["old".to_string(), "new".to_string()].into();
        let s = h.series(1, &all, base + WINDOW_S + 600);
        assert!(!s.contains_key("old"));
        assert!(s.contains_key("new"));
    }

    #[test]
    fn only_good_finite_numbers_are_taken() {
        use crate::integrations::Value;
        let mut s = Snapshot::new(Status::Ok);
        s.values.insert("a".into(), Value::num(1.23456));
        s.values.insert("nan".into(), Value::num(f64::NAN));
        s.values.insert("inf".into(), Value::num(f64::INFINITY));
        s.values.insert("text".into(), Value::text("up"));
        assert_eq!(numbers(&s, &[]), vec![("a".to_string(), 1.235)]);
        s.status = Status::Unknown;
        assert!(numbers(&s, &[]).is_empty());
        s.status = Status::Bad;
        s.error = Some("timeout".into());
        assert!(numbers(&s, &[]).is_empty());
    }

    #[test]
    fn a_moment_is_not_a_curve() {
        use crate::integrations::Value;
        let mut s = Snapshot::new(Status::Ok);
        s.values.insert("age".into(), Value::num(1_762_900_000.0));
        s.values.insert("load".into(), Value::num(1.5));
        let defs = [
            MetricDef {
                id: "age",
                label_key: "x",
                format: Format::Since,
                peak: false,
            },
            MetricDef {
                id: "load",
                label_key: "x",
                format: Format::Number,
                peak: false,
            },
        ];
        assert_eq!(numbers(&s, &defs), vec![("load".to_string(), 1.5)]);
    }

    #[test]
    fn a_service_keeps_at_most_64_series_and_new_names_over_it_are_dropped() {
        let h = History::default();
        let base = 1_760_000_000;
        let many = |from: usize, to: usize| -> Vec<(String, f64)> {
            (from..to).map(|k| (format!("m{k:03}"), 1.0)).collect()
        };
        let w = h.record(1, 60, base, many(0, 70));
        assert_eq!(w.points.len(), MAX_METRICS);
        assert_eq!(
            w.readings.len(),
            MAX_METRICS,
            "no daily row for a dropped name"
        );
        let all: BTreeSet<String> = (0..70).map(|k| format!("m{k:03}")).collect();
        assert_eq!(h.series(1, &all, base).len(), MAX_METRICS);
        // Still full: a name that is not there is dropped, one that is, kept.
        let w = h.record(1, 60, base + 60, many(60, 70));
        assert_eq!(w.points.len(), 4);
        // A day later the old series have aged out and make room.
        let w = h.record(1, 60, base + WINDOW_S + 600, many(64, 70));
        assert_eq!(w.points.len(), 6);
    }

    #[test]
    fn the_tail_tells_a_new_point_from_a_rewritten_one_without_a_copy() {
        let h = History::default();
        let base = 1_760_000_100 - 1_760_000_100 % SLOT_S;
        let m = |name: &str| -> BTreeSet<String> { [name.to_string()].into() };
        h.record(1, 60, base + 5, r("m", 1.0));
        let sent = Point {
            at: base,
            value: 1.0,
        };
        let known: BTreeMap<String, Point> = [("m".to_string(), sent)].into();
        let now = base + 10;

        let t = h.tail(1, &known, &m("m"), now)["m"];
        assert_eq!((t.last, t.newer, t.known_value), (sent, 0, Some(1.0)));

        // The same slot rewritten: no newer point, the point changed.
        h.record(1, 60, base + 20, r("m", 2.0));
        let t = h.tail(1, &known, &m("m"), now)["m"];
        assert_eq!(
            t.last,
            Point {
                at: base,
                value: 2.0
            }
        );
        assert_eq!((t.newer, t.known_value), (0, Some(2.0)));

        // A rewrite and then a new slot: one newer point, and the point the
        // page has is not what it had.
        h.record(1, 60, base + 305, r("m", 3.0));
        let t = h.tail(1, &known, &m("m"), now)["m"];
        assert_eq!(t.newer, 1);
        assert_ne!(t.known_value, Some(1.0));

        // Metrics the page has no point of are left out.
        assert!(h.tail(1, &BTreeMap::new(), &m("m"), now).is_empty());
        assert!(h.tail(2, &known, &m("m"), now).is_empty());
    }

    #[test]
    fn days_are_utc() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(86_399), "1970-01-01");
        assert_eq!(day_of(86_400), "1970-01-02");
    }
}
