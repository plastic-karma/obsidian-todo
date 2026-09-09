use std::{ops::Range, sync::LazyLock};

use chrono::{
    DateTime, Datelike, Days, Months, NaiveDate, NaiveTime, TimeDelta, TimeZone, Timelike,
};
use regex::Regex;

use crate::{
    commands::task::TaskFilter,
    error::{Error, Result},
    model::{validate_name, validate_project_slug, validate_tag, validate_url},
    recurrence::validate_date_value,
};

/// A task or read-only command resolved without consulting the task store.
#[derive(Debug, Clone)]
pub enum ParsedInput {
    Task(ParsedTaskInput),
    List(TaskFilter),
}

/// A captured task with resolved metadata and local due date/time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTaskInput {
    pub name: String,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub url: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub due_time: Option<NaiveTime>,
}

struct Patterns {
    tokens: Regex,
    words: Regex,
    clocks: Regex,
    valid_clock: Regex,
    urls: Regex,
    separator: Regex,
}

static PATTERNS: LazyLock<Patterns> = LazyLock::new(|| {
    Patterns {
    tokens: Regex::new(r"\S+").expect("valid token pattern"),
    // Marks belong to the surrounding Unicode word, including decomposed accents.
    words: Regex::new(r"[\p{L}\p{N}\p{M}_]+").expect("valid word pattern"),
    // Consume the entire clock-like token before validating; never salvage a suffix.
    clocks: Regex::new(r"(?i)[0-9]+(?::[0-9]+)+(?:[ \t]*[ap]m[\p{L}\p{N}\p{M}_]*)?|[0-9]+[ \t]*[ap]m[\p{L}\p{N}\p{M}_]*")
        .expect("valid clock candidate pattern"),
    valid_clock: Regex::new(r"(?i)\A(?:([0-9]{2}):([0-9]{2})|([0-9]{1,2})(?::([0-9]{2}))?[ \t]*(am|pm))\z")
        .expect("valid clock pattern"),
    urls: Regex::new(r#"(?i)https?://[^\s<>"`]+"#).expect("valid URL candidate pattern"),
    separator: Regex::new(r"\A[\s\p{P}]\z").expect("valid separator pattern"),
}
});

struct Word<'a> {
    text: &'a str,
    range: Range<usize>,
}

#[derive(Clone, Copy)]
enum DateMeaning {
    Days(u64),
    Weeks(u64),
    Months(u64),
    Weekday(u32),
    Resolved(NaiveDate),
}

/// Parse a task capture or a leading `/list` command. List filters use `#project`,
/// `@tag`, and `!state` tokens; unknown commands and bare arguments are errors.
/// Task captures recognize metadata, explicit web links and natural date/clock
/// phrases. The injected timestamp determines today and relative clock values.
pub fn parse_line<Tz: TimeZone>(line: &str, reference: &DateTime<Tz>) -> Result<ParsedInput> {
    if line
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(Error::validation(
            "invalid_input",
            "Input must be a single line without control characters",
        )
        .with_field("input"));
    }
    let mut tokens = line.split_whitespace();
    if let Some(command) = tokens.next().filter(|token| token.starts_with('/')) {
        if command != "/list" {
            return Err(Error::usage(
                "unknown_input_command",
                format!("Unknown input command {command:?}; use /list"),
            )
            .with_field("input"));
        }
        let mut filter = TaskFilter {
            include_terminal: true,
            ..TaskFilter::default()
        };
        for token in tokens {
            let (values, value) = if let Some(project) = token.strip_prefix('#') {
                validate_project_slug(project)?;
                (&mut filter.projects, project)
            } else if let Some(tag) = token.strip_prefix('@') {
                validate_tag(tag)?;
                (&mut filter.tags, tag)
            } else if let Some(state) = token.strip_prefix('!').filter(|state| !state.is_empty()) {
                // Membership in the configured workflow is checked by task::list.
                (&mut filter.states, state)
            } else {
                return Err(Error::usage(
                    "invalid_input_filter",
                    format!("Invalid list filter {token:?}; use #project, @tag, or !state"),
                )
                .with_field("input"));
            };
            if !values.iter().any(|existing| existing == value) {
                values.push(value.to_owned());
            }
        }
        return Ok(ParsedInput::List(filter));
    }
    parse_task_line(line, reference).map(ParsedInput::Task)
}

fn parse_task_line<Tz: TimeZone>(line: &str, reference: &DateTime<Tz>) -> Result<ParsedTaskInput> {
    validate_name(line, "name")?;
    let mut parsed = ParsedTaskInput {
        name: String::new(),
        projects: Vec::new(),
        tags: Vec::new(),
        url: None,
        due_date: None,
        due_time: None,
    };
    let mut removals = Vec::new();
    let mut protected = Vec::new();
    for token in PATTERNS.tokens.find_iter(line) {
        let value = token.as_str();
        let metadata = if let Some(project) = value.strip_prefix('#') {
            validate_project_slug(project)?;
            if !parsed.projects.iter().any(|item| item == project) {
                parsed.projects.push(project.to_owned());
            }
            true
        } else if let Some(tag) = value.strip_prefix('@') {
            validate_tag(tag)?;
            if !parsed.tags.iter().any(|item| item == tag) {
                parsed.tags.push(tag.to_owned());
            }
            true
        } else {
            false
        };
        if metadata {
            removals.push(token.range());
        }
        if metadata || value.contains(['/', '\\', '@', '=', '#']) {
            protected.push(token.range());
        }
        if !metadata && parsed.url.is_none() {
            parsed.url = first_url(value).map(str::to_owned);
        }
    }
    let words: Vec<_> = PATTERNS
        .words
        .find_iter(line)
        .map(|word| Word {
            text: word.as_str(),
            range: word.range(),
        })
        .collect();
    let mut date_candidate = None;
    let mut time_candidate: Option<(Range<usize>, NaiveTime)> = None;
    for (index, word) in words.iter().enumerate() {
        if overlaps_any(&word.range, &protected) || identifier_boundary(line, &word.range) {
            continue;
        }
        let mut range = word.range.clone();
        let normalized = word.text.to_ascii_lowercase();
        let meaning = match normalized.as_str() {
            "today" | "tod" => Some(DateMeaning::Days(0)),
            "tomorrow" | "tom" => Some(DateMeaning::Days(1)),
            "next" => words
                .get(index + 1)
                .filter(|next| {
                    whitespace_between(line, word.range.end, next.range.start)
                        && !overlaps_any(&next.range, &protected)
                        && !identifier_boundary(line, &next.range)
                })
                .and_then(|next| {
                    let meaning = if next.text.eq_ignore_ascii_case("week") {
                        DateMeaning::Weeks(1)
                    } else if next.text.eq_ignore_ascii_case("month") {
                        DateMeaning::Months(1)
                    } else {
                        return None;
                    };
                    range.end = next.range.end;
                    Some(meaning)
                }),
            "in" => {
                if let (Some(amount), Some(unit)) = (words.get(index + 1), words.get(index + 2)) {
                    if whitespace_between(line, word.range.end, amount.range.start)
                        && whitespace_between(line, amount.range.end, unit.range.start)
                        && !overlaps_any(&(amount.range.start..unit.range.end), &protected)
                        && !identifier_boundary(line, &unit.range)
                    {
                        if let Some(amount) = positive_integer(amount.text) {
                            range.end = unit.range.end;
                            let unit = unit.text.to_ascii_lowercase();
                            match unit.strip_suffix('s').unwrap_or(&unit) {
                                "day" => Some(DateMeaning::Days(amount)),
                                "week" => Some(DateMeaning::Weeks(amount)),
                                "month" => Some(DateMeaning::Months(amount)),
                                "hour" | "minute" => {
                                    let multiplier =
                                        if unit.starts_with("hour") { 3600 } else { 60 };
                                    let (date, time) =
                                        relative_time(reference, amount, multiplier)?;
                                    time_candidate = Some((range.clone(), time));
                                    Some(DateMeaning::Resolved(date))
                                }
                                _ => None,
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            other => weekday(other).map(DateMeaning::Weekday),
        };
        if let Some(meaning) = meaning {
            date_candidate = Some((range, meaning));
        }
    }
    for candidate in PATTERNS.clocks.find_iter(line) {
        let mut range = candidate.range();
        if overlaps_any(&range, &protected) || !clock_boundaries(line, &range) {
            continue;
        }
        let Some(time) = clock_time(candidate.as_str()) else {
            continue;
        };
        if let Some(previous) = words
            .iter()
            .rev()
            .find(|word| word.range.end <= range.start)
        {
            if previous.text.eq_ignore_ascii_case("at")
                && whitespace_between(line, previous.range.end, range.start)
                && !overlaps_any(&previous.range, &protected)
                && !identifier_boundary(line, &previous.range)
            {
                range.start = previous.range.start;
            }
        }
        if time_candidate
            .as_ref()
            .is_none_or(|(previous, _)| range.start > previous.start)
        {
            time_candidate = Some((range, time));
        }
    }
    if let Some((range, meaning)) = date_candidate {
        parsed.due_date = Some(resolve_date(meaning, reference.date_naive())?);
        removals.push(expand_phrase(line, range, &protected));
    } else if time_candidate.is_some() {
        let today = reference.date_naive();
        validate_date_value(today, "due_date")?;
        parsed.due_date = Some(today);
    }
    if let Some((range, time)) = time_candidate {
        parsed.due_time = Some(time);
        removals.push(expand_phrase(line, range, &protected));
    }
    parsed.name = remove_ranges(line, removals);
    validate_name(&parsed.name, "name")?;
    Ok(parsed)
}

fn overlaps_any(range: &Range<usize>, protected: &[Range<usize>]) -> bool {
    protected
        .iter()
        .any(|other| range.start < other.end && other.start < range.end)
}

fn whitespace_between(line: &str, start: usize, end: usize) -> bool {
    start < end && line[start..end].chars().all(char::is_whitespace)
}

fn identifier_boundary(line: &str, range: &Range<usize>) -> bool {
    let before = line[..range.start].chars().next_back();
    let mut after = line[range.end..].chars();
    let next = after.next();
    before.is_some_and(|c| matches!(c, '-' | '+' | '.' | ':' | '\\'))
        || next.is_some_and(|c| matches!(c, '-' | '+' | '_' | '\\'))
        || (matches!(next, Some('.' | ':')) && after.next().is_some_and(char::is_alphanumeric))
}

fn positive_integer(value: &str) -> Option<u64> {
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|amount| *amount > 0)
        .map(|amount| amount as u64)
}

fn weekday(value: &str) -> Option<u32> {
    match value {
        "monday" | "mon" => Some(0),
        "tuesday" | "tue" | "tues" => Some(1),
        "wednesday" | "wed" => Some(2),
        "thursday" | "thu" | "thur" | "thurs" => Some(3),
        "friday" | "fri" => Some(4),
        "saturday" | "sat" => Some(5),
        "sunday" | "sun" => Some(6),
        _ => None,
    }
}

fn resolution_error() -> Error {
    Error::validation(
        "invalid_date",
        "The detected due date is outside the supported calendar range",
    )
    .with_field("due_date")
}

fn resolve_date(meaning: DateMeaning, today: NaiveDate) -> Result<NaiveDate> {
    let date = match meaning {
        DateMeaning::Days(days) => today.checked_add_days(Days::new(days)),
        DateMeaning::Weeks(weeks) => weeks
            .checked_mul(7)
            .and_then(|days| today.checked_add_days(Days::new(days))),
        DateMeaning::Months(months) => u32::try_from(months)
            .ok()
            .and_then(|months| today.checked_add_months(Months::new(months))),
        DateMeaning::Weekday(target) => {
            let distance = (target + 7 - today.weekday().num_days_from_monday()) % 7;
            today.checked_add_days(Days::new(u64::from(if distance == 0 {
                7
            } else {
                distance
            })))
        }
        DateMeaning::Resolved(date) => Some(date),
    }
    .ok_or_else(resolution_error)?;
    validate_date_value(date, "due_date")?;
    Ok(date)
}

fn relative_time<Tz: TimeZone>(
    reference: &DateTime<Tz>,
    amount: u64,
    multiplier: u64,
) -> Result<(NaiveDate, NaiveTime)> {
    let seconds = amount
        .checked_mul(multiplier)
        .and_then(|seconds| i64::try_from(seconds).ok())
        .and_then(TimeDelta::try_seconds)
        .ok_or_else(resolution_error)?;
    // Duration addition follows elapsed time, including the timezone's DST transitions.
    let mut resolved = reference
        .clone()
        .checked_add_signed(seconds)
        .ok_or_else(resolution_error)?;
    if resolved.second() > 0 || resolved.nanosecond() > 0 {
        resolved = resolved
            .checked_add_signed(TimeDelta::minutes(1))
            .ok_or_else(resolution_error)?;
    }
    let date = resolved.date_naive();
    validate_date_value(date, "due_date")?;
    let time = NaiveTime::from_hms_opt(resolved.hour(), resolved.minute(), 0)
        .ok_or_else(resolution_error)?;
    Ok((date, time))
}

fn clock_boundaries(line: &str, range: &Range<usize>) -> bool {
    let forbidden = |c: char| {
        c.is_alphanumeric()
            || matches!(c, '_' | ':' | '/' | '\\' | '-' | '+' | '@' | '#')
            || PATTERNS.words.is_match(c.encode_utf8(&mut [0; 4]))
    };
    if line[..range.start]
        .chars()
        .next_back()
        .is_some_and(|c| forbidden(c) || c == '.')
    {
        return false;
    }
    let mut after = line[range.end..].chars();
    let next = after.next();
    !next.is_some_and(forbidden)
        && !(next == Some('.') && after.next().is_some_and(char::is_alphanumeric))
}

fn clock_time(value: &str) -> Option<NaiveTime> {
    let captures = PATTERNS.valid_clock.captures(value)?;
    let (hour, minute) = if let (Some(hour), Some(minute)) = (captures.get(1), captures.get(2)) {
        (
            hour.as_str().parse::<u32>().ok()?,
            minute.as_str().parse::<u32>().ok()?,
        )
    } else {
        let hour = captures.get(3)?.as_str().parse::<u32>().ok()?;
        if !(1..=12).contains(&hour) {
            return None;
        }
        let minute = captures
            .get(4)
            .map_or(Some(0), |minute| minute.as_str().parse::<u32>().ok())?;
        let pm = captures.get(5)?.as_str().eq_ignore_ascii_case("pm");
        (hour % 12 + if pm { 12 } else { 0 }, minute)
    };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

fn first_url(token: &str) -> Option<&str> {
    for candidate in PATTERNS.urls.find_iter(token) {
        // Do not infer links embedded in an email, identifier, or another URL.
        if candidate.start() > 0
            && !token[..candidate.start()]
                .chars()
                .next_back()
                .is_some_and(|c| matches!(c, '(' | '[' | '{' | '<' | '"' | '\''))
        {
            continue;
        }
        let mut value = candidate.as_str();
        let mut balance = [0_i64; 3];
        for byte in value.bytes() {
            match byte {
                b'(' => balance[0] += 1,
                b')' => balance[0] -= 1,
                b'[' => balance[1] += 1,
                b']' => balance[1] -= 1,
                b'{' => balance[2] += 1,
                b'}' => balance[2] -= 1,
                _ => {}
            }
        }
        while let Some(last) = value.chars().next_back() {
            let closing = match last {
                ')' => Some(0),
                ']' => Some(1),
                '}' => Some(2),
                _ => None,
            };
            let unmatched = closing.is_some_and(|index| balance[index] < 0);
            if unmatched || matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'') {
                if let Some(index) = closing {
                    balance[index] += 1;
                }
                value = &value[..value.len() - last.len_utf8()];
            } else {
                break;
            }
        }
        if validate_url(Some(value)).is_ok() {
            return Some(value);
        }
    }
    None
}

fn phrase_separator(c: char) -> bool {
    !matches!(c, '/' | '\\' | '#' | '@' | '_')
        && PATTERNS.separator.is_match(c.encode_utf8(&mut [0; 4]))
}

fn expand_phrase(line: &str, mut range: Range<usize>, protected: &[Range<usize>]) -> Range<usize> {
    while let Some(c) = line[..range.start].chars().next_back() {
        let previous = range.start - c.len_utf8();
        if !phrase_separator(c) || overlaps_any(&(previous..range.start), protected) {
            break;
        }
        range.start = previous;
    }
    while let Some(c) = line[range.end..].chars().next() {
        let next = range.end + c.len_utf8();
        if !phrase_separator(c) || overlaps_any(&(range.end..next), protected) {
            break;
        }
        range.end = next;
    }
    range
}

fn remove_ranges(line: &str, mut ranges: Vec<Range<usize>>) -> String {
    ranges.sort_unstable_by_key(|range| range.start);
    let mut name = String::with_capacity(line.len());
    let mut cursor = 0;
    for range in ranges {
        if range.start > cursor {
            append_words(&mut name, &line[cursor..range.start]);
        }
        cursor = cursor.max(range.end);
    }
    append_words(&mut name, &line[cursor..]);
    name
}

fn append_words(name: &mut String, value: &str) {
    for word in value.split_whitespace() {
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn reference() -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339("2026-09-04T12:00:00+00:00").unwrap()
    }

    fn parse(value: &str) -> ParsedTaskInput {
        parse_at(value, &reference())
    }

    fn parse_at<Tz: TimeZone>(value: &str, reference: &DateTime<Tz>) -> ParsedTaskInput {
        match parse_line(value, reference).unwrap() {
            ParsedInput::Task(task) => task,
            ParsedInput::List(_) => panic!("expected a task capture"),
        }
    }

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").unwrap()
    }

    fn time(value: &str) -> NaiveTime {
        NaiveTime::parse_from_str(value, "%H:%M").unwrap()
    }

    #[test]
    fn slash_list_filters_are_literal_and_deduplicated() {
        let ParsedInput::List(filter) = parse_line(
            "  /list #personal #personal @Chörés/home @Chörés/home @tomorrow !open !done !open  ",
            &reference(),
        )
        .unwrap() else {
            panic!("expected a list command");
        };
        assert_eq!(filter.projects, ["personal"]);
        assert_eq!(filter.tags, ["Chörés/home", "tomorrow"]);
        assert_eq!(filter.states, ["open", "done"]);
        assert!(filter.include_terminal);
        assert!(matches!(
            parse_line("/list", &reference()).unwrap(),
            ParsedInput::List(_)
        ));
    }

    #[test]
    fn slash_commands_require_a_leading_whole_token_and_never_fall_back_to_tasks() {
        for (line, code) in [
            ("/", "unknown_input_command"),
            ("/delete #personal", "unknown_input_command"),
            ("/list#personal", "unknown_input_command"),
            ("/list tomorrow", "invalid_input_filter"),
            ("/list !", "invalid_input_filter"),
            ("/list !open\n", "invalid_input"),
        ] {
            assert_eq!(
                parse_line(line, &reference()).unwrap_err().code(),
                code,
                "{line:?}"
            );
        }
        for line in ["/list #", "/list @", "/list @bad,tag"] {
            assert!(parse_line(line, &reference()).is_err(), "{line}");
        }
        let task = parse("Review /list !open");
        assert_eq!(task.name, "Review /list !open");
        assert_eq!(task.due_date, None);
    }

    #[test]
    fn captures_requested_examples_without_losing_links() {
        let call = parse("Call Plumber tom 9am #personal @chores");
        assert_eq!(call.name, "Call Plumber");
        assert_eq!(call.projects, ["personal"]);
        assert_eq!(call.tags, ["chores"]);
        assert_eq!(call.due_date, Some(date("2026-09-05")));
        assert_eq!(call.due_time, Some(time("09:00")));
        let follow = parse("follow up with John about https://github.com/issues/124 #work @prs");
        assert_eq!(
            follow.name,
            "follow up with John about https://github.com/issues/124"
        );
        assert_eq!(follow.url.as_deref(), Some("https://github.com/issues/124"));
        assert_eq!(follow.projects, ["work"]);
        assert_eq!(follow.tags, ["prs"]);
        assert_eq!(follow.due_date, None);
    }

    #[test]
    fn only_last_contributing_date_and_time_are_removed() {
        let parsed = parse("Call today at 9 am tomorrow with team at 4 pm");
        assert_eq!(parsed.name, "Call today at 9 am with team");
        assert_eq!(parsed.due_date, Some(date("2026-09-05")));
        assert_eq!(parsed.due_time, Some(time("16:00")));
        let replaced = parse("Call in 2 hours tomorrow at 4 pm");
        assert_eq!(replaced.name, "Call in 2 hours");
        let date_from_relative = parse("Call in 2 hours at 4 pm");
        assert_eq!(date_from_relative.name, "Call");
        assert_eq!(date_from_relative.due_date, Some(date("2026-09-04")));
        assert_eq!(date_from_relative.due_time, Some(time("16:00")));
        let time_from_relative = parse("Call at 4 pm in 2 hours tomorrow");
        assert_eq!(time_from_relative.name, "Call at 4 pm");
        assert_eq!(time_from_relative.due_date, Some(date("2026-09-05")));
        assert_eq!(time_from_relative.due_time, Some(time("14:00")));
        let malformed_clock = parse("Call tomorrow at 25:30");
        assert_eq!(malformed_clock.name, "Call at 25:30");
        assert_eq!(malformed_clock.due_date, Some(date("2026-09-05")));
        assert_eq!(malformed_clock.due_time, None);
        let superseded = parse("Call in 9223372036854775807 months tomorrow");
        assert_eq!(superseded.name, "Call in 9223372036854775807 months");
        assert_eq!(superseded.due_date, Some(date("2026-09-05")));
    }

    #[test]
    fn calendar_phrases_use_next_occurrence_and_clamped_months() {
        for (phrase, expected) in [
            ("ToDaY", "2026-09-04"),
            ("tod", "2026-09-04"),
            ("tomorrow", "2026-09-05"),
            ("Sun", "2026-09-06"),
            ("Monday", "2026-09-07"),
            ("Tues", "2026-09-08"),
            ("Wed", "2026-09-09"),
            ("Thurs", "2026-09-10"),
            ("Friday", "2026-09-11"),
            ("Sat", "2026-09-05"),
            ("next week", "2026-09-11"),
            ("next month", "2026-10-04"),
            ("in 3 days", "2026-09-07"),
            ("in 2 weeks", "2026-09-18"),
        ] {
            assert_eq!(
                parse(&format!("Task {phrase}")).due_date,
                Some(date(expected)),
                "{phrase}"
            );
        }
        let january = DateTime::parse_from_rfc3339("2024-01-31T12:00:00+00:00").unwrap();
        assert_eq!(
            parse_at("Budget next month", &january).due_date,
            Some(date("2024-02-29"))
        );
        assert_eq!(
            parse_at("Budget in 2 months", &january).due_date,
            Some(date("2024-03-31"))
        );
        let end = DateTime::parse_from_rfc3339("9999-12-31T23:59:00+00:00").unwrap();
        for phrase in [
            "tomorrow",
            "next month",
            "in 1 minute",
            "in 9223372036854775807 weeks",
        ] {
            assert!(
                parse_line(&format!("Task {phrase}"), &end).is_err(),
                "{phrase}"
            );
        }
    }

    #[test]
    fn relative_time_rounds_up_and_uses_local_date() {
        let late = DateTime::parse_from_rfc3339("2026-09-04T23:59:45-08:00").unwrap();
        let parsed = parse_at("Check oven in 1 minute", &late);
        assert_eq!(parsed.name, "Check oven");
        assert_eq!(parsed.due_date, Some(date("2026-09-05")));
        assert_eq!(parsed.due_time, Some(time("00:01")));
        let exact = DateTime::parse_from_rfc3339("2026-09-04T23:59:00-08:00").unwrap();
        assert_eq!(
            parse_at("Check in 1 minute", &exact).due_time,
            Some(time("00:00"))
        );
        let fractional =
            DateTime::parse_from_rfc3339("2026-09-04T23:58:00.000000001-08:00").unwrap();
        let rounded = parse_at("Check in 1 minute", &fractional);
        assert_eq!(rounded.due_date, Some(date("2026-09-05")));
        assert_eq!(rounded.due_time, Some(time("00:00")));
        let clock = parse_at("Call 09:30", &late);
        assert_eq!(clock.due_date, Some(date("2026-09-04")));
    }

    #[test]
    fn clocks_cover_noon_midnight_and_reject_invalid_suffixes() {
        for (phrase, expected) in [
            ("00:00", "00:00"),
            ("23:59", "23:59"),
            ("12 am", "00:00"),
            ("12 PM", "12:00"),
            ("at 1:05 pm", "13:05"),
            ("AT 9am", "09:00"),
        ] {
            let parsed = parse(&format!("Call {phrase}"));
            assert_eq!(parsed.name, "Call");
            assert_eq!(parsed.due_time, Some(time(expected)));
        }
        for phrase in [
            "24:00",
            "09:60",
            "9:30",
            "009:30",
            "09:300",
            "09:30:00",
            "09:30:",
            "09:3 pm",
            "13:00 pm",
            "0 am",
            "12:60 am",
            "09:30 pmish",
            "3 p.m.",
            "-09:30",
            "09:30.5",
            "v09:30",
            "09:30beta",
            "9am2",
            "task_09:30",
            "in 0 hours",
            "in -2 hours",
            "in 1.5 hours",
        ] {
            let input = format!("Call {phrase}");
            let parsed = parse(&input);
            assert_eq!(parsed.name, input);
            assert_eq!(parsed.due_time, None, "{phrase}");
            assert_eq!(parsed.due_date, None, "{phrase}");
        }
    }

    #[test]
    fn unicode_metadata_and_address_boundaries_are_lossless() {
        let parsed = parse("  Café\u{2003}with Zoë, Wed., after lunch #work #work @Équipe/Été @Équipe/Été @équipe/été");
        assert_eq!(parsed.name, "Café with Zoë after lunch");
        assert_eq!(parsed.projects, ["work"]);
        assert_eq!(parsed.tags, ["Équipe/Été", "équipe/été"]);
        for input in [
            "Read https://example.test/today",
            "Read https://example.test?time=09:30",
            "Mail today@example.test",
            "Read /tmp/tomorrow",
            "Read C:\\today",
            "Read task_today",
            "Read today-task",
            "Read today.md",
            "Read étodé",
            "Read today\u{301}",
            "Call in 2 hours/path",
            "Check Mon2",
            "Visit Tomorrowland",
        ] {
            let parsed = parse(input);
            assert_eq!(parsed.name, input);
            assert_eq!(parsed.due_date, None, "{input}");
            assert_eq!(parsed.due_time, None, "{input}");
        }
        let metadata = parse("Task #today @tomorrow @9am @Team/nested");
        assert_eq!(metadata.name, "Task");
        assert_eq!(metadata.due_date, None);
        assert_eq!(metadata.due_time, None);
        for input in ["Task #Work", "Task #", "Task @", "Task @bad,tag"] {
            assert!(parse_line(input, &reference()).is_err(), "{input}");
        }
    }

    #[test]
    fn urls_keep_balanced_punctuation_and_first_valid_explicit_link() {
        let link = "HTTPS://example.com/a_(b)?q=hello%20world&sort=new#section";
        let input = format!("Read ({link}). https://second.test tomorrow");
        let parsed = parse(&input);
        assert_eq!(parsed.url.as_deref(), Some(link));
        assert_eq!(parsed.name, format!("Read ({link}). https://second.test"));
        let parsed = parse("Read https://bad.test/%ZZ https://例え.jp/道");
        assert_eq!(parsed.url.as_deref(), Some("https://例え.jp/道"));
        let parsed = parse("Read https://example.test/? at 14:30 with team!");
        assert_eq!(parsed.name, "Read https://example.test/? with team!");
        for input in [
            "Read example.com",
            "Read ftp://example.com",
            "Read prefixhttps://example.com",
            "Read user@https://example.com",
            "Read https://",
            "Read https://example.com:65536",
        ] {
            assert_eq!(parse(input).url, None, "{input}");
        }
        let parsed = parse("Read https://example.test/#work user@host @work");
        assert!(parsed.projects.is_empty());
        assert_eq!(parsed.tags, ["work"]);
        assert_eq!(parsed.name, "Read https://example.test/#work user@host");
    }

    #[test]
    fn refuses_empty_titles_and_non_single_line_input() {
        for input in [
            "",
            "   ",
            "tomorrow 9am #work @chores",
            "Task\nOther",
            "Task\rOther",
            "Task\tOther",
            "Task\0Other",
            "Task\u{2028}Other",
            "Task\u{2029}Other",
        ] {
            assert!(parse_line(input, &reference()).is_err(), "{input:?}");
        }
    }
}
