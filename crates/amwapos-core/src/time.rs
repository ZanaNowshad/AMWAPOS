//! Time helpers. All persisted timestamps are UTC RFC3339 with millisecond
//! precision so that lexical order equals chronological order. Business dates
//! are derived in the store's configured IANA timezone (default Asia/Bahrain).

use chrono::{DateTime, NaiveDate, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;

use crate::error::{AppError, AppResult};

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

pub fn fmt(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now_str() -> String {
    fmt(now())
}

pub fn parse(s: &str) -> AppResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| AppError::validation(format!("'{s}' is not a valid timestamp.")))
}

pub fn tz(name: &str) -> AppResult<Tz> {
    name.parse::<Tz>().map_err(|_| AppError::validation(format!("Unknown timezone '{name}'.")))
}

/// The store's trading day: its timezone and the cutoff after local midnight
/// at which one business date ends and the next begins (0 = midnight).
/// A sale at 01:30 with a 03:00 cutoff belongs to the previous business date.
/// This is the only place the rule lives; every stored `business_date` and
/// every report date range goes through it.
#[derive(Debug, Clone)]
pub struct Day {
    pub tz: String,
    pub cutoff_minutes: i64,
}

impl Day {
    pub fn midnight(tz: &str) -> Self {
        Self { tz: tz.to_string(), cutoff_minutes: 0 }
    }
}

/// The store's trading day as configured (timezone from the business record,
/// cutoff from Settings → Shifts & cash).
pub fn day(c: &rusqlite::Connection) -> AppResult<Day> {
    use rusqlite::OptionalExtension;
    let tz: String =
        c.query_row("SELECT timezone FROM business LIMIT 1", [], |r| r.get(0)).optional()?.unwrap_or_else(|| "Asia/Bahrain".to_string());
    let cutoff: Option<i64> = c
        .query_row("SELECT json_extract(value_json, '$.day_cutoff_minutes') FROM settings WHERE key='shift'", [], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(Day { tz, cutoff_minutes: cutoff.unwrap_or(0).clamp(0, MAX_CUTOFF_MINUTES) })
}

/// The latest allowed cutoff: 06:00.
pub const MAX_CUTOFF_MINUTES: i64 = 360;

/// Business date (YYYY-MM-DD) for an instant on the store's trading day.
pub fn business_date(t: DateTime<Utc>, day: &Day) -> AppResult<String> {
    let z = tz(&day.tz)?;
    let local = t.with_timezone(&z) - chrono::Duration::minutes(day.cutoff_minutes);
    Ok(local.date_naive().format("%Y-%m-%d").to_string())
}

/// UTC bounds [start, end) for a business date range [from, to] inclusive:
/// each business date runs from its cutoff to the next day's cutoff.
pub fn local_date_range_utc(from: &str, to: &str, day: &Day) -> AppResult<(String, String)> {
    let z = tz(&day.tz)?;
    let f = NaiveDate::parse_from_str(from, "%Y-%m-%d")
        .map_err(|_| AppError::validation(format!("'{from}' is not a valid date (YYYY-MM-DD).")))?;
    let t =
        NaiveDate::parse_from_str(to, "%Y-%m-%d").map_err(|_| AppError::validation(format!("'{to}' is not a valid date (YYYY-MM-DD).")))?;
    if t < f {
        return Err(AppError::validation("The end date is before the start date."));
    }
    let cut = chrono::Duration::minutes(day.cutoff_minutes);
    let start = z
        .from_local_datetime(&(f.and_hms_opt(0, 0, 0).unwrap() + cut))
        .earliest()
        .ok_or_else(|| AppError::validation("Invalid start date for timezone."))?;
    let end_day = t.succ_opt().ok_or_else(|| AppError::validation("Date out of range."))?;
    let end = z
        .from_local_datetime(&(end_day.and_hms_opt(0, 0, 0).unwrap() + cut))
        .earliest()
        .ok_or_else(|| AppError::validation("Invalid end date for timezone."))?;
    Ok((fmt(start.with_timezone(&Utc)), fmt(end.with_timezone(&Utc))))
}

/// "24 Sep 2026 19:42" in the given timezone (falls back to the raw value).
pub fn display(ts: &str, zone: &str) -> String {
    match (parse(ts), tz(zone)) {
        (Ok(t), Ok(z)) => t.with_timezone(&z).format("%d %b %Y %H:%M").to_string(),
        _ => ts.to_string(),
    }
}

pub fn validate_date(s: &str) -> AppResult<()> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| AppError::validation(format!("'{s}' is not a valid date (YYYY-MM-DD).")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bahrain_business_date() {
        // 22:30 UTC on the 23rd is 01:30 on the 24th in Bahrain (UTC+3).
        let t = parse("2026-09-23T22:30:00.000Z").unwrap();
        let bh = Day::midnight("Asia/Bahrain");
        assert_eq!(business_date(t, &bh).unwrap(), "2026-09-24");
        let (a, b) = local_date_range_utc("2026-09-24", "2026-09-24", &bh).unwrap();
        assert_eq!(a, "2026-09-23T21:00:00.000Z");
        assert_eq!(b, "2026-09-24T21:00:00.000Z");
        assert!(local_date_range_utc("2026-09-25", "2026-09-24", &bh).is_err());
    }

    #[test]
    fn cutoff_moves_early_hours_to_the_previous_trading_day() {
        let late = Day { tz: "Asia/Bahrain".into(), cutoff_minutes: 180 };
        // 01:30 local on the 5th, before the 03:00 cutoff → trading day of the 4th.
        let t = parse("2026-10-04T22:30:00.000Z").unwrap();
        assert_eq!(business_date(t, &late).unwrap(), "2026-10-04");
        // 03:00 local exactly starts the 5th.
        let t = parse("2026-10-05T00:00:00.000Z").unwrap();
        assert_eq!(business_date(t, &late).unwrap(), "2026-10-05");
        // The 4th runs 03:00 on the 4th → 03:00 on the 5th (local), and the
        // range and the date agree on every instant.
        let (a, b) = local_date_range_utc("2026-10-04", "2026-10-04", &late).unwrap();
        assert_eq!(a, "2026-10-04T00:00:00.000Z");
        assert_eq!(b, "2026-10-05T00:00:00.000Z");
        for m in [0i64, 59, 60 * 12, 60 * 24 - 1] {
            let t = parse(&a).unwrap() + chrono::Duration::minutes(m);
            assert_eq!(business_date(t, &late).unwrap(), "2026-10-04");
        }
    }
}
