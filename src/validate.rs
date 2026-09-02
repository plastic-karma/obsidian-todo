use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use walkdir::WalkDir;

use crate::config::{validate_managed_directory, Config, CONFIG_PATH, SCHEMA_PATH, SCHEMA_VERSION};
use crate::error::{Error, IssueSeverity, Result, ValidationIssue};
use crate::frontmatter::{parse_project, parse_task, MAX_RECORD_BYTES};
use crate::model::{validate_project_slug, validate_task_id};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: usize,
    pub warnings: usize,
    pub tasks: usize,
    pub projects: usize,
    pub issues: Vec<ValidationIssue>,
}

impl ValidationReport {
    #[must_use]
    pub fn from_issues(mut issues: Vec<ValidationIssue>, tasks: usize, projects: usize) -> Self {
        issues.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.field.cmp(&right.field))
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.column.cmp(&right.column))
                .then_with(|| left.code.cmp(&right.code))
        });
        let errors = issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Error)
            .count();
        let warnings = issues.len().saturating_sub(errors);
        Self {
            valid: errors == 0,
            errors,
            warnings,
            tasks,
            projects,
            issues,
        }
    }

    pub fn into_result(self) -> Result<Self> {
        if self.valid {
            Ok(self)
        } else {
            Err(Error::from_issues(self.issues))
        }
    }
}

pub fn validate_store(root: impl AsRef<Path>) -> ValidationReport {
    let supplied_root = root.as_ref();
    let mut issues = Vec::new();
    let root = match validate_root(supplied_root) {
        Ok(root) => root,
        Err(issue) => {
            issues.push(issue);
            return ValidationReport::from_issues(issues, 0, 0);
        }
    };
    validate_metadata_directory(&root, &mut issues);
    let config = load_config_for_validation(&root, &mut issues);
    validate_schema(&root, &mut issues);
    let Some(config) = config else {
        return ValidationReport::from_issues(issues, 0, 0);
    };

    validate_managed_path_on_disk(
        &root,
        &config.tasks_directory,
        "tasks_directory",
        &mut issues,
    );
    validate_managed_path_on_disk(
        &root,
        &config.projects_directory,
        "projects_directory",
        &mut issues,
    );

    let (project_slugs, project_count) = validate_projects(&root, &config, &mut issues);
    let task_count = validate_tasks(&root, &config, &project_slugs, &mut issues);
    ValidationReport::from_issues(issues, task_count, project_count)
}

fn validate_root(root: &Path) -> std::result::Result<PathBuf, ValidationIssue> {
    let metadata = fs::symlink_metadata(root).map_err(|source| {
        ValidationIssue::error(
            "store_not_found",
            format!("Could not inspect the store root: {source}"),
        )
        .at_path(".")
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ValidationIssue::error(
            "invalid_store_root",
            "Store root must be a real directory",
        )
        .at_path("."));
    }
    fs::canonicalize(root).map_err(|source| {
        ValidationIssue::error(
            "invalid_store_root",
            format!("Could not resolve the store root: {source}"),
        )
        .at_path(".")
    })
}

fn validate_metadata_directory(root: &Path, issues: &mut Vec<ValidationIssue>) {
    let path = root.join(".todo");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => issues.push(
            ValidationIssue::error(
                "invalid_metadata_directory",
                ".todo must be a real directory",
            )
            .at_path(".todo"),
        ),
        Err(source) => issues.push(
            ValidationIssue::error(
                "metadata_directory_missing",
                format!("Could not read .todo: {source}"),
            )
            .at_path(".todo"),
        ),
    }
}

fn load_config_for_validation(root: &Path, issues: &mut Vec<ValidationIssue>) -> Option<Config> {
    let path = root.join(CONFIG_PATH);
    let bytes = match read_regular_file(&path, Path::new(CONFIG_PATH), MAX_CONFIG_BYTES) {
        Ok(bytes) => bytes,
        Err(issue) => {
            issues.push(issue);
            return None;
        }
    };
    let source = match String::from_utf8(bytes) {
        Ok(source) => source,
        Err(_) => {
            issues.push(
                ValidationIssue::error("invalid_config_utf8", "Configuration must be UTF-8")
                    .at_path(CONFIG_PATH),
            );
            return None;
        }
    };
    let mut value: toml::Value = match toml::from_str(&source) {
        Ok(value) => value,
        Err(source) => {
            issues.push(
                ValidationIssue::error(
                    "invalid_config",
                    format!("Invalid configuration syntax: {source}"),
                )
                .at_path(CONFIG_PATH),
            );
            return None;
        }
    };
    let Some(table) = value.as_table_mut() else {
        issues.push(
            ValidationIssue::error("invalid_config", "Configuration root must be a table")
                .at_path(CONFIG_PATH),
        );
        return None;
    };
    const KEYS: [&str; 6] = [
        "schema_version",
        "tasks_directory",
        "projects_directory",
        "obsidian_link_prefix",
        "default_state",
        "states",
    ];
    let unknown = table
        .keys()
        .filter(|key| !KEYS.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for key in unknown {
        table.remove(&key);
        issues.push(
            ValidationIssue::error(
                "unknown_config_field",
                format!("Unknown configuration field {key:?}"),
            )
            .at_path(CONFIG_PATH)
            .at_field(key),
        );
    }
    if let Some(states) = table.get_mut("states").and_then(toml::Value::as_array_mut) {
        for state in states {
            if let Some(state) = state.as_table_mut() {
                let unknown = state
                    .keys()
                    .filter(|key| !["id", "name", "terminal"].contains(&key.as_str()))
                    .cloned()
                    .collect::<Vec<_>>();
                for key in unknown {
                    state.remove(&key);
                    issues.push(
                        ValidationIssue::error(
                            "unknown_config_field",
                            format!("Unknown state configuration field {key:?}"),
                        )
                        .at_path(CONFIG_PATH)
                        .at_field(format!("states.{key}")),
                    );
                }
            }
        }
    }
    let config: Config = match value.try_into() {
        Ok(config) => config,
        Err(source) => {
            issues.push(
                ValidationIssue::error(
                    "invalid_config",
                    format!("Invalid configuration values: {source}"),
                )
                .at_path(CONFIG_PATH),
            );
            return None;
        }
    };
    issues.extend(config.validation_issues());
    Some(config)
}

fn validate_schema(root: &Path, issues: &mut Vec<ValidationIssue>) {
    let path = root.join(SCHEMA_PATH);
    let bytes = match read_regular_file(&path, Path::new(SCHEMA_PATH), MAX_CONFIG_BYTES) {
        Ok(bytes) => bytes,
        Err(issue) => {
            issues.push(issue);
            return;
        }
    };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(schema)
            if schema
                .get("x-obsidian-todo-schema-version")
                .and_then(serde_json::Value::as_u64)
                == Some(u64::from(SCHEMA_VERSION)) => {}
        Ok(_) => issues.push(
            ValidationIssue::error(
                "schema_version_mismatch",
                "schema.json is not compatible with schema version 1",
            )
            .at_path(SCHEMA_PATH),
        ),
        Err(source) => issues.push(
            ValidationIssue::error(
                "invalid_schema_file",
                format!("schema.json is invalid JSON: {source}"),
            )
            .at_path(SCHEMA_PATH),
        ),
    }
}

fn validate_managed_path_on_disk(
    root: &Path,
    configured: &str,
    field: &str,
    issues: &mut Vec<ValidationIssue>,
) {
    if validate_managed_directory(configured).is_err() {
        return;
    }
    let relative = Path::new(configured);
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return;
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                issues.push(
                    ValidationIssue::error(
                        "managed_path_symlink",
                        "Managed paths must not contain symlinks",
                    )
                    .at_path(relative)
                    .at_field(field),
                );
                return;
            }
            Ok(metadata) if !metadata.is_dir() => {
                issues.push(
                    ValidationIssue::error(
                        "managed_path_not_directory",
                        "Configured managed path is not a directory",
                    )
                    .at_path(relative)
                    .at_field(field),
                );
                return;
            }
            Ok(_) => {}
            Err(source) => {
                issues.push(
                    ValidationIssue::error(
                        "managed_directory_missing",
                        format!("Configured managed directory is missing: {source}"),
                    )
                    .at_path(relative)
                    .at_field(field),
                );
                return;
            }
        }
    }
}

fn validate_projects(
    root: &Path,
    config: &Config,
    issues: &mut Vec<ValidationIssue>,
) -> (HashSet<String>, usize) {
    let directory = root.join(&config.projects_directory);
    if !directory.is_dir() {
        return (HashSet::new(), 0);
    }
    let mut valid = HashSet::new();
    let mut count = 0;
    for entry in WalkDir::new(&directory)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                issues.push(
                    ValidationIssue::error(
                        "project_scan_failed",
                        format!("Could not scan projects: {source}"),
                    )
                    .at_path(&config.projects_directory),
                );
                continue;
            }
        };
        let relative = relative_to(root, entry.path());
        if entry.file_type().is_symlink() {
            issues.push(
                ValidationIssue::error("record_symlink", "Managed records must not be symlinks")
                    .at_path(relative),
            );
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "md")
        {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Unexpected non-Markdown file in projects directory",
                )
                .at_path(relative),
            );
            continue;
        }
        count += 1;
        if entry.depth() != 1 {
            issues.push(
                ValidationIssue::error(
                    "nested_project",
                    "Project records must be directly inside the projects directory",
                )
                .at_path(relative),
            );
            continue;
        }
        let Some(slug) = entry.path().file_stem().and_then(|value| value.to_str()) else {
            issues.push(
                ValidationIssue::error(
                    "invalid_project_path",
                    "Project filename must be valid UTF-8",
                )
                .at_path(relative),
            );
            continue;
        };
        if let Err(error) = validate_project_slug(slug) {
            issues.push(issue_from_error(error, &relative));
            continue;
        }
        match read_regular_file(entry.path(), &relative, MAX_RECORD_BYTES as u64).and_then(
            |bytes| {
                parse_project(slug, &relative, &bytes)
                    .map_err(|error| issue_from_error(error, &relative))
            },
        ) {
            Ok(_) => {
                valid.insert(slug.to_owned());
            }
            Err(issue) => issues.push(issue),
        }
    }
    (valid, count)
}

fn validate_tasks(
    root: &Path,
    config: &Config,
    projects: &HashSet<String>,
    issues: &mut Vec<ValidationIssue>,
) -> usize {
    let directory = root.join(&config.tasks_directory);
    if !directory.is_dir() {
        return 0;
    }
    let mut ids = HashMap::<String, PathBuf>::new();
    let mut count = 0;
    for entry in WalkDir::new(&directory)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                issues.push(
                    ValidationIssue::error(
                        "task_scan_failed",
                        format!("Could not scan tasks: {source}"),
                    )
                    .at_path(&config.tasks_directory),
                );
                continue;
            }
        };
        let relative = relative_to(root, entry.path());
        if entry.file_type().is_symlink() {
            issues.push(
                ValidationIssue::error("record_symlink", "Managed records must not be symlinks")
                    .at_path(relative),
            );
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .is_none_or(|extension| extension != "md")
        {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Unexpected non-Markdown file in tasks directory",
                )
                .at_path(relative),
            );
            continue;
        }
        count += 1;
        let Some(id) = entry.path().file_stem().and_then(|value| value.to_str()) else {
            issues.push(
                ValidationIssue::error("invalid_task_path", "Task filename must be valid UTF-8")
                    .at_path(relative),
            );
            continue;
        };
        if let Err(error) = validate_task_id(id) {
            issues.push(issue_from_error(error, &relative));
            continue;
        }
        let normalized = id.to_ascii_uppercase();
        if let Some(first) = ids.insert(normalized.clone(), relative.clone()) {
            issues.push(
                ValidationIssue::error(
                    "duplicate_task_id",
                    format!("Task ID {normalized} also appears at {}", first.display()),
                )
                .at_path(&relative),
            );
        }
        match read_regular_file(entry.path(), &relative, MAX_RECORD_BYTES as u64).and_then(
            |bytes| {
                parse_task(id, &relative, &bytes, config)
                    .map_err(|error| issue_from_error(error, &relative))
            },
        ) {
            Ok(task) => {
                for project in &task.projects {
                    if !projects.contains(project) {
                        issues.push(
                            ValidationIssue::error(
                                "missing_project_reference",
                                format!("Referenced project {project:?} does not exist"),
                            )
                            .at_path(&relative)
                            .at_field("projects")
                            .with_suggestion(format!(
                                "Create project {project:?} or remove this reference"
                            )),
                        );
                    }
                }
            }
            Err(issue) => issues.push(issue),
        }
    }
    count
}

fn read_regular_file(
    absolute: &Path,
    relative: &Path,
    limit: u64,
) -> std::result::Result<Vec<u8>, ValidationIssue> {
    let metadata = fs::symlink_metadata(absolute).map_err(|source| {
        ValidationIssue::error(
            "file_unreadable",
            format!("Could not inspect file: {source}"),
        )
        .at_path(relative)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(
            ValidationIssue::error("invalid_store_file", "Expected a regular file")
                .at_path(relative),
        );
    }
    if metadata.len() > limit {
        return Err(ValidationIssue::error(
            "file_too_large",
            format!("File exceeds the {limit}-byte limit"),
        )
        .at_path(relative));
    }
    fs::read(absolute).map_err(|source| {
        ValidationIssue::error("file_unreadable", format!("Could not read file: {source}"))
            .at_path(relative)
    })
}

fn issue_from_error(error: Error, fallback_path: &Path) -> ValidationIssue {
    let mut issue = ValidationIssue::error(error.code(), error.message()).at_path(
        error
            .path()
            .map_or_else(|| fallback_path.to_path_buf(), Path::to_path_buf),
    );
    if let Some(field) = error.field() {
        issue = issue.at_field(field);
    }
    if let (Some(line), Some(column)) = (error.line(), error.column()) {
        issue = issue.at_location(line, column);
    }
    issue
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::commands::init::{initialize, InitOptions};
    use crate::config::CONFIG_PATH;

    use super::*;

    fn initialized() -> (TempDir, PathBuf) {
        let temp = TempDir::new().expect("temp");
        let root = temp.path().join("Todo");
        initialize(&InitOptions {
            store_path: &root,
            vault_root: Some(temp.path()),
            current_directory: temp.path(),
            adopt_empty_layout: false,
            dry_run: false,
        })
        .expect("initialize");
        (temp, root)
    }

    #[test]
    fn initialized_store_is_valid() {
        let (_temp, root) = initialized();
        let report = validate_store(root);
        assert!(report.valid, "{:?}", report.issues);
        assert_eq!(report.errors, 0);
    }

    #[test]
    fn reports_multiple_independent_errors_sorted_by_path() {
        let (_temp, root) = initialized();
        fs::write(root.join("Projects/Bad Slug.md"), b"not front matter").expect("bad project");
        fs::write(
            root.join("Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md"),
            b"---\nname: \nstate: missing\nprojects: [\"[[Todo/Projects/nope]]\"]\ntags: [\"bad tag\"]\n---\n",
        )
        .expect("bad task");
        fs::write(root.join("Tasks/unexpected.bin"), b"junk").expect("unexpected");
        let report = validate_store(&root);
        assert!(!report.valid);
        assert!(report.errors >= 2, "{:?}", report.issues);
        assert_eq!(report.warnings, 1);
        assert!(report
            .issues
            .windows(2)
            .all(|pair| pair[0].path <= pair[1].path));
    }

    #[test]
    fn unknown_config_fields_do_not_hide_other_config_validation() {
        let (_temp, root) = initialized();
        let config_path = root.join(CONFIG_PATH);
        let mut source = fs::read_to_string(&config_path).expect("config");
        source.push_str("typo_one = true\ntypo_two = false\n");
        fs::write(config_path, source).expect("write config");
        let report = validate_store(root);
        assert_eq!(
            report
                .issues
                .iter()
                .filter(|issue| issue.code == "unknown_config_field")
                .count(),
            2
        );
    }
}
