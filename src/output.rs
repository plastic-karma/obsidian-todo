use std::io::Write;

use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{Error, IssueSeverity, Result};

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

pub fn write_success(
    output: &CommandOutput,
    format: OutputFormat,
    writer: &mut impl Write,
) -> Result<()> {
    match format {
        OutputFormat::Human => writeln!(writer, "{}", output.human).map_err(|source| {
            Error::new(
                crate::error::ErrorKind::Io,
                "io_error",
                format!("Could not write output: {source}"),
            )
        }),
        OutputFormat::Json => {
            serde_json::to_writer(&mut *writer, &output.json).map_err(|source| {
                Error::new(
                    crate::error::ErrorKind::Io,
                    "io_error",
                    format!("Could not write JSON output: {source}"),
                )
            })?;
            writeln!(writer).map_err(|source| {
                Error::new(
                    crate::error::ErrorKind::Io,
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
                        "path": issue.path,
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
                    "path": error.path(),
                    "field": error.field(),
                    "line": error.line(),
                    "column": error.column(),
                    "issues": issues,
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
            for issue in error.issues() {
                let severity = match issue.severity {
                    IssueSeverity::Error => "error",
                    IssueSeverity::Warning => "warning",
                };
                let path = issue
                    .path
                    .as_deref()
                    .map_or_else(|| "<store>".to_owned(), |path| path.display().to_string());
                let field = issue
                    .field
                    .as_deref()
                    .map_or_else(String::new, |field| format!(" [{field}]"));
                if writeln!(
                    writer,
                    "  {severity}[{}] {path}{field}: {}",
                    issue.code, issue.message
                )
                .is_err()
                {
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

#[derive(Serialize)]
pub struct Versioned<T: Serialize> {
    pub version: u8,
    #[serde(flatten)]
    pub value: T,
}
