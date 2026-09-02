use std::io::Write;
use std::path::Path;

use obsidian_todo::error::{Error, ErrorKind, IssueSeverity, Result, ValidationIssue};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Debug)]
pub struct CommandOutput {
    pub human: String,
    pub json: Value,
}

impl CommandOutput {
    #[must_use]
    pub fn new(human: impl Into<String>, json: Value) -> Self {
        Self {
            human: human.into(),
            json,
        }
    }
}
#[must_use]
pub fn json_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

#[must_use]
pub fn human_issue(issue: &ValidationIssue) -> String {
    let severity = match issue.severity {
        IssueSeverity::Error => "error",
        IssueSeverity::Warning => "warning",
    };
    let path = issue
        .path
        .as_deref()
        .map_or_else(|| "<store>".to_owned(), |path| path.display().to_string());
    let location = match (issue.line, issue.column) {
        (Some(line), Some(column)) => format!(":{line}:{column}"),
        (Some(line), None) => format!(":{line}"),
        (None, _) => String::new(),
    };
    let field = issue
        .field
        .as_deref()
        .map_or_else(String::new, |field| format!(" [{field}]"));
    format!(
        "{severity}[{}] {path}{location}{field}: {}",
        issue.code, issue.message
    )
}

pub fn write_success(
    output: &CommandOutput,
    format: OutputFormat,
    writer: &mut impl Write,
) -> Result<()> {
    match format {
        OutputFormat::Human => writeln!(writer, "{}", output.human).map_err(|source| {
            Error::new(
                ErrorKind::Io,
                "io_error",
                format!("Could not write output: {source}"),
            )
        }),
        OutputFormat::Json => {
            serde_json::to_writer(&mut *writer, &output.json).map_err(|source| {
                Error::new(
                    ErrorKind::Io,
                    "io_error",
                    format!("Could not write JSON output: {source}"),
                )
            })?;
            writeln!(writer).map_err(|source| {
                Error::new(
                    ErrorKind::Io,
                    "io_error",
                    format!("Could not write output: {source}"),
                )
            })
        }
    }
}

pub fn write_error(error: &Error, format: OutputFormat, writer: &mut impl Write) {
    let result = match format {
        OutputFormat::Json => {
            let issues = error
                .issues()
                .iter()
                .map(|issue| {
                    json!({
                        "code": issue.code,
                        "path": issue.path.as_deref().map(json_path),
                        "line": issue.line,
                        "column": issue.column,
                        "field": issue.field,
                        "message": issue.message,
                        "suggestion": issue.suggestion,
                        "severity": issue.severity,
                    })
                })
                .collect::<Vec<_>>();
            let document = json!({
                "version": 1,
                "error": {
                    "code": error.code(),
                    "message": error.message(),
                    "path": error.path().map(json_path),
                    "field": error.field(),
                    "line": error.line(),
                    "column": error.column(),
                    "issues": issues,
                    "validation": error.validation_summary(),
                }
            });
            serde_json::to_writer(&mut *writer, &document)
                .and_then(|()| writeln!(writer).map_err(serde_json::Error::io))
                .map_err(|source| source.to_string())
        }
        OutputFormat::Human => {
            if writeln!(writer, "error[{}]: {error}", error.code()).is_err() {
                return;
            }
            if let Some(summary) = error.validation_summary() {
                if writeln!(
                    writer,
                    "validation: {} task(s), {} project(s), {} error(s), {} warning(s)",
                    summary.tasks, summary.projects, summary.errors, summary.warnings
                )
                .is_err()
                {
                    return;
                }
            }
            for issue in error.issues() {
                if writeln!(writer, "  {}", human_issue(issue)).is_err() {
                    return;
                }
                if let Some(suggestion) = &issue.suggestion {
                    if writeln!(writer, "    fix: {suggestion}").is_err() {
                        return;
                    }
                }
            }
            Ok(())
        }
    };
    let _ = result;
}
#[cfg(all(test, unix))]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    use obsidian_todo::error::ValidationIssue;

    use super::*;

    #[test]
    fn json_errors_render_non_utf8_paths_without_panicking() {
        let path = PathBuf::from(OsString::from_vec(vec![b'b', b'a', b'd', 0xff]));
        let serialized_issue =
            serde_json::to_value(ValidationIssue::error("bad_path", "bad path").at_path(&path))
                .expect("serialize validation issue");
        assert!(serialized_issue["path"]
            .as_str()
            .is_some_and(|value| value.starts_with("bad")));
        for error in [
            Error::validation("bad_path", "bad path").with_path(&path),
            Error::from_issues(vec![
                ValidationIssue::error("bad_path", "bad path").at_path(&path)
            ]),
        ] {
            let mut output = Vec::new();
            write_error(&error, OutputFormat::Json, &mut output);
            let value: Value = serde_json::from_slice(&output).expect("valid JSON error");
            let rendered = if error.issues().is_empty() {
                &value["error"]["path"]
            } else {
                &value["error"]["issues"][0]["path"]
            };
            assert!(rendered
                .as_str()
                .is_some_and(|value| value.starts_with("bad")));
        }
    }
    #[test]
    fn human_issues_include_available_location_and_field() {
        let issue = ValidationIssue::error("invalid_yaml", "bad value")
            .at_path("Tasks/task.md")
            .at_field("name")
            .at_location(3, 7);
        assert_eq!(
            human_issue(&issue),
            "error[invalid_yaml] Tasks/task.md:3:7 [name]: bad value"
        );
    }
}
