use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, ValidationIssue};

pub const SCHEMA_VERSION: u32 = 1;
pub const CONFIG_PATH: &str = ".todo/config.toml";
pub const SCHEMA_PATH: &str = ".todo/schema.json";
pub const EMBEDDED_SCHEMA: &str = include_str!("../assets/schema.json");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub tasks_directory: String,
    pub projects_directory: String,
    pub obsidian_link_prefix: String,
    pub default_state: String,
    pub states: Vec<State>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub id: String,
    pub name: String,
    pub terminal: bool,
}

impl Config {
    #[must_use]
    pub fn defaults(obsidian_link_prefix: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tasks_directory: "Tasks".to_owned(),
            projects_directory: "Projects".to_owned(),
            obsidian_link_prefix,
            default_state: "open".to_owned(),
            states: vec![
                State {
                    id: "open".to_owned(),
                    name: "Open".to_owned(),
                    terminal: false,
                },
                State {
                    id: "active".to_owned(),
                    name: "Active".to_owned(),
                    terminal: false,
                },
                State {
                    id: "blocked".to_owned(),
                    name: "Blocked".to_owned(),
                    terminal: false,
                },
                State {
                    id: "done".to_owned(),
                    name: "Done".to_owned(),
                    terminal: true,
                },
                State {
                    id: "cancelled".to_owned(),
                    name: "Cancelled".to_owned(),
                    terminal: true,
                },
            ],
        }
    }

    pub fn parse(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source).map_err(|source| {
            Error::validation("invalid_config", format!("Invalid configuration: {source}"))
                .with_path(CONFIG_PATH)
        })?;
        let issues = config.validation_issues();
        if let Some(issue) = issues.first() {
            let kind = if config.schema_version > SCHEMA_VERSION {
                Error::unsupported(
                    "unsupported_schema",
                    format!(
                        "Schema version {} is newer than supported version {SCHEMA_VERSION}",
                        config.schema_version
                    ),
                )
            } else {
                Error::validation(issue_code(issue), issue.message.clone())
            };
            return Err(if let Some(field) = &issue.field {
                kind.with_path(CONFIG_PATH).with_field(field)
            } else {
                kind.with_path(CONFIG_PATH)
            });
        }
        Ok(config)
    }

    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|source| {
            Error::validation(
                "config_serialization_failed",
                format!("Could not serialize configuration: {source}"),
            )
        })
    }

    #[must_use]
    pub fn validation_issues(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        if self.schema_version != SCHEMA_VERSION {
            let message = if self.schema_version > SCHEMA_VERSION {
                format!(
                    "Schema version {} is newer than supported version {SCHEMA_VERSION}",
                    self.schema_version
                )
            } else {
                format!(
                    "Schema version {} is not supported; expected {SCHEMA_VERSION}",
                    self.schema_version
                )
            };
            issues.push(
                ValidationIssue::error("unsupported_schema", message)
                    .at_path(CONFIG_PATH)
                    .at_field("schema_version"),
            );
        }

        for (field, value) in [
            ("tasks_directory", self.tasks_directory.as_str()),
            ("projects_directory", self.projects_directory.as_str()),
        ] {
            if let Err(message) = validate_managed_directory(value) {
                issues.push(
                    ValidationIssue::error("invalid_managed_path", message)
                        .at_path(CONFIG_PATH)
                        .at_field(field),
                );
            }
        }

        if self.tasks_directory == self.projects_directory {
            issues.push(
                ValidationIssue::error(
                    "managed_paths_not_distinct",
                    "Tasks and projects directories must be distinct",
                )
                .at_path(CONFIG_PATH),
            );
        }

        if let Err(message) = validate_link_prefix(&self.obsidian_link_prefix) {
            issues.push(
                ValidationIssue::error("invalid_link_prefix", message)
                    .at_path(CONFIG_PATH)
                    .at_field("obsidian_link_prefix"),
            );
        }

        let mut ids = HashSet::with_capacity(self.states.len());
        let mut has_nonterminal = false;
        for state in &self.states {
            if !is_state_id(&state.id) {
                issues.push(
                    ValidationIssue::error(
                        "invalid_state_id",
                        format!("Invalid state ID {:?}", state.id),
                    )
                    .at_path(CONFIG_PATH)
                    .at_field("states.id"),
                );
            }
            if !ids.insert(state.id.as_str()) {
                issues.push(
                    ValidationIssue::error(
                        "duplicate_state_id",
                        format!("State ID {:?} appears more than once", state.id),
                    )
                    .at_path(CONFIG_PATH)
                    .at_field("states.id"),
                );
            }
            if state.name.trim().is_empty() {
                issues.push(
                    ValidationIssue::error("invalid_state_name", "State names cannot be empty")
                        .at_path(CONFIG_PATH)
                        .at_field("states.name"),
                );
            }
            has_nonterminal |= !state.terminal;
        }

        if !has_nonterminal {
            issues.push(
                ValidationIssue::error(
                    "missing_nonterminal_state",
                    "At least one state must be nonterminal",
                )
                .at_path(CONFIG_PATH)
                .at_field("states"),
            );
        }

        match self.state(&self.default_state) {
            None => issues.push(
                ValidationIssue::error(
                    "invalid_default_state",
                    format!(
                        "Default state {:?} does not reference a configured state",
                        self.default_state
                    ),
                )
                .at_path(CONFIG_PATH)
                .at_field("default_state"),
            ),
            Some(state) if state.terminal => issues.push(
                ValidationIssue::error(
                    "terminal_default_state",
                    "Default state must be nonterminal",
                )
                .at_path(CONFIG_PATH)
                .at_field("default_state"),
            ),
            Some(_) => {}
        }

        issues
    }

    #[must_use]
    pub fn state(&self, id: &str) -> Option<&State> {
        self.states.iter().find(|state| state.id == id)
    }

    #[must_use]
    pub fn state_order(&self, id: &str) -> Option<usize> {
        self.states.iter().position(|state| state.id == id)
    }

    pub fn tasks_path(&self) -> Result<PathBuf> {
        managed_path(&self.tasks_directory)
    }

    pub fn projects_path(&self) -> Result<PathBuf> {
        managed_path(&self.projects_directory)
    }

    #[must_use]
    pub fn project_link(&self, slug: &str) -> String {
        let directory = self.projects_directory.replace('\\', "/");
        if self.obsidian_link_prefix.is_empty() {
            format!("[[{directory}/{slug}]]")
        } else {
            format!("[[{}/{directory}/{slug}]]", self.obsidian_link_prefix)
        }
    }

    #[must_use]
    pub fn project_link_target_prefix(&self) -> String {
        let directory = self.projects_directory.replace('\\', "/");
        if self.obsidian_link_prefix.is_empty() {
            format!("{directory}/")
        } else {
            format!("{}/{directory}/", self.obsidian_link_prefix)
        }
    }
}

fn issue_code(issue: &ValidationIssue) -> &'static str {
    match issue.code.as_str() {
        "unsupported_schema" => "unsupported_schema",
        "invalid_managed_path" => "invalid_managed_path",
        "managed_paths_not_distinct" => "managed_paths_not_distinct",
        "invalid_link_prefix" => "invalid_link_prefix",
        "invalid_state_id" => "invalid_state_id",
        "duplicate_state_id" => "duplicate_state_id",
        "invalid_state_name" => "invalid_state_name",
        "missing_nonterminal_state" => "missing_nonterminal_state",
        "invalid_default_state" => "invalid_default_state",
        "terminal_default_state" => "terminal_default_state",
        _ => "invalid_config",
    }
}

pub fn managed_path(value: &str) -> Result<PathBuf> {
    validate_managed_directory(value)
        .map_err(|message| Error::validation("invalid_managed_path", message))?;
    Ok(PathBuf::from(value))
}

pub fn validate_managed_directory(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Err("Managed directory cannot be empty".to_owned());
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(format!("Managed directory {value:?} must be relative"));
    }
    let mut components = path.components();
    let Some(first) = components.next() else {
        return Err("Managed directory cannot be empty".to_owned());
    };
    let Component::Normal(first) = first else {
        return Err(format!(
            "Managed directory {value:?} must be normalized without '.' or '..'"
        ));
    };
    if first == ".todo" {
        return Err("Managed directories must be distinct from .todo".to_owned());
    }
    if components.any(|component| !matches!(component, Component::Normal(_))) {
        return Err(format!(
            "Managed directory {value:?} must be normalized without '.' or '..'"
        ));
    }
    Ok(())
}

pub fn validate_link_prefix(value: &str) -> std::result::Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    if value.starts_with('/') || value.ends_with('/') || value.contains('\\') {
        return Err("Obsidian link prefix must be a relative '/'-separated path".to_owned());
    }
    if value.split('/').any(|part| {
        part.is_empty()
            || matches!(part, "." | "..")
            || part.contains("[[")
            || part.contains("]]")
            || part.contains(['|', '#', '^'])
    }) {
        return Err("Obsidian link prefix contains an invalid path component".to_owned());
    }
    Ok(())
}

#[must_use]
pub fn is_state_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_required_state_order() {
        let config = Config::defaults("Todo".to_owned());
        assert_eq!(
            config
                .states
                .iter()
                .map(|state| state.id.as_str())
                .collect::<Vec<_>>(),
            ["open", "active", "blocked", "done", "cancelled"]
        );
        assert!(config.validation_issues().is_empty());
    }

    #[test]
    fn state_ids_require_lowercase_slug() {
        for valid in ["open", "state-2", "state_2", "0"] {
            assert!(is_state_id(valid), "{valid}");
        }
        for invalid in ["", "Open", "-open", "_open", "open space", "å"] {
            assert!(!is_state_id(invalid), "{invalid}");
        }
    }

    #[test]
    fn managed_paths_reject_escape_and_metadata_directory() {
        for invalid in [
            "",
            ".",
            "../Tasks",
            "Tasks/../Elsewhere",
            "/Tasks",
            ".todo",
            ".todo/tasks",
        ] {
            assert!(validate_managed_directory(invalid).is_err(), "{invalid}");
        }
        assert!(validate_managed_directory("Areas/Todos").is_ok());
    }

    #[test]
    fn unknown_config_keys_are_rejected() {
        let mut source = Config::defaults("Todo".to_owned())
            .to_toml()
            .expect("serialize");
        source.push_str("typo = true\n");
        let error = Config::parse(&source).expect_err("unknown key must fail");
        assert_eq!(error.code(), "invalid_config");
    }

    #[test]
    fn project_links_use_forward_slashes() {
        let mut config = Config::defaults("Area/Todo".to_owned());
        config.projects_directory = "Records/Projects".to_owned();
        assert_eq!(
            config.project_link("work"),
            "[[Area/Todo/Records/Projects/work]]"
        );
    }
}
