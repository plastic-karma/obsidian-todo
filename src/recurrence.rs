use std::collections::HashSet;
use std::fmt;

use chrono::{Datelike, Days, Months, NaiveDate, Weekday};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecurrenceMode {
    Schedule,
    Completion,
}

impl RecurrenceMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "schedule" => Ok(Self::Schedule),
            "completion" => Ok(Self::Completion),
            _ => Err(Error::validation(
                "invalid_recurrence_mode",
                "recurrence_from must be 'schedule' or 'completion'",
            )
            .with_field("recurrence_from")),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Completion => "completion",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceRule {
    frequency: Frequency,
    interval: u64,
    by_day: Vec<Weekday>,
    by_month_day: Vec<u32>,
    by_month: Vec<u32>,
}

impl RecurrenceRule {
    pub fn parse(source: &str) -> Result<Self> {
        if source.is_empty() {
            return Err(invalid_rule("Recurrence rule cannot be empty"));
        }
        let mut frequency = None;
        let mut interval = 1;
        let mut by_day = Vec::new();
        let mut by_month_day = Vec::new();
        let mut by_month = Vec::new();
        let mut clauses = HashSet::new();

        for clause in source.split(';') {
            let Some((raw_name, raw_value)) = clause.split_once('=') else {
                return Err(invalid_rule(format!(
                    "Recurrence clause {clause:?} must contain '='"
                )));
            };
            if raw_name.is_empty() || raw_value.is_empty() || raw_value.contains('=') {
                return Err(invalid_rule(format!(
                    "Recurrence clause {clause:?} has an empty or malformed value"
                )));
            }
            let name = raw_name.to_ascii_uppercase();
            if !clauses.insert(name.clone()) {
                return Err(invalid_rule(format!(
                    "Recurrence clause {name} appears more than once"
                )));
            }
            let value = raw_value.to_ascii_uppercase();
            match name.as_str() {
                "FREQ" => {
                    frequency = Some(match value.as_str() {
                        "DAILY" => Frequency::Daily,
                        "WEEKLY" => Frequency::Weekly,
                        "MONTHLY" => Frequency::Monthly,
                        "YEARLY" => Frequency::Yearly,
                        _ => {
                            return Err(Error::unsupported(
                                "unsupported_recurrence",
                                format!("Unsupported recurrence frequency {value:?}"),
                            )
                            .with_field("recurrence"));
                        }
                    });
                }
                "INTERVAL" => {
                    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(invalid_rule("INTERVAL must be a positive integer"));
                    }
                    interval = value
                        .parse::<u64>()
                        .ok()
                        .filter(|interval| *interval > 0)
                        .ok_or_else(|| invalid_rule("INTERVAL must be a positive integer"))?;
                }
                "BYDAY" => {
                    by_day = parse_unique_list(&value, parse_weekday, "BYDAY")?;
                    by_day.sort_by_key(|weekday| weekday.num_days_from_monday());
                }
                "BYMONTHDAY" => {
                    by_month_day = parse_number_list(&value, 1, 31, "BYMONTHDAY")?;
                }
                "BYMONTH" => {
                    by_month = parse_number_list(&value, 1, 12, "BYMONTH")?;
                }
                _ => {
                    return Err(Error::unsupported(
                        "unsupported_recurrence",
                        format!("Unsupported recurrence clause {name}"),
                    )
                    .with_field("recurrence"));
                }
            }
        }

        let frequency = frequency.ok_or_else(|| invalid_rule("FREQ is required"))?;
        if !by_day.is_empty() && frequency != Frequency::Weekly {
            return Err(invalid_rule("BYDAY is allowed only with FREQ=WEEKLY"));
        }
        if !by_month_day.is_empty() && !matches!(frequency, Frequency::Monthly | Frequency::Yearly)
        {
            return Err(invalid_rule(
                "BYMONTHDAY is allowed only with FREQ=MONTHLY or FREQ=YEARLY",
            ));
        }
        if !by_month.is_empty() && frequency != Frequency::Yearly {
            return Err(invalid_rule("BYMONTH is allowed only with FREQ=YEARLY"));
        }

        Ok(Self {
            frequency,
            interval,
            by_day,
            by_month_day,
            by_month,
        })
    }

    #[must_use]
    pub const fn frequency(&self) -> Frequency {
        self.frequency
    }

    #[must_use]
    pub const fn interval(&self) -> u64 {
        self.interval
    }

    #[must_use]
    pub fn by_day(&self) -> &[Weekday] {
        &self.by_day
    }

    #[must_use]
    pub fn by_month_day(&self) -> &[u32] {
        &self.by_month_day
    }

    #[must_use]
    pub fn by_month(&self) -> &[u32] {
        &self.by_month
    }

    #[must_use]
    pub fn matches_date(&self, date: NaiveDate) -> bool {
        match self.frequency {
            Frequency::Daily => true,
            Frequency::Weekly => self.by_day.is_empty() || self.by_day.contains(&date.weekday()),
            Frequency::Monthly => {
                self.by_month_day.is_empty() || self.by_month_day.contains(&date.day())
            }
            Frequency::Yearly => {
                (self.by_month.is_empty() || self.by_month.contains(&date.month()))
                    && (self.by_month_day.is_empty() || self.by_month_day.contains(&date.day()))
            }
        }
    }

    pub fn next_due(
        &self,
        current_due: NaiveDate,
        completed_on: NaiveDate,
        mode: RecurrenceMode,
    ) -> Result<NaiveDate> {
        let (anchor, threshold) = match mode {
            RecurrenceMode::Schedule => (current_due, current_due.max(completed_on)),
            RecurrenceMode::Completion => (completed_on, completed_on),
        };
        match self.frequency {
            Frequency::Daily => self.next_daily(anchor, threshold),
            Frequency::Weekly => self.next_weekly(anchor, threshold),
            Frequency::Monthly => self.next_monthly(anchor, threshold),
            Frequency::Yearly => self.next_yearly(anchor, threshold),
        }
    }

    fn next_daily(&self, anchor: NaiveDate, threshold: NaiveDate) -> Result<NaiveDate> {
        let elapsed = u64::try_from((threshold - anchor).num_days()).unwrap_or(0);
        let periods = elapsed / self.interval + 1;
        let days = periods
            .checked_mul(self.interval)
            .ok_or_else(date_overflow)?;
        anchor
            .checked_add_days(Days::new(days))
            .ok_or_else(date_overflow)
    }

    fn next_weekly(&self, anchor: NaiveDate, threshold: NaiveDate) -> Result<NaiveDate> {
        let default_weekday = anchor.weekday();
        let weekdays = if self.by_day.is_empty() {
            std::slice::from_ref(&default_weekday)
        } else {
            &self.by_day
        };
        let anchor_week = anchor
            .checked_sub_days(Days::new(u64::from(
                anchor.weekday().num_days_from_monday(),
            )))
            .ok_or_else(date_overflow)?;
        let elapsed_weeks = u64::try_from((threshold - anchor_week).num_days() / 7).unwrap_or(0);
        let mut period = elapsed_weeks / self.interval;
        for _ in 0..2 {
            let week_offset = period
                .checked_mul(self.interval)
                .and_then(|value| value.checked_mul(7))
                .ok_or_else(date_overflow)?;
            let week = anchor_week
                .checked_add_days(Days::new(week_offset))
                .ok_or_else(date_overflow)?;
            for weekday in weekdays {
                let candidate = week
                    .checked_add_days(Days::new(u64::from(weekday.num_days_from_monday())))
                    .ok_or_else(date_overflow)?;
                if candidate > threshold {
                    return Ok(candidate);
                }
            }
            period = period.checked_add(1).ok_or_else(date_overflow)?;
        }
        Err(no_occurrence())
    }

    fn next_monthly(&self, anchor: NaiveDate, threshold: NaiveDate) -> Result<NaiveDate> {
        let default_day = anchor.day();
        let days = if self.by_month_day.is_empty() {
            std::slice::from_ref(&default_day)
        } else {
            &self.by_month_day
        };
        let elapsed_months = month_index(threshold)
            .checked_sub(month_index(anchor))
            .and_then(|months| u64::try_from(months).ok())
            .unwrap_or(0);
        let mut period = elapsed_months / self.interval;
        let cycle = 4_800 / gcd(self.interval, 4_800) + 1;
        for _ in 0..cycle {
            let month_offset = period
                .checked_mul(self.interval)
                .ok_or_else(date_overflow)?;
            let month_offset = u32::try_from(month_offset).map_err(|_| date_overflow())?;
            let month = anchor
                .with_day(1)
                .and_then(|date| date.checked_add_months(Months::new(month_offset)))
                .ok_or_else(date_overflow)?;
            for day in days {
                if let Some(candidate) = month.with_day(*day) {
                    if candidate > threshold {
                        return Ok(candidate);
                    }
                }
            }
            period = period.checked_add(1).ok_or_else(date_overflow)?;
        }
        Err(no_occurrence())
    }

    fn next_yearly(&self, anchor: NaiveDate, threshold: NaiveDate) -> Result<NaiveDate> {
        let default_month = anchor.month();
        let months = if self.by_month.is_empty() {
            std::slice::from_ref(&default_month)
        } else {
            &self.by_month
        };
        let default_day = anchor.day();
        let days = if self.by_month_day.is_empty() {
            std::slice::from_ref(&default_day)
        } else {
            &self.by_month_day
        };
        let elapsed_years = u64::try_from(threshold.year() - anchor.year()).unwrap_or(0);
        let mut period = elapsed_years / self.interval;
        let cycle = 400 / gcd(self.interval, 400) + 1;
        for _ in 0..cycle {
            let years = period
                .checked_mul(self.interval)
                .ok_or_else(date_overflow)?;
            let year_delta = i32::try_from(years).map_err(|_| date_overflow())?;
            let year = anchor
                .year()
                .checked_add(year_delta)
                .ok_or_else(date_overflow)?;
            for month in months {
                for day in days {
                    if let Some(candidate) = NaiveDate::from_ymd_opt(year, *month, *day) {
                        if candidate > threshold {
                            return Ok(candidate);
                        }
                    }
                }
            }
            period = period.checked_add(1).ok_or_else(date_overflow)?;
        }
        Err(no_occurrence())
    }
}

impl fmt::Display for RecurrenceRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let frequency = match self.frequency {
            Frequency::Daily => "DAILY",
            Frequency::Weekly => "WEEKLY",
            Frequency::Monthly => "MONTHLY",
            Frequency::Yearly => "YEARLY",
        };
        write!(formatter, "FREQ={frequency};INTERVAL={}", self.interval)?;
        if !self.by_day.is_empty() {
            write!(formatter, ";BYDAY=")?;
            join_display(formatter, self.by_day.iter().map(|day| weekday_name(*day)))?;
        }
        if !self.by_month_day.is_empty() {
            write!(formatter, ";BYMONTHDAY=")?;
            join_display(formatter, self.by_month_day.iter())?;
        }
        if !self.by_month.is_empty() {
            write!(formatter, ";BYMONTH=")?;
            join_display(formatter, self.by_month.iter())?;
        }
        Ok(())
    }
}

fn join_display<T: fmt::Display>(
    formatter: &mut fmt::Formatter<'_>,
    values: impl IntoIterator<Item = T>,
) -> fmt::Result {
    let mut separator = "";
    for value in values {
        write!(formatter, "{separator}{value}")?;
        separator = ",";
    }
    Ok(())
}

fn parse_unique_list<T: Eq + std::hash::Hash + Copy>(
    value: &str,
    parser: impl Fn(&str) -> Result<T>,
    clause: &str,
) -> Result<Vec<T>> {
    let mut values = Vec::new();
    let mut seen = HashSet::new();
    for item in value.split(',') {
        if item.is_empty() {
            return Err(invalid_rule(format!("{clause} contains an empty item")));
        }
        let parsed = parser(item)?;
        if !seen.insert(parsed) {
            return Err(invalid_rule(format!(
                "{clause} contains duplicate value {item:?}"
            )));
        }
        values.push(parsed);
    }
    Ok(values)
}

fn parse_number_list(value: &str, minimum: u32, maximum: u32, clause: &str) -> Result<Vec<u32>> {
    let mut values = parse_unique_list(
        value,
        |item| {
            if item.starts_with('-') {
                return Err(Error::unsupported(
                    "unsupported_recurrence",
                    format!("{clause} does not support negative values in v1"),
                )
                .with_field("recurrence"));
            }
            if !item.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid_rule(format!(
                    "{clause} values must be integers from {minimum} through {maximum}"
                )));
            }
            item.parse::<u32>()
                .ok()
                .filter(|number| (*number >= minimum) && (*number <= maximum))
                .ok_or_else(|| {
                    invalid_rule(format!(
                        "{clause} values must be integers from {minimum} through {maximum}"
                    ))
                })
        },
        clause,
    )?;
    values.sort_unstable();
    Ok(values)
}

fn parse_weekday(value: &str) -> Result<Weekday> {
    match value {
        "MO" => Ok(Weekday::Mon),
        "TU" => Ok(Weekday::Tue),
        "WE" => Ok(Weekday::Wed),
        "TH" => Ok(Weekday::Thu),
        "FR" => Ok(Weekday::Fri),
        "SA" => Ok(Weekday::Sat),
        "SU" => Ok(Weekday::Sun),
        _ => Err(Error::unsupported(
            "unsupported_recurrence",
            format!("Unsupported BYDAY value {value:?}; ordinal weekdays are not supported"),
        )
        .with_field("recurrence")),
    }
}

const fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

const fn gcd(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn month_index(date: NaiveDate) -> i64 {
    i64::from(date.year()) * 12 + i64::from(date.month0())
}

fn invalid_rule(message: impl Into<String>) -> Error {
    Error::validation("invalid_recurrence", message).with_field("recurrence")
}

fn date_overflow() -> Error {
    Error::validation(
        "recurrence_date_overflow",
        "Recurrence has no next date in the supported calendar range",
    )
    .with_field("recurrence")
}

fn no_occurrence() -> Error {
    Error::validation(
        "recurrence_has_no_occurrence",
        "Recurrence selections never produce a valid calendar date",
    )
    .with_field("recurrence")
}

pub fn parse_date(value: &str, field: &str) -> Result<NaiveDate> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        return Err(Error::validation(
            "invalid_date",
            format!("{field} must be a date in YYYY-MM-DD format"),
        )
        .with_field(field));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
        Error::validation(
            "invalid_date",
            format!("{field} is not a valid Gregorian calendar date"),
        )
        .with_field(field)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        parse_date(value, "date").expect("valid date")
    }

    fn next(rule: &str, due: &str, completed: &str, mode: RecurrenceMode) -> NaiveDate {
        RecurrenceRule::parse(rule)
            .expect("valid rule")
            .next_due(date(due), date(completed), mode)
            .expect("next occurrence")
    }

    #[test]
    fn parses_and_normalizes_every_supported_clause() {
        let rule = RecurrenceRule::parse("bymonth=12,2;freq=yearly;bymonthday=31,1;interval=2")
            .expect("rule");
        assert_eq!(
            rule.to_string(),
            "FREQ=YEARLY;INTERVAL=2;BYMONTHDAY=1,31;BYMONTH=2,12"
        );
        assert_eq!(
            RecurrenceRule::parse("FREQ=DAILY")
                .expect("daily")
                .to_string(),
            "FREQ=DAILY;INTERVAL=1"
        );
    }

    #[test]
    fn rejects_unknown_duplicate_and_misplaced_clauses() {
        for source in [
            "FREQ=DAILY;COUNT=2",
            "FREQ=DAILY;FREQ=WEEKLY",
            "FREQ=DAILY;BYDAY=MO",
            "FREQ=WEEKLY;BYMONTHDAY=1",
            "FREQ=MONTHLY;BYMONTH=2",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=WEEKLY;BYDAY=1MO",
            "FREQ=MONTHLY;BYMONTHDAY=-1",
            "FREQ=YEARLY;BYMONTH=13",
        ] {
            assert!(RecurrenceRule::parse(source).is_err(), "{source}");
        }
    }
    #[test]
    fn every_documented_unsupported_feature_returns_exit_seven() {
        for clause in [
            "DTSTART=20260901",
            "COUNT=2",
            "UNTIL=20261231",
            "WKST=MO",
            "BYSETPOS=1",
            "BYYEARDAY=1",
            "BYWEEKNO=1",
            "BYHOUR=9",
            "BYMINUTE=30",
            "BYSECOND=0",
        ] {
            let source = format!("FREQ=DAILY;{clause}");
            let error = RecurrenceRule::parse(&source).expect_err("unsupported clause");
            assert_eq!(error.code(), "unsupported_recurrence", "{source}");
            assert_eq!(error.exit_code(), 7, "{source}");
        }
        for source in [
            "FREQ=WEEKLY;BYDAY=1MO",
            "FREQ=MONTHLY;BYMONTHDAY=-1",
            "FREQ=YEARLY;BYMONTH=-1",
        ] {
            let error = RecurrenceRule::parse(source).expect_err("unsupported value");
            assert_eq!(error.code(), "unsupported_recurrence", "{source}");
            assert_eq!(error.exit_code(), 7, "{source}");
        }
    }

    #[test]
    fn daily_interval_anchors_to_schedule_or_completion() {
        assert_eq!(
            next(
                "FREQ=DAILY;INTERVAL=3",
                "2026-09-01",
                "2026-09-09",
                RecurrenceMode::Schedule,
            ),
            date("2026-09-10")
        );
        assert_eq!(
            next(
                "FREQ=DAILY;INTERVAL=3",
                "2026-09-01",
                "2026-09-09",
                RecurrenceMode::Completion,
            ),
            date("2026-09-12")
        );
    }

    #[test]
    fn weekly_multiple_days_and_interval_keep_week_alignment() {
        let rule = "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TH";
        assert_eq!(
            next(rule, "2026-09-07", "2026-09-07", RecurrenceMode::Schedule),
            date("2026-09-10")
        );
        assert_eq!(
            next(rule, "2026-09-07", "2026-09-11", RecurrenceMode::Schedule),
            date("2026-09-21")
        );
        assert_eq!(
            next(rule, "2026-09-07", "2026-09-09", RecurrenceMode::Completion),
            date("2026-09-10")
        );
    }

    #[test]
    fn weekly_early_and_late_completion_use_strictly_later_occurrence() {
        let rule = "FREQ=WEEKLY;BYDAY=MO";
        assert_eq!(
            next(rule, "2026-09-07", "2026-09-05", RecurrenceMode::Schedule),
            date("2026-09-14")
        );
        assert_eq!(
            next(rule, "2026-09-07", "2026-09-09", RecurrenceMode::Schedule),
            date("2026-09-14")
        );
    }

    #[test]
    fn monthly_skips_missing_29th_30th_and_31st() {
        assert_eq!(
            next(
                "FREQ=MONTHLY;BYMONTHDAY=29",
                "2025-01-29",
                "2025-01-29",
                RecurrenceMode::Schedule,
            ),
            date("2025-03-29")
        );
        assert_eq!(
            next(
                "FREQ=MONTHLY;BYMONTHDAY=30",
                "2026-01-30",
                "2026-01-30",
                RecurrenceMode::Schedule,
            ),
            date("2026-03-30")
        );
        assert_eq!(
            next(
                "FREQ=MONTHLY;BYMONTHDAY=31",
                "2026-01-31",
                "2026-01-31",
                RecurrenceMode::Schedule,
            ),
            date("2026-03-31")
        );
        assert_eq!(
            next(
                "FREQ=MONTHLY;BYMONTHDAY=29",
                "2024-01-29",
                "2024-01-29",
                RecurrenceMode::Schedule,
            ),
            date("2024-02-29")
        );
    }

    #[test]
    fn yearly_february_29_skips_non_leap_years() {
        assert_eq!(
            next(
                "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29",
                "2024-02-29",
                "2024-02-29",
                RecurrenceMode::Schedule,
            ),
            date("2028-02-29")
        );
    }

    #[test]
    fn monthly_and_yearly_defaults_derive_from_anchor() {
        assert_eq!(
            next(
                "FREQ=MONTHLY;INTERVAL=1",
                "2026-01-31",
                "2026-01-31",
                RecurrenceMode::Schedule,
            ),
            date("2026-03-31")
        );
        assert_eq!(
            next(
                "FREQ=YEARLY;INTERVAL=1",
                "2024-02-29",
                "2024-02-29",
                RecurrenceMode::Schedule,
            ),
            date("2028-02-29")
        );
    }

    #[test]
    fn date_parser_rejects_bad_shape_and_non_leap_day() {
        for invalid in ["2026-2-01", "2026/02/01", "2026-02-29", "abcd-ef-gh"] {
            assert!(parse_date(invalid, "due_date").is_err(), "{invalid}");
        }
        assert_eq!(
            date("2024-02-29"),
            NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()
        );
    }
}
