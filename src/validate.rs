use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::{
    embedded_schema, managed_paths_overlap, source_location, validate_managed_directory, Config,
    CONFIG_PATH, SCHEMA_PATH, TODOS_BASE_PATH,
};
use crate::error::{Error, IssueSeverity, Result, ValidationIssue};
use crate::frontmatter::{
    parent_from_properties, parse_document, parse_project, parse_task, MAX_RECORD_BYTES,
};
use crate::model::{analyze_parents, validate_project_slug, validate_task_id, ParentRecord};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
const VALIDATION_PROJECT_SLUG: &str = "validation-record";
const VALIDATION_TASK_ID: &str = "01K4B0ZSBZZV25T1K0D3TA8JHR";
type IssueResult<T> = std::result::Result<T, Box<ValidationIssue>>;

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
    validate_store_impl(root.as_ref(), true)
}

pub(crate) fn validate_store_unlocked(root: &Path) -> ValidationReport {
    validate_store_impl(root, false)
}

fn validate_store_impl(supplied_root: &Path, acquire_lock: bool) -> ValidationReport {
    let mut issues = Vec::new();
    let root = match validate_root(supplied_root) {
        Ok(root) => root,
        Err(issue) => {
            issues.push(*issue);
            return ValidationReport::from_issues(issues, 0, 0);
        }
    };
    if !validate_metadata_directory(&root, &mut issues) {
        return ValidationReport::from_issues(issues, 0, 0);
    }
    let _lock = if acquire_lock {
        match acquire_validation_lock(&root) {
            Ok(lock) => lock,
            Err(issue) => {
                issues.push(*issue);
                None
            }
        }
    } else {
        None
    };
    validate_metadata_entries(&root, &mut issues);
    let config = load_config_for_validation(&root, &mut issues);
    validate_schema(
        &root,
        config.as_ref().map(|config| config.schema_version),
        &mut issues,
    );
    let Some(config) = config else {
        return ValidationReport::from_issues(issues, 0, 0);
    };
    validate_store_root_entries(&root, &config, &mut issues);
    if let Err(error) = crate::attachments::ensure_enabled(&config) {
        issues.push(ValidationIssue::warning(error.code(), error.message()).at_path("Attachments"));
    }

    let mut tasks_path = validate_managed_path_on_disk(
        &root,
        &config.tasks_directory,
        "tasks_directory",
        &mut issues,
    );
    let mut projects_path = validate_managed_path_on_disk(
        &root,
        &config.projects_directory,
        "projects_directory",
        &mut issues,
    );
    match fs::canonicalize(root.join(".todo")) {
        Ok(metadata_path) => {
            if tasks_path
                .as_ref()
                .is_some_and(|path| managed_paths_overlap(path, &metadata_path))
            {
                issues.push(
                    ValidationIssue::error(
                        "managed_paths_not_distinct",
                        "Tasks directory resolves into the metadata directory",
                    )
                    .at_path(CONFIG_PATH)
                    .at_field("tasks_directory"),
                );
                tasks_path = None;
            }
            if projects_path
                .as_ref()
                .is_some_and(|path| managed_paths_overlap(path, &metadata_path))
            {
                issues.push(
                    ValidationIssue::error(
                        "managed_paths_not_distinct",
                        "Projects directory resolves into the metadata directory",
                    )
                    .at_path(CONFIG_PATH)
                    .at_field("projects_directory"),
                );
                projects_path = None;
            }
        }
        Err(source) => issues.push(
            ValidationIssue::error(
                "metadata_scan_failed",
                format!("Could not resolve the metadata directory: {source}"),
            )
            .at_path(".todo"),
        ),
    }
    let paths_overlap = matches!(
        (&tasks_path, &projects_path),
        (Some(tasks), Some(projects)) if managed_paths_overlap(tasks, projects)
    );
    if paths_overlap
        && !managed_paths_overlap(
            Path::new(&config.tasks_directory),
            Path::new(&config.projects_directory),
        )
    {
        issues.push(
            ValidationIssue::error(
                "managed_paths_not_distinct",
                "Tasks and projects directories resolve to overlapping paths",
            )
            .at_path(".todo/config.toml"),
        );
    }

    let (project_slugs, project_count) = if paths_overlap {
        (HashSet::new(), 0)
    } else if let Some(projects_path) = projects_path {
        validate_projects(&root, &projects_path, &mut issues)
    } else {
        (HashSet::new(), 0)
    };
    let task_count = if paths_overlap {
        0
    } else if let Some(tasks_path) = tasks_path {
        validate_tasks(&root, &tasks_path, &config, &project_slugs, &mut issues)
    } else {
        0
    };
    ValidationReport::from_issues(issues, task_count, project_count)
}

fn acquire_validation_lock(root: &Path) -> IssueResult<Option<std::fs::File>> {
    let absolute = root.join(CONFIG_PATH);
    let relative = Path::new(CONFIG_PATH);
    let metadata = match fs::symlink_metadata(&absolute) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(Box::new(
                ValidationIssue::error(
                    "validation_lock_failed",
                    format!("Could not inspect the validation lock file: {source}"),
                )
                .at_path(relative),
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&absolute).map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "validation_lock_failed",
                format!("Could not open the validation lock file: {source}"),
            )
            .at_path(relative),
        )
    })?;
    let open_metadata = file.metadata().map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "validation_lock_failed",
                format!("Could not inspect the open validation lock file: {source}"),
            )
            .at_path(relative),
        )
    })?;
    if !open_metadata.is_file() {
        return Err(Box::new(
            ValidationIssue::error(
                "invalid_store_file",
                "Store configuration lock must be a regular file",
            )
            .at_path(relative),
        ));
    }
    FileExt::lock_shared(&file).map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "validation_lock_failed",
                format!("Could not acquire the store's shared advisory lock: {source}"),
            )
            .at_path(relative),
        )
    })?;
    Ok(Some(file))
}

fn validate_root(root: &Path) -> IssueResult<PathBuf> {
    let metadata = fs::symlink_metadata(root).map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "store_not_found",
                format!("Could not inspect the store root: {source}"),
            )
            .at_path("."),
        )
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Box::new(
            ValidationIssue::error("invalid_store_root", "Store root must be a real directory")
                .at_path("."),
        ));
    }
    fs::canonicalize(root).map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "invalid_store_root",
                format!("Could not resolve the store root: {source}"),
            )
            .at_path("."),
        )
    })
}

fn validate_metadata_directory(root: &Path, issues: &mut Vec<ValidationIssue>) -> bool {
    let path = root.join(".todo");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => {
            issues.push(
                ValidationIssue::error(
                    "invalid_metadata_directory",
                    ".todo must be a real directory",
                )
                .at_path(".todo"),
            );
            false
        }
        Err(source) => {
            issues.push(
                ValidationIssue::error(
                    "metadata_directory_missing",
                    format!("Could not read .todo: {source}"),
                )
                .at_path(".todo"),
            );
            false
        }
    }
}

fn validate_metadata_entries(root: &Path, issues: &mut Vec<ValidationIssue>) {
    let directory = root.join(".todo");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) => {
            issues.push(
                ValidationIssue::error(
                    "metadata_scan_failed",
                    format!("Could not inspect .todo contents: {source}"),
                )
                .at_path(".todo"),
            );
            return;
        }
    };
    for entry in entries {
        match entry {
            Ok(entry)
                if matches!(
                    entry.file_name().to_str(),
                    Some("config.toml" | "schema.json")
                ) => {}
            Ok(entry) => issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Unexpected entry in the metadata directory",
                )
                .at_path(relative_to(root, &entry.path())),
            ),
            Err(source) => issues.push(
                ValidationIssue::error(
                    "metadata_scan_failed",
                    format!("Could not inspect a .todo entry: {source}"),
                )
                .at_path(".todo"),
            ),
        }
    }
}

fn validate_store_root_entries(root: &Path, config: &Config, issues: &mut Vec<ValidationIssue>) {
    let mut managed = Vec::with_capacity(2);
    for configured in [&config.tasks_directory, &config.projects_directory] {
        if validate_managed_directory(configured).is_ok() {
            managed.push(PathBuf::from(configured));
        }
    }

    let mut entries = WalkDir::new(root)
        .sort_by_file_name()
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                issues.push(
                    ValidationIssue::error(
                        "store_scan_failed",
                        format!("Could not inspect store contents: {source}"),
                    )
                    .at_path("."),
                );
                continue;
            }
        };
        let relative = relative_to(root, entry.path());
        if relative == Path::new(".todo") || relative == Path::new("Attachments") {
            if entry.file_type().is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        if relative == Path::new(TODOS_BASE_PATH) && entry.file_type().is_file() {
            continue;
        }
        if managed.contains(&relative) {
            if entry.file_type().is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        if managed.iter().any(|path| path.starts_with(&relative)) {
            continue;
        }
        issues.push(
            ValidationIssue::warning(
                "unexpected_file",
                "Unexpected entry outside the configured store trees",
            )
            .at_path(relative),
        );
        if entry.file_type().is_dir() {
            entries.skip_current_dir();
        }
    }
}

fn load_config_for_validation(root: &Path, issues: &mut Vec<ValidationIssue>) -> Option<Config> {
    let path = root.join(CONFIG_PATH);
    let bytes = match read_regular_file(&path, Path::new(CONFIG_PATH), MAX_CONFIG_BYTES) {
        Ok(bytes) => bytes,
        Err(issue) => {
            issues.push(*issue);
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
        Err(parse_error) => {
            let mut issue = ValidationIssue::error(
                "invalid_config",
                format!("Invalid configuration syntax: {parse_error}"),
            )
            .at_path(CONFIG_PATH);
            if let Some(span) = parse_error.span() {
                let (line, column) = source_location(&source, span.start);
                issue = issue.at_location(line, column);
            }
            issues.push(issue);
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
    let declared_schema_version = table
        .get("schema_version")
        .and_then(toml::Value::as_integer);
    if let Some(version) = declared_schema_version.filter(|version| !matches!(*version, 1 | 2)) {
        issues.push(
            ValidationIssue::error(
                "unsupported_schema",
                format!("Schema version {version} is unsupported; supported versions are 1 and 2"),
            )
            .at_path(CONFIG_PATH)
            .at_field("schema_version"),
        );
        return None;
    }
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

fn validate_schema(root: &Path, version: Option<u32>, issues: &mut Vec<ValidationIssue>) {
    let path = root.join(SCHEMA_PATH);
    let bytes = match read_regular_file(&path, Path::new(SCHEMA_PATH), MAX_CONFIG_BYTES) {
        Ok(bytes) => bytes,
        Err(issue) => {
            issues.push(*issue);
            return;
        }
    };
    let source = match embedded_schema(version.unwrap_or(2)) {
        Ok(source) => source,
        Err(error) => {
            issues.push(issue_from_error(error, Path::new(CONFIG_PATH)));
            return;
        }
    };
    let embedded = match serde_json::from_str::<serde_json::Value>(source) {
        Ok(schema) => schema,
        Err(source) => {
            issues.push(ValidationIssue::error(
                "invalid_embedded_schema",
                format!("The schema embedded in this build is invalid JSON: {source}"),
            ));
            return;
        }
    };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(schema) if version.is_none() || schema == embedded => {}
        Ok(_) => issues.push(
            ValidationIssue::error(
                "schema_version_mismatch",
                format!(
                    "schema.json is not compatible with schema version {}",
                    version.unwrap_or(2)
                ),
            )
            .at_path(SCHEMA_PATH),
        ),
        Err(source) => issues.push(
            ValidationIssue::error(
                "invalid_schema_file",
                format!("schema.json is invalid JSON: {source}"),
            )
            .at_path(SCHEMA_PATH)
            .at_location(source.line(), source.column()),
        ),
    }
}

fn validate_managed_path_on_disk(
    root: &Path,
    configured: &str,
    field: &str,
    issues: &mut Vec<ValidationIssue>,
) -> Option<PathBuf> {
    if validate_managed_directory(configured).is_err() {
        return None;
    }
    let relative = Path::new(configured);
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return None;
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
                return None;
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
                return None;
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
                return None;
            }
        }
    }
    match fs::canonicalize(&current) {
        Ok(path) if path.starts_with(root) => Some(path),
        Ok(_) => {
            issues.push(
                ValidationIssue::error(
                    "managed_path_escape",
                    "Managed path resolves outside the store",
                )
                .at_path(relative)
                .at_field(field),
            );
            None
        }
        Err(source) => {
            issues.push(
                ValidationIssue::error(
                    "managed_path_unreadable",
                    format!("Could not resolve configured managed directory: {source}"),
                )
                .at_path(relative)
                .at_field(field),
            );
            None
        }
    }
}

fn validate_projects(
    root: &Path,
    directory: &Path,
    issues: &mut Vec<ValidationIssue>,
) -> (HashSet<String>, usize) {
    if !directory.is_dir() {
        return (HashSet::new(), 0);
    }
    let mut valid = HashSet::new();
    let mut seen = HashMap::<String, PathBuf>::new();
    let mut count = 0;
    let mut entries = WalkDir::new(directory)
        .sort_by_file_name()
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                issues.push(
                    ValidationIssue::error(
                        "project_scan_failed",
                        format!("Could not scan projects: {source}"),
                    )
                    .at_path(relative_to(root, directory)),
                );
                continue;
            }
        };
        let relative = relative_to(root, entry.path());
        if is_hidden_name(entry.file_name()) {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Ignored hidden file or directory in projects directory",
                )
                .at_path(relative),
            );
            if entry.file_type().is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        if entry.file_type().is_symlink() {
            issues.push(
                ValidationIssue::error("record_symlink", "Managed records must not be symlinks")
                    .at_path(relative),
            );
            continue;
        }
        if entry.file_type().is_dir() {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Unexpected directory in the flat projects directory",
                )
                .at_path(relative),
            );
            continue;
        }
        if !entry.file_type().is_file() {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Unexpected non-file entry in projects directory",
                )
                .at_path(relative),
            );
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
        let nested = entry.depth() != 1;
        if nested {
            issues.push(
                ValidationIssue::error(
                    "nested_project",
                    "Project records must be directly inside the projects directory",
                )
                .at_path(&relative),
            );
        }
        let slug = entry.path().file_stem().and_then(|value| value.to_str());
        let valid_slug = match slug {
            Some(slug) => match validate_project_slug(slug) {
                Ok(()) => Some(slug),
                Err(error) => {
                    issues.push(issue_from_error(error, &relative));
                    None
                }
            },
            None => {
                issues.push(
                    ValidationIssue::error(
                        "invalid_project_path",
                        "Project filename must be valid UTF-8",
                    )
                    .at_path(&relative),
                );
                None
            }
        };
        if let Some(slug) = valid_slug {
            if let Some(first) = seen.insert(slug.to_owned(), relative.clone()) {
                issues.push(
                    ValidationIssue::error(
                        "duplicate_project_slug",
                        format!("Project slug {slug:?} also appears at {}", first.display()),
                    )
                    .at_path(&relative)
                    .at_field("slug"),
                );
            }
            if !nested {
                valid.insert(slug.to_owned());
            }
        }
        let parse_slug = valid_slug.unwrap_or(VALIDATION_PROJECT_SLUG);
        match read_regular_file(entry.path(), &relative, MAX_RECORD_BYTES as u64).and_then(
            |bytes| {
                parse_project(parse_slug, &relative, &bytes)
                    .map_err(|error| Box::new(issue_from_error(error, &relative)))
            },
        ) {
            Ok(_) => {}
            Err(issue) => issues.push(*issue),
        }
    }
    (valid, count)
}

fn validate_tasks(
    root: &Path,
    directory: &Path,
    config: &Config,
    projects: &HashSet<String>,
    issues: &mut Vec<ValidationIssue>,
) -> usize {
    if !directory.is_dir() {
        return 0;
    }
    let mut ids = HashMap::<String, PathBuf>::new();
    let mut parent_records = Vec::new();
    let mut count = 0;
    let mut entries = WalkDir::new(directory)
        .sort_by_file_name()
        .min_depth(1)
        .follow_links(false)
        .into_iter();
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                issues.push(
                    ValidationIssue::error(
                        "task_scan_failed",
                        format!("Could not scan tasks: {source}"),
                    )
                    .at_path(relative_to(root, directory)),
                );
                continue;
            }
        };
        let relative = relative_to(root, entry.path());
        if is_hidden_name(entry.file_name()) {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Ignored hidden file or directory in tasks directory",
                )
                .at_path(relative),
            );
            if entry.file_type().is_dir() {
                entries.skip_current_dir();
            }
            continue;
        }
        if entry.file_type().is_symlink() {
            if config.schema_version == 2 && entry.path().extension() == Some(OsStr::new("md")) {
                if let Some(id) = entry.path().file_stem().and_then(|value| value.to_str()) {
                    if validate_task_id(id).is_ok() {
                        parent_records.push(ParentRecord {
                            id: id.to_ascii_uppercase(),
                            path: relative.clone(),
                            parent: None,
                        });
                    }
                }
            }
            issues.push(
                ValidationIssue::error("record_symlink", "Managed records must not be symlinks")
                    .at_path(relative),
            );
            continue;
        }
        if entry.file_type().is_dir() {
            continue;
        }
        if !entry.file_type().is_file() {
            issues.push(
                ValidationIssue::warning(
                    "unexpected_file",
                    "Unexpected non-file entry in tasks directory",
                )
                .at_path(relative),
            );
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
        let id = entry.path().file_stem().and_then(|value| value.to_str());
        let normalized = match id {
            Some(id) => match validate_task_id(id) {
                Ok(()) => Some(id.to_ascii_uppercase()),
                Err(error) => {
                    issues.push(issue_from_error(error, &relative));
                    None
                }
            },
            None => {
                issues.push(
                    ValidationIssue::error(
                        "invalid_task_path",
                        "Task filename must be valid UTF-8",
                    )
                    .at_path(&relative),
                );
                None
            }
        };
        let graph_index = normalized.as_ref().map(|id| {
            let index = parent_records.len();
            parent_records.push(ParentRecord {
                id: id.clone(),
                path: relative.clone(),
                parent: None,
            });
            index
        });
        if config.schema_version == 1 {
            if let Some(normalized) = &normalized {
                if let Some(first) = ids.insert(normalized.clone(), relative.clone()) {
                    issues.push(
                        ValidationIssue::error(
                            "duplicate_task_id",
                            format!("Task ID {normalized} also appears at {}", first.display()),
                        )
                        .at_path(&relative),
                    );
                }
            }
        }
        let parse_id = normalized.as_deref().unwrap_or(VALIDATION_TASK_ID);
        let bytes = match read_regular_file(entry.path(), &relative, MAX_RECORD_BYTES as u64) {
            Ok(bytes) => bytes,
            Err(issue) => {
                issues.push(*issue);
                continue;
            }
        };
        match parse_task(parse_id, &relative, &bytes, config) {
            Ok(task) => {
                if crate::attachments::ensure_enabled(config).is_ok() {
                    issues.extend(crate::attachments::diagnostics(
                        root,
                        &relative,
                        &task.body,
                        &config.obsidian_link_prefix,
                    ));
                }
                if let Some(index) = graph_index {
                    parent_records[index].parent = task.parent;
                }
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
            Err(error) => {
                let parent_error = error.field() == Some("parent");
                issues.push(issue_from_error(error, &relative));
                if config.schema_version == 2 {
                    if let Ok(document) = parse_document(&bytes) {
                        match parent_from_properties(&document.properties) {
                            Ok(parent) => {
                                if let Some(index) = graph_index {
                                    parent_records[index].parent = parent;
                                }
                            }
                            Err(error) if !parent_error => {
                                issues.push(issue_from_error(error, &relative));
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
        }
    }
    if config.schema_version == 2 {
        issues.extend(analyze_parents(&parent_records));
    }
    count
}

fn read_regular_file(absolute: &Path, relative: &Path, limit: u64) -> IssueResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(absolute).map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "file_unreadable",
                format!("Could not inspect file: {source}"),
            )
            .at_path(relative),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Box::new(
            ValidationIssue::error("invalid_store_file", "Expected a regular file")
                .at_path(relative),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(absolute).map_err(|source| {
        Box::new(
            ValidationIssue::error("file_unreadable", format!("Could not open file: {source}"))
                .at_path(relative),
        )
    })?;
    let metadata = file.metadata().map_err(|source| {
        Box::new(
            ValidationIssue::error(
                "file_unreadable",
                format!("Could not inspect open file: {source}"),
            )
            .at_path(relative),
        )
    })?;
    if !metadata.is_file() {
        return Err(Box::new(
            ValidationIssue::error("invalid_store_file", "Expected a regular file")
                .at_path(relative),
        ));
    }
    if metadata.len() > limit {
        return Err(Box::new(
            ValidationIssue::error(
                "file_too_large",
                format!("File exceeds the {limit}-byte limit"),
            )
            .at_path(relative),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| {
            Box::new(
                ValidationIssue::error("file_unreadable", format!("Could not read file: {source}"))
                    .at_path(relative),
            )
        })?;
    if exceeds_limit(bytes.len(), limit) {
        return Err(Box::new(
            ValidationIssue::error(
                "file_too_large",
                format!("File exceeds the {limit}-byte limit"),
            )
            .at_path(relative),
        ));
    }
    Ok(bytes)
}
fn exceeds_limit(length: usize, limit: u64) -> bool {
    match u64::try_from(length) {
        Ok(length) => length > limit,
        Err(_) => true,
    }
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

fn is_hidden_name(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

fn relative_to(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map_or_else(|_| path.to_path_buf(), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

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
        assert_eq!(report.warnings, 0, "{:?}", report.issues);
    }
    #[test]
    fn visible_task_subdirectories_are_valid_storage() {
        let (_temp, root) = initialized();
        let nested = root.join("Tasks/archive");
        fs::create_dir(&nested).expect("task subdirectory");
        fs::write(
            nested.join("01K4B0ZSBZZV25T1K0D3TA8JHR.md"),
            b"---\nname: Archived placement\nstate: open\nprojects: []\ntags: []\n---\n",
        )
        .expect("nested task");

        let report = validate_store(&root);
        assert!(report.valid, "{:?}", report.issues);
        assert_eq!(report.warnings, 0, "{:?}", report.issues);
        assert_eq!(report.tasks, 1);
        let store = crate::store::Store::open(root).expect("open");
        let task = store
            .get_task("01K4B0")
            .expect("resolve recursively stored task");
        assert_eq!(
            task.path,
            Path::new("Tasks/archive/01K4B0ZSBZZV25T1K0D3TA8JHR.md")
        );
    }

    #[test]
    fn case_variant_task_ids_report_duplicate_identity() {
        let (_temp, root) = initialized();
        let id = "01K4B0ZSBZZV25T1K0D3TA8JHR";
        let record = b"---\nname: Task\nstate: open\nprojects: []\ntags: []\n---\n";
        fs::create_dir(root.join("Tasks/one")).expect("first task directory");
        fs::create_dir(root.join("Tasks/two")).expect("second task directory");
        fs::write(root.join(format!("Tasks/one/{id}.md")), record).expect("uppercase task");
        fs::write(
            root.join(format!("Tasks/two/{}.md", id.to_ascii_lowercase())),
            record,
        )
        .expect("lowercase task");
        let report = validate_store(root);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "duplicate_task_id"));
        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_task_id"));
    }

    #[test]
    fn schema_must_match_the_authoritative_embedded_shape() {
        let (_temp, root) = initialized();
        fs::write(
            root.join(SCHEMA_PATH),
            r#"{"x-obsidian-todo-schema-version":1}"#,
        )
        .expect("tamper with schema");
        let report = validate_store(root);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "schema_version_mismatch"));
        assert_eq!(
            report
                .into_result()
                .expect_err("incompatible schema")
                .exit_code(),
            7
        );
    }

    #[test]
    fn nested_project_duplicates_are_reported_independently() {
        let (_temp, root) = initialized();
        let record = b"---\nname: Work\n---\n";
        fs::write(root.join("Projects/work.md"), record).expect("flat project");
        fs::create_dir(root.join("Projects/nested")).expect("nested directory");
        fs::write(root.join("Projects/nested/work.md"), record).expect("nested project");
        let report = validate_store(root);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "duplicate_project_slug"));
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "nested_project"));
    }

    #[test]
    fn invalid_record_paths_do_not_hide_independent_content_errors() {
        let (_temp, root) = initialized();
        fs::create_dir(root.join("Projects/nested")).expect("nested projects");
        fs::write(
            root.join("Projects/nested/Bad Slug.md"),
            b"not front matter\n",
        )
        .expect("invalid project");
        fs::write(root.join("Tasks/not-an-id.md"), b"not front matter\n").expect("invalid task");

        let report = validate_store(root);
        let codes = report
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect::<HashSet<_>>();
        for code in [
            "nested_project",
            "invalid_project_slug",
            "invalid_task_id",
            "missing_frontmatter",
        ] {
            assert!(codes.contains(code), "missing {code}: {:?}", report.issues);
        }
    }

    #[test]
    fn existing_project_paths_satisfy_references_even_when_metadata_is_invalid() {
        let (_temp, root) = initialized();
        fs::write(root.join("Projects/work.md"), b"---\nplugin: true\n---\n")
            .expect("invalid project");
        fs::write(
            root.join("Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md"),
            b"---\nname: Task\nstate: open\nprojects: [\"[[Todo/Projects/work]]\"]\ntags: []\n---\n",
        )
        .expect("task");
        let report = validate_store(root);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "missing_property"));
        assert!(!report
            .issues
            .iter()
            .any(|issue| issue.code == "missing_project_reference"));
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
    fn reports_unexpected_root_and_metadata_entries_as_warnings() {
        let (_temp, root) = initialized();
        fs::write(root.join("notes.md"), "unmanaged").expect("root note");
        fs::write(root.join(".todo/config.toml.bak"), "backup").expect("metadata backup");
        let report = validate_store(root);
        assert!(report.valid, "{:?}", report.issues);
        assert_eq!(report.warnings, 2);
        assert!(report
            .issues
            .iter()
            .all(|issue| issue.code == "unexpected_file"));
    }

    #[test]
    fn nested_managed_paths_do_not_hide_unexpected_siblings() {
        let (_temp, root) = initialized();
        let config_path = root.join(CONFIG_PATH);
        let mut config = Config::parse(&fs::read_to_string(&config_path).expect("config"))
            .expect("parse config");
        config.tasks_directory = "Data/Tasks".to_owned();
        config.projects_directory = "Data/Projects".to_owned();
        fs::remove_dir(root.join("Tasks")).expect("remove old tasks");
        fs::remove_dir(root.join("Projects")).expect("remove old projects");
        fs::create_dir_all(root.join("Data/Tasks")).expect("new tasks");
        fs::create_dir_all(root.join("Data/Projects")).expect("new projects");
        fs::write(config_path, config.to_toml().expect("serialize config")).expect("update config");
        fs::write(root.join("Data/stray.md"), "unmanaged").expect("stray note");

        let report = validate_store(root);
        assert!(report.valid, "{:?}", report.issues);
        assert_eq!(report.warnings, 1);
        assert_eq!(
            report.issues[0].path.as_deref(),
            Some(Path::new("Data/stray.md"))
        );
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
    #[test]
    fn future_schema_is_reported_even_when_v1_fields_are_missing() {
        let (_temp, root) = initialized();
        fs::write(
            root.join(CONFIG_PATH),
            "schema_version = 3\nfuture_field = true\n",
        )
        .expect("write future config");
        let report = validate_store(root);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "unsupported_schema"));
        assert_eq!(
            report
                .into_result()
                .expect_err("unsupported schema")
                .exit_code(),
            7
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_symlink_is_rejected_without_reading_its_config() {
        let (temp, root) = initialized();
        let outside = temp.path().join("outside-metadata");
        fs::create_dir(&outside).expect("outside metadata");
        fs::write(
            outside.join("config.toml"),
            "schema_version = 2\nfuture_field = true\n",
        )
        .expect("outside config");
        fs::remove_dir_all(root.join(".todo")).expect("remove metadata");
        symlink(&outside, root.join(".todo")).expect("metadata symlink");

        let report = validate_store(root);
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].code, "invalid_metadata_directory");
    }

    #[test]
    fn invalid_managed_path_is_never_scanned_outside_store() {
        let (temp, root) = initialized();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(
            outside.join("01K4B0ZSBZZV25T1K0D3TA8JHR.md"),
            b"---\nname: Outside\nstate: open\nprojects: []\ntags: []\n---\n",
        )
        .expect("outside task");
        let config_path = root.join(CONFIG_PATH);
        let mut config: toml::Value =
            toml::from_str(&fs::read_to_string(&config_path).expect("config")).expect("TOML");
        config["tasks_directory"] = toml::Value::String(outside.to_string_lossy().into_owned());
        fs::write(
            config_path,
            toml::to_string_pretty(&config).expect("serialize config"),
        )
        .expect("write config");

        let report = validate_store(&root);
        assert_eq!(report.tasks, 0);
        assert!(report
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_managed_path"));
        assert!(report
            .issues
            .iter()
            .all(|issue| issue.path.as_deref() != Some(outside.as_path())));
    }

    #[test]
    fn normal_scans_ignore_hidden_records_and_validate_warns() {
        let (_temp, root) = initialized();
        let record = b"---\nname: Hidden\nstate: open\nprojects: []\ntags: []\n---\n";
        fs::write(root.join("Tasks/.editor.md"), record).expect("hidden task");
        fs::write(
            root.join("Projects/.editor.md"),
            b"---\nname: Hidden\n---\n",
        )
        .expect("hidden project");

        let store = crate::store::Store::open(&root).expect("open store");
        assert!(store.list_tasks().expect("tasks").is_empty());
        assert!(store.list_projects().expect("projects").is_empty());
        let report = validate_store(root);
        assert!(report.valid);
        assert_eq!(report.tasks, 0);
        assert_eq!(report.projects, 0);
        assert_eq!(report.warnings, 2);
    }

    #[test]
    fn malformed_physical_targets_and_invalid_filenames_do_not_fabricate_graph_nodes() {
        let (_temp, root) = initialized();
        let parent = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let child = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
        let orphan = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
        let missing = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
        let dummy_reference = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
        let invalid_parent = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
        fs::write(
            root.join(format!("Tasks/{parent}.md")),
            "not front matter\n",
        )
        .expect("malformed target");
        for (id, metadata) in [
            (child, format!("name: Child\nparent: \"{parent}\"")),
            (orphan, format!("parent: \"{missing}\"")),
            (
                "not-an-id",
                format!("name: Bad filename\nparent: \"{VALIDATION_TASK_ID}\""),
            ),
            (
                dummy_reference,
                format!("name: Missing physical target\nparent: \"{VALIDATION_TASK_ID}\""),
            ),
            (invalid_parent, "parent: null".to_owned()),
        ] {
            fs::write(
                root.join(format!("Tasks/{id}.md")),
                format!("---\n{metadata}\nstate: open\nprojects: []\ntags: []\n---\n"),
            )
            .expect("record");
        }
        let report = validate_store(&root);
        let graph = report
            .issues
            .iter()
            .filter(|issue| issue.field.as_deref() == Some("parent"))
            .map(|issue| (issue.path.clone().expect("path"), issue.code.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            graph,
            vec![
                (
                    PathBuf::from(format!("Tasks/{orphan}.md")),
                    "missing_parent_reference"
                ),
                (
                    PathBuf::from(format!("Tasks/{dummy_reference}.md")),
                    "missing_parent_reference"
                ),
                (
                    PathBuf::from(format!("Tasks/{invalid_parent}.md")),
                    "invalid_parent_id"
                ),
            ]
        );
        for (id, code) in [
            (parent, "missing_frontmatter"),
            (orphan, "missing_property"),
            (invalid_parent, "missing_property"),
            ("not-an-id", "invalid_task_id"),
        ] {
            assert!(
                report.issues.iter().any(|issue| issue.path.as_deref()
                    == Some(Path::new(&format!("Tasks/{id}.md")))
                    && issue.code == code),
                "{id}: {:?}",
                report.issues
            );
        }
        assert_eq!(report.tasks, 6);
    }

    #[test]
    fn safely_parsed_edges_preserve_cycles_despite_unrelated_record_errors() {
        let (_temp, root) = initialized();
        let first = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let second = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
        let descendant = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
        for (id, parent, name) in [
            (first, second, ""),
            (second, first, "name: Terminal\n"),
            (descendant, second, "name: Descendant\n"),
        ] {
            fs::write(
                root.join(format!("Tasks/{id}.md")),
                format!(
                    "---\n{name}state: done\nprojects: []\ntags: []\nparent: \"{parent}\"\n---\n"
                ),
            )
            .expect("record");
        }
        let report = validate_store(&root);
        let cycles = report
            .issues
            .iter()
            .filter(|issue| issue.code == "parent_cycle")
            .map(|issue| issue.path.clone().expect("path"))
            .collect::<Vec<_>>();
        assert_eq!(
            cycles,
            vec![
                PathBuf::from(format!("Tasks/{first}.md")),
                PathBuf::from(format!("Tasks/{second}.md")),
            ]
        );
        assert_eq!(report.errors, 3, "{:?}", report.issues);
    }

    #[test]
    fn validation_selects_exact_historical_schema_without_promoting_parent_extras() {
        let (_temp, root) = initialized();
        let mut config = Config::defaults("Todo".to_owned());
        config.schema_version = 1;
        fs::write(root.join(CONFIG_PATH), config.to_toml().expect("config"))
            .expect("legacy config");
        fs::write(root.join(SCHEMA_PATH), embedded_schema(1).expect("v1")).expect("legacy schema");
        fs::write(
            root.join("Tasks/01ARZ3NDEKTSV4RRFFQ69G5FAV.md"),
            "---\nname: Legacy\nstate: open\nprojects: []\ntags: []\nparent: null\n---\n",
        )
        .expect("legacy metadata");
        assert!(validate_store(&root).valid);
        fs::write(root.join(SCHEMA_PATH), embedded_schema(2).expect("v2"))
            .expect("interrupted upgrade");
        let intermediate = validate_store(&root);
        assert_eq!(intermediate.errors, 1, "{:?}", intermediate.issues);
        assert_eq!(intermediate.issues[0].code, "schema_version_mismatch");
        config.schema_version = 2;
        fs::write(root.join(CONFIG_PATH), config.to_toml().expect("config")).expect("v2 config");
        fs::write(root.join(SCHEMA_PATH), embedded_schema(1).expect("v1")).expect("reversed pair");
        let reversed = validate_store(&root);
        assert_eq!(reversed.errors, 2, "{:?}", reversed.issues);
        assert!(reversed
            .issues
            .iter()
            .any(|issue| issue.code == "schema_version_mismatch"));
        assert!(reversed
            .issues
            .iter()
            .any(|issue| issue.code == "invalid_parent_id"));
    }

    #[test]
    fn upgrade_preflight_reuses_validation_under_an_existing_exclusive_lock() {
        let (_temp, root) = initialized();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(CONFIG_PATH))
            .expect("config");
        file.lock_exclusive().expect("exclusive lock");
        fs::write(
            root.join("Tasks/01ARZ3NDEKTSV4RRFFQ69G5FAV.md"),
            "---\nname: Orphan\nstate: open\nprojects: []\ntags: []\nparent: \"01ARZ3NDEKTSV4RRFFQ69G5FAW\"\n---\n",
        ).expect("orphan");
        let report = validate_store_unlocked(&root);
        assert_eq!(report.errors, 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].code, "missing_parent_reference");
    }

    #[test]
    fn malformed_duplicate_identity_suppresses_only_ambiguous_graph_claims() {
        let (_temp, root) = initialized();
        let duplicate = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
        let child = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
        let orphan = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
        let missing = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
        fs::create_dir(root.join("Tasks/nested")).expect("nested tasks");
        for (path, name, parent) in [
            (format!("Tasks/{duplicate}.md"), "", duplicate),
            (
                format!("Tasks/nested/{}.md", duplicate.to_ascii_lowercase()),
                "name: Duplicate\n",
                missing,
            ),
            (format!("Tasks/{child}.md"), "name: Child\n", duplicate),
            (
                format!("Tasks/{orphan}.md"),
                "name: Independent orphan\n",
                missing,
            ),
        ] {
            fs::write(
                root.join(path),
                format!(
                    "---\n{name}state: open\nprojects: []\ntags: []\nparent: \"{parent}\"\n---\n"
                ),
            )
            .expect("record");
        }
        let report = validate_store(&root);
        let mut codes = report
            .issues
            .iter()
            .map(|issue| issue.code.as_str())
            .collect::<Vec<_>>();
        codes.sort_unstable();
        assert_eq!(
            codes,
            [
                "duplicate_task_id",
                "missing_parent_reference",
                "missing_property"
            ]
        );
        let missing_issue = report
            .issues
            .iter()
            .find(|issue| issue.code == "missing_parent_reference")
            .expect("orphan");
        assert_eq!(
            missing_issue.path.as_ref(),
            Some(&PathBuf::from(format!("Tasks/{orphan}.md")))
        );
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_physical_parent_is_reported_without_following_or_fabricating_absence() {
        let (temp, root) = initialized();
        let outside = temp.path().join("outside.md");
        fs::write(&outside, "not front matter\n").expect("outside record");
        symlink(&outside, root.join("Tasks/01ARZ3NDEKTSV4RRFFQ69G5FAV.md")).expect("unsafe parent");
        fs::write(
            root.join("Tasks/01ARZ3NDEKTSV4RRFFQ69G5FAW.md"),
            "---\nname: Child\nstate: open\nprojects: []\ntags: []\nparent: \"01ARZ3NDEKTSV4RRFFQ69G5FAV\"\n---\n",
        ).expect("child");
        let report = validate_store(&root);
        assert_eq!(report.errors, 1, "{:?}", report.issues);
        assert_eq!(report.issues[0].code, "record_symlink");
    }
}
