use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate, NaiveTime, Timelike};
use serde_yaml_ng::Mapping;
use ulid::Ulid;

use crate::config::Config;
use crate::error::{Error, Result, ValidationIssue};
use crate::recurrence::{parse_date, validate_date_value, RecurrenceMode, RecurrenceRule};

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub state: String,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub parent: Option<String>,
    pub url: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub due_time: Option<NaiveTime>,
    pub recurrence: Option<RecurrenceRule>,
    pub recurrence_from: Option<RecurrenceMode>,
    pub last_completed_date: Option<NaiveDate>,
    pub body: String,
    pub extra_properties: Mapping,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRecord {
    pub id: String,
    pub path: PathBuf,
    pub parent: Option<String>,
}

/// Analyze physical identities without choosing edges from duplicate records.
#[must_use]
pub fn analyze_parents(records: &[ParentRecord]) -> Vec<ValidationIssue> {
    let ids = records
        .iter()
        .map(|record| record.id.to_ascii_uppercase())
        .collect::<Vec<_>>();
    let mut identities = HashMap::<&str, usize>::with_capacity(records.len());
    let mut ambiguous = vec![false; records.len()];
    for (index, id) in ids.iter().enumerate() {
        if let Some(&first) = identities.get(id.as_str()) {
            ambiguous[first] = true;
            ambiguous[index] = true;
            if records[index].path < records[first].path {
                identities.insert(id, index);
            }
        } else {
            identities.insert(id, index);
        }
    }

    let mut issues = Vec::new();
    let mut edges = vec![None; records.len()];
    for (index, record) in records.iter().enumerate() {
        if ambiguous[index] {
            if identities[ids[index].as_str()] == index {
                issues.push((
                    index,
                    ValidationIssue::error(
                        "duplicate_task_id",
                        format!("Task ID {} appears more than once", ids[index]),
                    )
                    .at_path(&record.path),
                ));
            }
            continue;
        }
        let Some(parent) = &record.parent else {
            continue;
        };
        let parent = parent.to_ascii_uppercase();
        let fault = match identities.get(parent.as_str()) {
            None => Some((
                "missing_parent_reference",
                format!("Parent task {parent} does not exist"),
            )),
            Some(&target) if ambiguous[target] => None,
            Some(&target) if target == index => Some((
                "self_parent_reference",
                format!("Task {} cannot parent itself", ids[index]),
            )),
            Some(&target) => {
                edges[index] = Some(target);
                None
            }
        };
        if let Some((code, message)) = fault {
            issues.push((
                index,
                ValidationIssue::error(code, message)
                    .at_path(&record.path)
                    .at_field("parent"),
            ));
        }
    }

    // Each node is visited once. The current walk is explicit, so depth cannot
    // exhaust the call stack and entering descendants are not cycle members.
    let mut state = vec![0_u8; records.len()];
    let mut positions = vec![0; records.len()];
    let mut walk = Vec::<usize>::new();
    for start in 0..records.len() {
        if state[start] != 0 {
            continue;
        }
        walk.clear();
        let mut current = Some(start);
        while let Some(index) = current {
            if state[index] == 2 {
                break;
            }
            if state[index] == 1 {
                for &member in &walk[positions[index]..] {
                    issues.push((
                        member,
                        ValidationIssue::error(
                            "parent_cycle",
                            format!("Task {} participates in a parent cycle", ids[member]),
                        )
                        .at_path(&records[member].path)
                        .at_field("parent"),
                    ));
                }
                break;
            }
            state[index] = 1;
            positions[index] = walk.len();
            walk.push(index);
            current = edges[index];
        }
        for &index in &walk {
            state[index] = 2;
        }
    }
    issues.sort_by(|(left, left_issue), (right, right_issue)| {
        ids[*left]
            .cmp(&ids[*right])
            .then_with(|| left_issue.code.cmp(&right_issue.code))
    });
    issues.into_iter().map(|(_, issue)| issue).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub slug: String,
    pub path: PathBuf,
    pub name: String,
    pub body: String,
    pub extra_properties: Mapping,
}

pub trait Clock: Send + Sync {
    fn today(&self) -> NaiveDate;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn today(&self) -> NaiveDate {
        Local::now().date_naive()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    today: NaiveDate,
}

impl FixedClock {
    #[must_use]
    pub const fn new(today: NaiveDate) -> Self {
        Self { today }
    }
}

impl Clock for FixedClock {
    fn today(&self) -> NaiveDate {
        self.today
    }
}

impl Task {
    pub fn validate(&self, config: &Config) -> Result<()> {
        validate_task_id(&self.id)?;
        if let Some(parent) = &self.parent {
            if config.schema_version != 2 {
                return Err(Error::unsupported(
                    "unsupported_schema",
                    "Typed parent relationships require schema version 2",
                )
                .with_field("parent"));
            }
            normalize_parent_id(parent)?;
        }
        validate_url(self.url.as_deref())?;
        validate_name(&self.name, "name")?;
        if config.state(&self.state).is_none() {
            return Err(Error::validation(
                "unknown_state",
                format!("Task state {:?} is not configured", self.state),
            )
            .with_field("state"));
        }
        validate_unique_projects(&self.projects)?;
        for project in &self.projects {
            validate_project_slug(project)?;
        }
        validate_tags(&self.tags)?;
        if let Some(due_date) = self.due_date {
            validate_date_value(due_date, "due_date")?;
        }
        if let Some(due_time) = self.due_time {
            validate_time_value(due_time)?;
            if self.due_date.is_none() {
                return Err(Error::validation(
                    "due_time_requires_due_date",
                    "A task with due_time must have due_date",
                )
                .with_field("due_time"));
            }
        }
        if let Some(last_completed_date) = self.last_completed_date {
            validate_date_value(last_completed_date, "last_completed_date")?;
        }
        match (&self.recurrence, self.recurrence_from) {
            (Some(rule), Some(_)) => {
                let due = self.due_date.ok_or_else(|| {
                    Error::validation(
                        "recurrence_requires_due_date",
                        "A recurring task must have due_date",
                    )
                    .with_field("due_date")
                })?;
                if !rule.matches_date(due) {
                    return Err(Error::validation(
                        "due_date_not_occurrence",
                        "due_date does not match the recurrence selections",
                    )
                    .with_field("due_date"));
                }
            }
            (Some(_), None) => {
                return Err(Error::validation(
                    "recurrence_requires_mode",
                    "A recurring task must have recurrence_from",
                )
                .with_field("recurrence_from"));
            }
            (None, Some(_)) => {
                return Err(Error::validation(
                    "mode_requires_recurrence",
                    "recurrence_from is invalid without recurrence",
                )
                .with_field("recurrence_from"));
            }
            (None, None) if self.last_completed_date.is_some() => {
                return Err(Error::validation(
                    "completion_date_requires_recurrence",
                    "last_completed_date is invalid without recurrence",
                )
                .with_field("last_completed_date"));
            }
            (None, None) => {}
        }
        Ok(())
    }

    #[must_use]
    pub fn terminal(&self, config: &Config) -> bool {
        config
            .state(&self.state)
            .is_some_and(|state| state.terminal)
    }
}

/// Parse an exact minute-granularity civil time from 00:00 through 23:59.
pub fn parse_time(value: &str) -> Result<NaiveTime> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return Err(invalid_time());
    }
    let hour = u32::from(bytes[0] - b'0') * 10 + u32::from(bytes[1] - b'0');
    let minute = u32::from(bytes[3] - b'0') * 10 + u32::from(bytes[4] - b'0');
    NaiveTime::from_hms_opt(hour, minute, 0).ok_or_else(invalid_time)
}

fn validate_time_value(value: NaiveTime) -> Result<()> {
    if value.second() != 0 || value.nanosecond() != 0 {
        return Err(invalid_time());
    }
    Ok(())
}

fn invalid_time() -> Error {
    Error::validation(
        "invalid_due_time",
        "due_time must be an HH:MM time from 00:00 through 23:59",
    )
    .with_field("due_time")
}

/// Validate a web link without normalizing it or contacting its host.
pub fn validate_url(value: Option<&str>) -> Result<()> {
    let Some(value) = value else {
        return Ok(());
    };
    let invalid = || {
        Error::validation(
            "invalid_url",
            "URL must be an absolute HTTP or HTTPS URL with a valid host",
        )
        .with_field("url")
    };
    if value.chars().any(|character| {
        character.is_whitespace()
            || character.is_control()
            || matches!(
                character,
                '\\' | '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`'
            )
    }) {
        return Err(invalid());
    }
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (bytes
                .get(index + 1)
                .is_none_or(|byte| !byte.is_ascii_hexdigit())
                || bytes
                    .get(index + 2)
                    .is_none_or(|byte| !byte.is_ascii_hexdigit()))
        {
            return Err(invalid());
        }
    }
    let (scheme, remainder) = value.split_once("://").ok_or_else(invalid)?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(invalid());
    }
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            if userinfo.contains(['@', '[', ']']) {
                return Err(invalid());
            }
            host_port
        }
        None => authority,
    };
    let port = if let Some(ipv6) = host_port.strip_prefix('[') {
        let (host, suffix) = ipv6.split_once(']').ok_or_else(invalid)?;
        host.parse::<std::net::Ipv6Addr>().map_err(|_| invalid())?;
        if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or_else(invalid)?)
        }
    } else {
        let (host, port) = match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        };
        if host.is_empty()
            || host.contains(['[', ']'])
            || !host.chars().all(|character| {
                !character.is_ascii()
                    || character.is_ascii_alphanumeric()
                    || "-._~%!$&'()*+,;=".contains(character)
            })
        {
            return Err(invalid());
        }
        if host.contains('%') {
            let host_bytes = host.as_bytes();
            let mut decoded = Vec::with_capacity(host_bytes.len());
            let mut index = 0;
            while index < host_bytes.len() {
                if host_bytes[index] == b'%' {
                    // Escape shape was checked above.
                    let high = (host_bytes[index + 1] as char)
                        .to_digit(16)
                        .unwrap_or_default();
                    let low = (host_bytes[index + 2] as char)
                        .to_digit(16)
                        .unwrap_or_default();
                    decoded.push((high * 16 + low) as u8);
                    index += 3;
                } else {
                    decoded.push(host_bytes[index]);
                    index += 1;
                }
            }
            let decoded = std::str::from_utf8(&decoded).map_err(|_| invalid())?;
            if decoded.chars().any(|character| {
                character.is_whitespace()
                    || character.is_control()
                    || "/\\?#@:[]<>\"{}|^`".contains(character)
            }) {
                return Err(invalid());
            }
        }
        port
    };
    if port.is_some_and(|port| {
        port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().is_err()
    }) {
        return Err(invalid());
    }
    Ok(())
}

pub fn validate_name(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(Error::validation(
            "invalid_name",
            format!("{field} must contain a non-whitespace character"),
        )
        .with_field(field));
    }
    if value.contains(['\r', '\n']) {
        return Err(
            Error::validation("invalid_name", format!("{field} must be a single line"))
                .with_field(field),
        );
    }
    Ok(())
}

pub fn validate_tag(value: &str) -> Result<()> {
    if value.trim().is_empty()
        || value.starts_with('#')
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || value.contains([',', '[', ']', '{', '}'])
    {
        return Err(
            Error::validation("invalid_tag", format!("Invalid Obsidian tag {value:?}"))
                .with_field("tags"),
        );
    }
    Ok(())
}

pub fn validate_tags(values: &[String]) -> Result<()> {
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        validate_tag(value)?;
        if !seen.insert(value.as_str()) {
            return Err(Error::validation(
                "duplicate_tag",
                format!("Tag {value:?} appears more than once"),
            )
            .with_field("tags"));
        }
    }
    Ok(())
}

pub fn validate_project_slug(value: &str) -> Result<()> {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return Err(invalid_slug(value));
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit())
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(invalid_slug(value));
    }
    Ok(())
}

fn invalid_slug(value: &str) -> Error {
    Error::validation(
        "invalid_project_slug",
        format!("Project slug {value:?} must match [a-z0-9][a-z0-9-]*"),
    )
    .with_field("slug")
}

pub fn validate_unique_projects(values: &[String]) -> Result<()> {
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        if !seen.insert(value.as_str()) {
            return Err(Error::validation(
                "duplicate_project",
                format!("Project {value:?} appears more than once"),
            )
            .with_field("projects"));
        }
    }
    Ok(())
}

pub fn validate_task_id(value: &str) -> Result<()> {
    if value.len() != 26
        || value.as_bytes()[0].to_ascii_uppercase() > b'7'
        || !value.bytes().all(is_ulid_character)
        || Ulid::from_string(&value.to_ascii_uppercase()).is_err()
    {
        return Err(Error::validation(
            "invalid_task_id",
            format!("Task ID {value:?} is not a valid ULID"),
        ));
    }
    Ok(())
}

pub fn normalize_parent_id(value: &str) -> Result<String> {
    validate_task_id(value).map_err(|_| {
        Error::validation(
            "invalid_parent_id",
            format!("Parent {value:?} must be a full valid ULID"),
        )
        .with_field("parent")
    })?;
    Ok(value.to_ascii_uppercase())
}

#[must_use]
pub fn is_ulid_character(byte: u8) -> bool {
    matches!(
        byte.to_ascii_uppercase(),
        b'0'..=b'9'
            | b'A'..=b'H'
            | b'J'
            | b'K'
            | b'M'
            | b'N'
            | b'P'..=b'T'
            | b'V'..=b'Z'
    )
}

pub fn validate_id_prefix(value: &str) -> Result<String> {
    if value.len() > 26
        || value.len() < 6
        || value.as_bytes()[0].to_ascii_uppercase() > b'7'
        || !value.bytes().all(is_ulid_character)
        || (value.len() == 26 && Ulid::from_string(&value.to_ascii_uppercase()).is_err())
    {
        return Err(Error::usage(
            "invalid_task_id_prefix",
            "Task ID prefixes must contain 6 to 26 valid Crockford Base32 characters",
        ));
    }
    Ok(value.to_ascii_uppercase())
}

pub fn normalize_body(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    while normalized.ends_with('\n') {
        normalized.pop();
    }
    normalized.push('\n');
    normalized
}

pub fn date_from_yaml(value: &serde_yaml_ng::Value, field: &str) -> Result<NaiveDate> {
    let value = value.as_str().ok_or_else(|| {
        Error::validation(
            "invalid_property_type",
            format!("{field} must be a YYYY-MM-DD date scalar"),
        )
        .with_field(field)
    })?;
    parse_date(value, field)
}

pub fn relative_path_string(path: &Path) -> String {
    let mut output = String::new();
    for component in path.components() {
        if !output.is_empty() {
            output.push('/');
        }
        output.push_str(&component.as_os_str().to_string_lossy());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_nonempty_single_lines() {
        for invalid in ["", "  \t", "one\ntwo", "one\rtwo"] {
            assert!(validate_name(invalid, "name").is_err(), "{invalid:?}");
        }
        assert!(validate_name("  meaningful  ", "name").is_ok());
        assert!(validate_name("Unicode 任务", "name").is_ok());
    }

    #[test]
    fn validates_tags_without_renaming_unicode_or_case() {
        for valid in ["Finance", "nested/review", "hash#inside", "日本語"] {
            assert!(validate_tag(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "#tag", "two words", "comma,tag", "[[link]]", "line\n"] {
            assert!(validate_tag(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn validates_slugs_and_ulids() {
        for slug in ["a", "project-2", "0"] {
            assert!(validate_project_slug(slug).is_ok(), "{slug}");
        }
        for slug in ["", "Project", "-project", "with_underscore", "é"] {
            assert!(validate_project_slug(slug).is_err(), "{slug}");
        }
        assert!(validate_task_id("01K4B0ZSBZZV25T1K0D3TA8JHR").is_ok());
        assert!(validate_task_id("01k4b0zsbzzv25t1k0d3ta8jhr").is_ok());
        assert!(validate_task_id("81K4B0ZSBZZV25T1K0D3TA8JHR").is_err());
        assert!(validate_task_id("01K4B0ZSBZZV25T1K0D3TA8JRI").is_err());
    }

    #[test]
    fn prefixes_are_case_insensitive_and_at_least_six_characters() {
        assert_eq!(validate_id_prefix("01k4b0").expect("prefix"), "01K4B0");
        assert!(validate_id_prefix("01K4B").is_err());
        assert!(validate_id_prefix("01K4BI").is_err());
    }

    #[test]
    fn supplied_bodies_get_lf_and_one_trailing_newline() {
        assert_eq!(normalize_body("one\r\ntwo\r\n\r\n"), "one\ntwo\n");
        assert_eq!(normalize_body(""), "");
    }

    #[test]
    fn parent_arguments_require_full_unpadded_ulids() {
        assert_eq!(
            normalize_parent_id("01k4b0zsbzzv25t1k0d3ta8jhr").expect("parent"),
            "01K4B0ZSBZZV25T1K0D3TA8JHR"
        );
        for invalid in [
            "",
            "none",
            "01K4B0",
            " 01K4B0ZSBZZV25T1K0D3TA8JHR",
            "01K4B0ZSBZZV25T1K0D3TA8JHR ",
            "[[01K4B0ZSBZZV25T1K0D3TA8JHR]]",
            "Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md",
            "81K4B0ZSBZZV25T1K0D3TA8JHR",
            "01K4B0ZSBZZV25T1K0D3TA8JRI",
        ] {
            let error = normalize_parent_id(invalid).expect_err(invalid);
            assert_eq!(error.code(), "invalid_parent_id");
            assert_eq!(error.field(), Some("parent"));
            assert_eq!(error.exit_code(), 5);
        }
    }

    #[test]
    fn shared_parent_graph_conformance() {
        #[derive(serde::Deserialize)]
        struct Corpus {
            graph_cases: Vec<GraphCase>,
        }
        #[derive(serde::Deserialize)]
        struct GraphCase {
            name: String,
            tasks: Vec<GraphTask>,
            expected_issues: Vec<GraphIssue>,
        }
        #[derive(serde::Deserialize)]
        struct GraphTask {
            id: String,
            parent: Option<String>,
        }
        #[derive(Debug, PartialEq, Eq, serde::Deserialize)]
        struct GraphIssue {
            code: String,
            task_id: String,
        }
        let corpus: Corpus =
            serde_json::from_str(include_str!("../tests/fixtures/subtasks.json")).expect("corpus");
        for case in corpus.graph_cases {
            let mut records = case
                .tasks
                .into_iter()
                .map(|task| ParentRecord {
                    path: PathBuf::from(format!("Tasks/{}.md", task.id.to_ascii_uppercase())),
                    id: task.id,
                    parent: task.parent,
                })
                .collect::<Vec<_>>();
            for _ in 0..2 {
                let actual = analyze_parents(&records)
                    .into_iter()
                    .map(|issue| {
                        if issue.code != "duplicate_task_id" {
                            assert_eq!(issue.field.as_deref(), Some("parent"), "{}", case.name);
                        }
                        GraphIssue {
                            task_id: issue
                                .path
                                .expect("child path")
                                .file_stem()
                                .expect("filename")
                                .to_str()
                                .expect("ULID")
                                .to_owned(),
                            code: issue.code,
                        }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(actual, case.expected_issues, "{}", case.name);
                records.reverse();
                for record in &mut records {
                    record.id.make_ascii_lowercase();
                    if let Some(parent) = &mut record.parent {
                        parent.make_ascii_lowercase();
                    }
                }
            }
        }
    }

    #[test]
    fn deep_parent_walk_reports_only_cycle_members_and_detach_repairs_it() {
        const DEPTH: usize = 30_000;
        let mut records = (0..DEPTH)
            .map(|index| {
                let id = Ulid::from(index as u128).to_string();
                ParentRecord {
                    path: PathBuf::from(format!("Tasks/{id}.md")),
                    id,
                    parent: (index + 1 < DEPTH)
                        .then(|| Ulid::from((index + 1) as u128).to_string()),
                }
            })
            .collect::<Vec<_>>();
        assert!(analyze_parents(&records).is_empty());
        records[DEPTH - 1].parent = Some(records[DEPTH / 2].id.clone());
        let issues = analyze_parents(&records);
        assert_eq!(issues.len(), DEPTH / 2);
        for (issue, member) in issues.iter().zip(&records[DEPTH / 2..]) {
            assert_eq!(issue.code, "parent_cycle");
            assert_eq!(issue.path.as_ref(), Some(&member.path));
        }
        records[DEPTH / 2].parent = None;
        assert!(analyze_parents(&records).is_empty());
    }

    #[test]
    fn three_duplicate_paths_never_select_an_arbitrary_parent_edge() {
        let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let child = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
        let missing = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
        let mut records = vec![
            ParentRecord {
                id: id.to_owned(),
                path: "Tasks/z.md".into(),
                parent: Some(id.to_owned()),
            },
            ParentRecord {
                id: id.to_ascii_lowercase(),
                path: "Tasks/a.md".into(),
                parent: Some(missing.to_owned()),
            },
            ParentRecord {
                id: id.to_owned(),
                path: "Tasks/b.md".into(),
                parent: Some(child.to_owned()),
            },
            ParentRecord {
                id: child.to_owned(),
                path: "Tasks/child.md".into(),
                parent: Some(id.to_owned()),
            },
        ];
        for _ in 0..2 {
            let actual = analyze_parents(&records)
                .into_iter()
                .map(|issue| (issue.code, issue.path))
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                vec![(
                    "duplicate_task_id".to_owned(),
                    Some(PathBuf::from("Tasks/a.md")),
                )]
            );
            records.reverse();
        }
    }
}
