//! The `schedule:` grammar a skill page declares (#819): a cron expression,
//! optionally prefixed `CRON_TZ=<IANA zone> `.
//!
//! Five fields (minute granularity) or six (a leading seconds field). Without
//! a `CRON_TZ=` prefix the expression is read in UTC, never in the host's
//! local zone: the indexer that validates a page and the runner that fires it
//! may sit in different zones, and both must agree on when a window opens.
//!
//! One parser for both readers — the indexer's `check_schedule` and the
//! runner's schedule loop — so a page that validates is a page that fires.

use std::fmt;

use chrono::{DateTime, SubsecRound, Utc};
use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};

/// The prefix naming the zone a cron expression is read in.
pub const CRON_TZ_PREFIX: &str = "CRON_TZ=";

/// A parsed `schedule:` value.
#[derive(Debug, Clone)]
pub struct CronSchedule {
    expr: String,
    tz: Option<Tz>,
    cron: Cron,
}

/// Why a `schedule:` value does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleError {
    /// `CRON_TZ=<zone>` names no IANA zone.
    UnknownTimezone(String),
    /// The cron part is empty or malformed.
    InvalidCron(String),
}

impl fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTimezone(tz) => write!(f, "`CRON_TZ={tz}` names no IANA time zone"),
            Self::InvalidCron(msg) => write!(f, "not a 5- or 6-field cron expression: {msg}"),
        }
    }
}

impl std::error::Error for ScheduleError {}

impl CronSchedule {
    /// Parse `[CRON_TZ=<zone> ]<cron>`.
    ///
    /// # Errors
    ///
    /// [`ScheduleError`] for an unknown zone or a malformed cron part.
    pub fn parse(expr: &str) -> Result<Self, ScheduleError> {
        let trimmed = expr.trim();
        let (tz, cron_part) = match trimmed.strip_prefix(CRON_TZ_PREFIX) {
            Some(rest) => {
                let (zone, cron_part) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let tz = zone
                    .parse::<Tz>()
                    .map_err(|_| ScheduleError::UnknownTimezone(zone.to_owned()))?;
                (Some(tz), cron_part.trim())
            }
            None => (None, trimmed),
        };
        if cron_part.is_empty() {
            return Err(ScheduleError::InvalidCron("empty expression".to_owned()));
        }
        let cron = CronParser::builder()
            .seconds(Seconds::Optional)
            .year(Year::Disallowed)
            .build()
            .parse(cron_part)
            .map_err(|e| ScheduleError::InvalidCron(e.to_string()))?;
        Ok(Self {
            expr: trimmed.to_owned(),
            tz,
            cron,
        })
    }

    /// The expression as written (trimmed), `CRON_TZ=` prefix included.
    #[must_use]
    pub fn expr(&self) -> &str {
        &self.expr
    }

    /// The start of the most recent window that has opened at or before
    /// `now` — the one occurrence a scheduler should fire, never the backlog
    /// of every occurrence it slept through. `None` when the pattern has no
    /// past occurrence within croner's search limit.
    #[must_use]
    pub fn latest_window_start(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        // Croner matches whole seconds; a sub-second `now` would make an
        // inclusive match on the current second fail.
        let now = now.trunc_subsecs(0);
        match self.tz {
            Some(tz) => self
                .cron
                .find_previous_occurrence(&now.with_timezone(&tz), true)
                .ok()
                .map(|t| t.with_timezone(&Utc)),
            None => self.cron.find_previous_occurrence(&now, true).ok(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn five_field_cron_reads_in_utc_and_returns_the_latest_window_only() {
        let s = CronSchedule::parse("15 7 * * *").unwrap();
        // Three days after a window: the latest one, not a backlog.
        assert_eq!(
            s.latest_window_start(utc(2026, 11, 13, 9, 0, 0)),
            Some(utc(2026, 11, 13, 7, 15, 0))
        );
        // Before today's window opens: yesterday's.
        assert_eq!(
            s.latest_window_start(utc(2026, 11, 13, 7, 14, 59)),
            Some(utc(2026, 11, 12, 7, 15, 0))
        );
        // Exactly at the window (sub-second jitter included) is inside it.
        let at = utc(2026, 11, 13, 7, 15, 0) + chrono::Duration::milliseconds(400);
        assert_eq!(s.latest_window_start(at), Some(utc(2026, 11, 13, 7, 15, 0)));
    }

    #[test]
    fn six_field_cron_has_second_granularity() {
        let s = CronSchedule::parse("*/2 * * * * *").unwrap();
        assert_eq!(
            s.latest_window_start(utc(2026, 11, 13, 9, 0, 5)),
            Some(utc(2026, 11, 13, 9, 0, 4))
        );
    }

    #[test]
    fn cron_tz_prefix_shifts_the_window_into_that_zone() {
        // 07:15 in Berlin is 06:15 UTC in November (CET, UTC+1).
        let s = CronSchedule::parse("CRON_TZ=Europe/Berlin 15 7 * * 1-5").unwrap();
        assert_eq!(s.expr(), "CRON_TZ=Europe/Berlin 15 7 * * 1-5");
        // Friday 2026-11-13 09:00 UTC.
        assert_eq!(
            s.latest_window_start(utc(2026, 11, 13, 9, 0, 0)),
            Some(utc(2026, 11, 13, 6, 15, 0))
        );
        // Sunday: Friday's window is still the latest (weekdays only).
        assert_eq!(
            s.latest_window_start(utc(2026, 11, 15, 12, 0, 0)),
            Some(utc(2026, 11, 13, 6, 15, 0))
        );
    }

    #[test]
    fn malformed_values_are_refused_with_the_reason() {
        assert!(matches!(
            CronSchedule::parse("CRON_TZ=Mars/Olympus 0 * * * *"),
            Err(ScheduleError::UnknownTimezone(z)) if z == "Mars/Olympus"
        ));
        for bad in [
            "",
            "   ",
            "CRON_TZ=UTC",
            "every monday",
            "61 * * * *",
            "* * *",
            "0 0 0 1 1 * 2027",
        ] {
            assert!(
                matches!(CronSchedule::parse(bad), Err(ScheduleError::InvalidCron(_))),
                "{bad:?} must not parse"
            );
        }
    }
}
