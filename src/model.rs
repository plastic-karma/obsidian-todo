use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate};
use serde_yaml_ng::Mapping;
use ulid::Ulid;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::recurrence::{parse_date, validate_date_value, RecurrenceMode, RecurrenceRule};

#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
    pub state: String,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub due_date: Option<NaiveDate>,
    pub recurrence: Option<RecurrenceRule>,
    pub recurrence_from: Option<RecurrenceMode>,
    pub last_completed_date: Option<NaiveDate>,
    pub body: String,
    pub extra_properties: Mapping,
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
}
