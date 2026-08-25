//! When a routine should run.
//!
//! A five-field cron expression (`minute hour day-of-month month day-of-week`)
//! parsed once and matched against a wall-clock time in a named zone.
//!
//! # Why this is not a cron crate
//!
//! The parser is about 150 lines and is the piece most in need of exhaustive,
//! deterministic tests: an unattended agent that fires at the wrong hour, or
//! twice, or never, fails in ways nobody notices for a week. Owning it means
//! every edge (ranges, step syntax, the day-of-month/day-of-week OR rule, the
//! leap day) has a test against a fixed instant rather than the machine's
//! clock.
//!
//! # The day-of-week rule
//!
//! When both day-of-month and day-of-week are restricted, cron fires if either
//! matches, not both. `0 0 1 * MON` is "the 1st, and every Monday", not
//! "Mondays that fall on the 1st". Every cron implementation since Vixie does
//! this, and getting it backwards would make a routine run far more or far
//! less often than its author expected.

use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CronError {
    #[error("a cron expression needs 5 fields (minute hour day month weekday), got {0}")]
    WrongFieldCount(usize),
    #[error("`{0}` is not a valid {1} value")]
    BadValue(String, &'static str),
    #[error("range {0}-{1} is backwards")]
    BackwardsRange(u32, u32),
    #[error("step must be at least 1")]
    ZeroStep,
    #[error("unknown timezone `{0}`")]
    BadTimezone(String),
    #[error("this schedule can never fire")]
    Unsatisfiable,
}

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cron {
    /// The expression as written, so it round-trips and reads back unchanged.
    pub expr: String,
    #[serde(skip)]
    fields: Option<Fields>,
}

#[derive(Debug, Clone, PartialEq)]
struct Fields {
    minute: BTreeSet<u32>,
    hour: BTreeSet<u32>,
    dom: BTreeSet<u32>,
    month: BTreeSet<u32>,
    dow: BTreeSet<u32>,
    dom_restricted: bool,
    dow_restricted: bool,
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(CronError::WrongFieldCount(parts.len()));
        }
        let fields = Fields {
            minute: field(parts[0], 0, 59, "minute")?,
            hour: field(parts[1], 0, 23, "hour")?,
            dom: field(parts[2], 1, 31, "day of month")?,
            month: month_field(parts[3])?,
            dow: dow_field(parts[4])?,
            dom_restricted: parts[2] != "*",
            dow_restricted: parts[4] != "*",
        };
        Ok(Self {
            expr: expr.to_owned(),
            fields: Some(fields),
        })
    }

    fn fields(&self) -> Result<Fields, CronError> {
        match &self.fields {
            Some(f) => Ok(f.clone()),
            // Deserialised from disk: reparse from the stored expression.
            None => Cron::parse(&self.expr)?
                .fields
                .ok_or(CronError::Unsatisfiable),
        }
    }

    /// The first firing strictly after `after`, in `tz`.
    ///
    /// Returns `None` if nothing matches within four years, which only happens
    /// for a genuinely impossible date such as 30 February.
    pub fn next_after(
        &self,
        after: DateTime<Utc>,
        tz: Tz,
    ) -> Result<Option<DateTime<Utc>>, CronError> {
        let f = self.fields()?;
        // Start at the next whole minute: a schedule fires on minute
        // boundaries, and matching the current one would fire twice.
        let mut t = after.with_timezone(&tz) + Duration::minutes(1);
        t = t
            .with_second(0)
            .and_then(|t| t.with_nanosecond(0))
            .ok_or(CronError::Unsatisfiable)?;

        // Four years covers a leap day in every case.
        let limit = t + Duration::days(366 * 4);
        while t < limit {
            if f.matches(
                t.month(),
                t.day(),
                t.weekday().num_days_from_sunday(),
                t.hour(),
                t.minute(),
            ) {
                return Ok(Some(t.with_timezone(&Utc)));
            }
            t += Duration::minutes(1);
        }
        Ok(None)
    }

    /// Whether `at` is a firing time.
    pub fn matches(&self, at: DateTime<Utc>, tz: Tz) -> Result<bool, CronError> {
        let f = self.fields()?;
        let t = at.with_timezone(&tz);
        Ok(f.matches(
            t.month(),
            t.day(),
            t.weekday().num_days_from_sunday(),
            t.hour(),
            t.minute(),
        ))
    }
}

impl Fields {
    fn matches(&self, month: u32, dom: u32, dow: u32, hour: u32, minute: u32) -> bool {
        if !self.minute.contains(&minute) || !self.hour.contains(&hour) {
            return false;
        }
        if !self.month.contains(&month) {
            return false;
        }
        // The Vixie rule: with both day fields restricted, either may match.
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => self.dom.contains(&dom) || self.dow.contains(&dow),
            (true, false) => self.dom.contains(&dom),
            (false, true) => self.dow.contains(&dow),
            (false, false) => true,
        }
    }
}

/// Parse one numeric field: `*`, `n`, `a-b`, `*/n`, `a-b/n`, and lists.
fn field(spec: &str, min: u32, max: u32, name: &'static str) -> Result<BTreeSet<u32>, CronError> {
    let mut out = BTreeSet::new();
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s
                    .parse()
                    .map_err(|_| CronError::BadValue(s.to_owned(), name))?;
                if step == 0 {
                    return Err(CronError::ZeroStep);
                }
                (r, step)
            }
            None => (part, 1),
        };

        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            let a: u32 = a
                .parse()
                .map_err(|_| CronError::BadValue(a.to_owned(), name))?;
            let b: u32 = b
                .parse()
                .map_err(|_| CronError::BadValue(b.to_owned(), name))?;
            if a > b {
                return Err(CronError::BackwardsRange(a, b));
            }
            (a, b)
        } else {
            let v: u32 = range
                .parse()
                .map_err(|_| CronError::BadValue(range.to_owned(), name))?;
            // A bare value with a step means "from here to the end", which is
            // what every cron does with `5/10`.
            if step > 1 {
                (v, max)
            } else {
                (v, v)
            }
        };

        if lo < min || hi > max {
            return Err(CronError::BadValue(part.to_owned(), name));
        }
        let mut v = lo;
        while v <= hi {
            out.insert(v);
            v += step;
        }
    }
    if out.is_empty() {
        return Err(CronError::BadValue(spec.to_owned(), name));
    }
    Ok(out)
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn month_field(spec: &str) -> Result<BTreeSet<u32>, CronError> {
    field(&substitute(spec, &MONTHS, 1), 1, 12, "month")
}

fn dow_field(spec: &str) -> Result<BTreeSet<u32>, CronError> {
    // Sunday is both 0 and 7 in cron; normalise 7 to 0 so matching is simple.
    let set = field(&substitute(spec, &DAYS, 0), 0, 7, "day of week")?;
    Ok(set
        .into_iter()
        .map(|d| if d == 7 { 0 } else { d })
        .collect())
}

/// Replace three-letter names with their numbers, case-insensitively.
fn substitute(spec: &str, names: &[&str], base: u32) -> String {
    let lower = spec.to_ascii_lowercase();
    let mut out = lower;
    for (i, n) in names.iter().enumerate() {
        out = out.replace(n, &(i as u32 + base).to_string());
    }
    out
}

/// Parse a timezone name, defaulting to UTC for an empty string.
pub fn timezone(name: &str) -> Result<Tz, CronError> {
    if name.trim().is_empty() {
        return Ok(Tz::UTC);
    }
    name.parse::<Tz>()
        .map_err(|_| CronError::BadTimezone(name.to_owned()))
}

/// Render a schedule in words, for a confirmation a person can check.
///
/// A routine is unattended, so the one moment to catch "you meant 9am, this
/// says every minute at 9" is when it is created.
pub fn describe(expr: &str) -> String {
    match expr.split_whitespace().collect::<Vec<_>>()[..] {
        [m, h, "*", "*", "*"] if !m.contains('*') && !h.contains('*') => {
            format!("every day at {h}:{m:0>2}")
        }
        [m, h, "*", "*", d] if !m.contains('*') && !h.contains('*') => {
            format!("at {h}:{m:0>2} on {d}")
        }
        ["0", "*", "*", "*", "*"] => "every hour, on the hour".into(),
        [m, "*", "*", "*", "*"] if !m.contains('*') => format!("every hour at :{m:0>2}"),
        _ => format!("cron `{expr}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn next(expr: &str, from: &str, tz: &str) -> String {
        Cron::parse(expr)
            .unwrap()
            .next_after(at(from), timezone(tz).unwrap())
            .unwrap()
            .unwrap()
            .to_rfc3339()
    }

    #[test]
    fn a_daily_schedule_fires_once_a_day() {
        assert_eq!(
            next("0 9 * * *", "2026-08-12T08:00:00Z", "UTC"),
            "2026-08-12T09:00:00+00:00"
        );
        // Already past today: roll to tomorrow.
        assert_eq!(
            next("0 9 * * *", "2026-08-12T09:30:00Z", "UTC"),
            "2026-08-13T09:00:00+00:00"
        );
    }

    #[test]
    fn the_current_minute_never_fires_twice() {
        // Exactly on the boundary: the next firing must be the following one,
        // or a scheduler that ticks twice in one minute runs the routine
        // twice.
        assert_eq!(
            next("0 9 * * *", "2026-08-12T09:00:00Z", "UTC"),
            "2026-08-13T09:00:00+00:00"
        );
    }

    #[test]
    fn a_named_timezone_is_honoured_including_its_offset() {
        // 09:00 in Kolkata is 03:30 UTC.
        assert_eq!(
            next("0 9 * * *", "2026-08-12T00:00:00Z", "Asia/Kolkata"),
            "2026-08-12T03:30:00+00:00"
        );
    }

    #[test]
    fn a_schedule_survives_a_daylight_saving_change() {
        // New York moves to EST on 2026-11-01. A 09:00 local routine must
        // still be 09:00 local the next day, not shifted by an hour.
        let tz = timezone("America/New_York").unwrap();
        let c = Cron::parse("0 9 * * *").unwrap();
        let after_change = c
            .next_after(at("2026-11-01T20:00:00Z"), tz)
            .unwrap()
            .unwrap();
        assert_eq!(
            after_change.with_timezone(&tz).hour(),
            9,
            "the routine drifted across the DST boundary"
        );
    }

    #[test]
    fn weekday_names_and_numbers_both_work() {
        let a = next("0 9 * * MON", "2026-08-12T00:00:00Z", "UTC");
        let b = next("0 9 * * 1", "2026-08-12T00:00:00Z", "UTC");
        assert_eq!(a, b);
        // 2026-08-12 is a Wednesday; the next Monday is the 17th.
        assert!(a.starts_with("2026-08-17"), "got {a}");
    }

    #[test]
    fn sunday_is_both_zero_and_seven() {
        let a = Cron::parse("0 9 * * 0").unwrap();
        let b = Cron::parse("0 9 * * 7").unwrap();
        let tz = Tz::UTC;
        assert_eq!(
            a.next_after(at("2026-08-12T00:00:00Z"), tz).unwrap(),
            b.next_after(at("2026-08-12T00:00:00Z"), tz).unwrap()
        );
    }

    #[test]
    fn day_of_month_and_weekday_are_or_not_and() {
        // The Vixie rule: `1 * MON` means the 1st or any Monday. Getting this
        // backwards changes how often a routine runs by a lot.
        let c = Cron::parse("0 9 1 * MON").unwrap();
        // 2026-09-01 is a Tuesday, and must still fire because it is the 1st.
        assert!(c.matches(at("2026-09-01T09:00:00Z"), Tz::UTC).unwrap());
        // 2026-09-07 is a Monday, and must fire because it is a Monday.
        assert!(c.matches(at("2026-09-07T09:00:00Z"), Tz::UTC).unwrap());
        // A Wednesday that is not the 1st must not.
        assert!(!c.matches(at("2026-09-09T09:00:00Z"), Tz::UTC).unwrap());
    }

    #[test]
    fn steps_ranges_and_lists_parse() {
        let every_15 = Cron::parse("*/15 * * * *").unwrap();
        for m in [0, 15, 30, 45] {
            assert!(every_15
                .matches(at(&format!("2026-08-12T10:{m:02}:00Z")), Tz::UTC)
                .unwrap());
        }
        assert!(!every_15
            .matches(at("2026-08-12T10:07:00Z"), Tz::UTC)
            .unwrap());

        let workday = Cron::parse("0 9-17 * * MON-FRI").unwrap();
        assert!(workday
            .matches(at("2026-08-12T09:00:00Z"), Tz::UTC)
            .unwrap());
        assert!(workday
            .matches(at("2026-08-12T17:00:00Z"), Tz::UTC)
            .unwrap());
        assert!(!workday
            .matches(at("2026-08-12T18:00:00Z"), Tz::UTC)
            .unwrap());
        // 2026-08-15 is a Saturday.
        assert!(!workday
            .matches(at("2026-08-15T09:00:00Z"), Tz::UTC)
            .unwrap());

        let list = Cron::parse("0,30 9,17 * * *").unwrap();
        assert!(list.matches(at("2026-08-12T17:30:00Z"), Tz::UTC).unwrap());
        assert!(!list.matches(at("2026-08-12T17:15:00Z"), Tz::UTC).unwrap());
    }

    #[test]
    fn a_leap_day_schedule_finds_the_next_leap_year() {
        let c = Cron::parse("0 9 29 2 *").unwrap();
        let n = c
            .next_after(at("2026-08-12T00:00:00Z"), Tz::UTC)
            .unwrap()
            .unwrap();
        assert!(n.to_rfc3339().starts_with("2028-02-29"), "got {n}");
    }

    #[test]
    fn an_impossible_date_returns_nothing_rather_than_looping_forever() {
        let c = Cron::parse("0 9 30 2 *").unwrap();
        assert_eq!(
            c.next_after(at("2026-08-12T00:00:00Z"), Tz::UTC).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_expressions_are_rejected_with_a_reason() {
        assert_eq!(Cron::parse("0 9 * *"), Err(CronError::WrongFieldCount(4)));
        assert!(matches!(
            Cron::parse("0 99 * * *"),
            Err(CronError::BadValue(_, _))
        ));
        assert!(matches!(
            Cron::parse("0 9 * * FUNDAY"),
            Err(CronError::BadValue(_, _))
        ));
        assert_eq!(Cron::parse("*/0 * * * *"), Err(CronError::ZeroStep));
        assert_eq!(
            Cron::parse("0 17-9 * * *"),
            Err(CronError::BackwardsRange(17, 9))
        );
    }

    #[test]
    fn an_unknown_timezone_is_rejected() {
        assert!(matches!(
            timezone("Mars/Olympus"),
            Err(CronError::BadTimezone(_))
        ));
        assert_eq!(timezone("").unwrap(), Tz::UTC);
        assert_eq!(timezone("UTC").unwrap(), Tz::UTC);
    }

    #[test]
    fn a_schedule_survives_a_round_trip_through_json() {
        let c = Cron::parse("*/15 9-17 * * MON-FRI").unwrap();
        let j = serde_json::to_string(&c).unwrap();
        let back: Cron = serde_json::from_str(&j).unwrap();
        // The parsed fields are not serialised, so this also proves the
        // reparse-on-demand path works.
        assert_eq!(back.expr, c.expr);
        assert!(back.matches(at("2026-08-12T09:15:00Z"), Tz::UTC).unwrap());
    }

    #[test]
    fn schedules_read_back_in_words() {
        assert_eq!(describe("0 9 * * *"), "every day at 9:00");
        assert_eq!(describe("30 8 * * *"), "every day at 8:30");
        assert_eq!(describe("0 9 * * MON"), "at 9:00 on MON");
        assert_eq!(describe("0 * * * *"), "every hour, on the hour");
        assert_eq!(describe("15 * * * *"), "every hour at :15");
        assert_eq!(describe("*/5 * * * *"), "cron `*/5 * * * *`");
    }
}
