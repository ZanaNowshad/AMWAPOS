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

/// Business date (YYYY-MM-DD) for an instant in the given timezone.
pub fn business_date(t: DateTime<Utc>, zone: &str) -> AppResult<String> {
    let z = tz(zone)?;
    Ok(t.with_timezone(&z).date_naive().format("%Y-%m-%d").to_string())
}

/// UTC bounds [start, end) for a local calendar date range [from, to] inclusive.
pub fn local_date_range_utc(from: &str, to: &str, zone: &str) -> AppResult<(String, String)> {
    let z = tz(zone)?;
    let f = NaiveDate::parse_from_str(from, "%Y-%m-%d")
        .map_err(|_| AppError::validation(format!("'{from}' is not a valid date (YYYY-MM-DD).")))?;
    let t =
        NaiveDate::parse_from_str(to, "%Y-%m-%d").map_err(|_| AppError::validation(format!("'{to}' is not a valid date (YYYY-MM-DD).")))?;
    if t < f {
        return Err(AppError::validation("The end date is before the start date."));
    }
    let start = z
        .from_local_datetime(&f.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .ok_or_else(|| AppError::validation("Invalid start date for timezone."))?;
    let end_day = t.succ_opt().ok_or_else(|| AppError::validation("Date out of range."))?;
    let end = z
        .from_local_datetime(&end_day.and_hms_opt(0, 0, 0).unwrap())
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
        assert_eq!(business_date(t, "Asia/Bahrain").unwrap(), "2026-09-24");
        let (a, b) = local_date_range_utc("2026-09-24", "2026-09-24", "Asia/Bahrain").unwrap();
        assert_eq!(a, "2026-09-23T21:00:00.000Z");
        assert_eq!(b, "2026-09-24T21:00:00.000Z");
        assert!(local_date_range_utc("2026-09-25", "2026-09-24", "Asia/Bahrain").is_err());
    }
}
