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

/// The `source` of a trigger the runner fires from a skill's `schedule:`.
pub const SCHEDULE_SOURCE: &str = "runner-schedule";
/// The `source` of a trigger a signed `POST /hooks/schedule/<skill>` fired (#827).
pub const HOOK_SOURCE: &str = "schedule-webhook";
/// The longest delivery id a webhook trigger's id may carry.
pub const MAX_DELIVERY_ID_LEN: usize = 128;

/// Whether `id` may name a webhook delivery: 1 to [`MAX_DELIVERY_ID_LEN`]
/// ASCII letters, digits, `.`, `_` or `-` — it becomes part of an event id.
#[must_use]
pub fn is_delivery_id(id: &str) -> bool {
    (1..=MAX_DELIVERY_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The event id of the trigger webhook delivery `delivery` fires for `skill`.
/// First writer wins on it, so a redelivery is the same event.
#[must_use]
pub fn hook_event_id(skill: &str, delivery: &str) -> String {
    format!("hook:{skill}:{delivery}")
}

/// Whether a captured event is one of `skill`'s triggers on UTC `day`, as
/// `schedule_budget.runs_per_day` counts them: a schedule trigger whose window
/// starts that day (`sched:<skill>:<window>`), or a webhook trigger captured
/// that day (`hook:<skill>:<delivery>`). Both readers of the budget — the
/// runner's schedule loop and the gateway's webhook — count with this, so one
/// cap covers both. Only the bookkeeper can file these ids.
#[must_use]
pub fn counts_toward_budget(
    skill: &str,
    event_id: &str,
    source: &str,
    at: Option<&str>,
    day: chrono::NaiveDate,
) -> bool {
    let utc_day = |t: &str| {
        DateTime::parse_from_rfc3339(t)
            .ok()
            .map(|t| t.with_timezone(&Utc).date_naive())
    };
    if source == SCHEDULE_SOURCE {
        return event_id
            .strip_prefix(&format!("sched:{skill}:"))
            .and_then(utc_day)
            == Some(day);
    }
    source == HOOK_SOURCE
        && event_id.starts_with(&format!("hook:{skill}:"))
        && !event_id.ends_with(":skipped")
        && at.and_then(utc_day) == Some(day)
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

    /// The DST rule (crew review PR-1, F6; references/11): a zoned schedule
    /// fires ONCE per local wall-clock time. Spring forward — 02:30 does not
    /// exist on 2027-03-28 in Berlin (02:00 CET jumps to 03:00 CEST): the
    /// window is the first instant after the gap, 03:00 CEST = 01:00 UTC.
    #[test]
    fn a_local_time_skipped_by_spring_forward_fires_at_the_end_of_the_gap() {
        let s = CronSchedule::parse("CRON_TZ=Europe/Berlin 30 2 * * *").unwrap();
        // Just before the gap ends: the latest window is still the 27th's
        // 02:30 CET (01:30 UTC).
        assert_eq!(
            s.latest_window_start(utc(2027, 3, 28, 0, 59, 59)),
            Some(utc(2027, 3, 27, 1, 30, 0))
        );
        // From 03:00 CEST on, the 28th's window has opened — at 03:00 local.
        for now in [utc(2027, 3, 28, 1, 0, 0), utc(2027, 3, 28, 9, 0, 0)] {
            assert_eq!(
                s.latest_window_start(now),
                Some(utc(2027, 3, 28, 1, 0, 0)),
                "{now}"
            );
        }
    }

    /// Fall back — 02:30 occurs twice on 2026-10-25 in Berlin (03:00 CEST
    /// falls back to 02:00 CET): only the FIRST occurrence (CEST, 00:30 UTC)
    /// is a window; the second (CET, 01:30 UTC) is not a new one.
    #[test]
    fn a_local_time_repeated_by_fall_back_fires_at_its_first_occurrence_only() {
        let s = CronSchedule::parse("CRON_TZ=Europe/Berlin 30 2 * * *").unwrap();
        for now in [
            utc(2026, 10, 25, 0, 30, 0),
            utc(2026, 10, 25, 1, 30, 0),
            utc(2026, 10, 25, 1, 45, 0),
            utc(2026, 10, 25, 12, 0, 0),
        ] {
            assert_eq!(
                s.latest_window_start(now),
                Some(utc(2026, 10, 25, 0, 30, 0)),
                "{now}"
            );
        }
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

    #[test]
    fn schedule_and_webhook_triggers_share_one_daily_budget() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let at = Some("2026-11-14T09:00:00Z");
        let counts = |id: &str, source: &str, at: Option<&str>| {
            counts_toward_budget("report", id, source, at, day)
        };
        assert!(counts(
            "sched:report:2026-11-14T07:15:00Z",
            SCHEDULE_SOURCE,
            None
        ));
        assert!(!counts(
            "sched:report:2026-11-13T23:59:00Z",
            SCHEDULE_SOURCE,
            at
        ));
        assert!(counts("hook:report:gh-1", HOOK_SOURCE, at));
        assert!(!counts(
            "hook:report:gh-1",
            HOOK_SOURCE,
            Some("2026-11-15T00:00:01Z")
        ));
        assert!(!counts("hook:report:gh-1:skipped", HOOK_SOURCE, at));
        assert!(
            !counts("hook:other:gh-1", HOOK_SOURCE, at),
            "another skill's"
        );
        assert!(
            !counts("hook:report:gh-1", "manual", at),
            "a source the bookkeeper did not file"
        );
        assert!(!counts("01HXYZ", "manual", at));
    }

    #[test]
    fn a_delivery_id_is_bounded_to_an_event_id_safe_charset() {
        assert!(is_delivery_id("72d3162e-cc78-11e3-81ab-4c9367dc0958"));
        assert!(is_delivery_id("a.b_c-1"));
        for bad in ["", "a b", "a:b", "a/b", "ü", &"x".repeat(129)] {
            assert!(!is_delivery_id(bad), "{bad}");
        }
        assert_eq!(hook_event_id("report", "d-1"), "hook:report:d-1");
    }
}
