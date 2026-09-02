use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Usage,
    NotFound,
    Ambiguous,
    Validation,
    Concurrent,
    Unsupported,
    Io,
}

impl ErrorKind {
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Usage => 2,
            Self::NotFound => 3,
            Self::Ambiguous => 4,
            Self::Validation => 5,
            Self::Concurrent => 6,
            Self::Unsupported => 7,
            Self::Io => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationIssue {
    pub code: String,
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub field: Option<String>,
    pub message: String,
    pub suggestion: Option<String>,
    pub severity: IssueSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueSeverity {
    Error,
    Warning,
}

impl ValidationIssue {
    #[must_use]
    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: None,
            line: None,
            column: None,
            field: None,
            message: message.into(),
            suggestion: None,
            severity: IssueSeverity::Error,
        }
    }

    #[must_use]
    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity: IssueSeverity::Warning,
            ..Self::error(code, message)
        }
    }

    #[must_use]
    pub fn at_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    #[must_use]
    pub fn at_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }

    #[must_use]
    pub const fn at_location(mut self, line: usize, column: usize) -> Self {
        self.line = Some(line);
        self.column = Some(column);
        self
    }

    #[must_use]
    pub fn with_suggestion(mut self, suggestion: impl Into<String>) -> Self {
        self.suggestion = Some(suggestion.into());
        self
    }
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    code: &'static str,
    details: Box<ErrorDetails>,
}

#[derive(Debug)]
struct ErrorDetails {
    message: String,
    path: Option<PathBuf>,
    field: Option<String>,
    line: Option<usize>,
    column: Option<usize>,
    issues: Vec<ValidationIssue>,
}

impl Error {
    #[must_use]
    pub fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            details: Box::new(ErrorDetails {
                message: message.into(),
                path: None,
                field: None,
                line: None,
                column: None,
                issues: Vec::new(),
            }),
        }
    }

    #[must_use]
    pub fn usage(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Usage, code, message)
    }

    #[must_use]
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    #[must_use]
    pub fn validation(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Validation, code, message)
    }

    #[must_use]
    pub fn unsupported(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, code, message)
    }

    #[must_use]
    pub fn io(action: &str, path: &Path, source: &std::io::Error) -> Self {
        Self::new(
            ErrorKind::Io,
            "io_error",
            format!("Could not {action}: {source}"),
        )
        .with_path(path)
    }

    #[must_use]
    pub fn from_issues(mut issues: Vec<ValidationIssue>) -> Self {
        issues.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.field.cmp(&right.field))
                .then_with(|| left.line.cmp(&right.line))
                .then_with(|| left.column.cmp(&right.column))
                .then_with(|| left.code.cmp(&right.code))
        });
        let error_count = issues
            .iter()
            .filter(|issue| issue.severity == IssueSeverity::Error)
            .count();
        let warning_count = issues.len().saturating_sub(error_count);
        let message = match (error_count, warning_count) {
            (0, warnings) => format!("Validation completed with {warnings} warning(s)"),
            (errors, 0) => format!("Validation failed with {errors} error(s)"),
            (errors, warnings) => {
                format!("Validation failed with {errors} error(s) and {warnings} warning(s)")
            }
        };
        let kind = if issues
            .iter()
            .any(|issue| issue.code == "unsupported_schema")
        {
            ErrorKind::Unsupported
        } else {
            ErrorKind::Validation
        };
        let mut error = Self::new(kind, "validation_failed", message);
        error.details.issues = issues;
        error
    }

    #[must_use]
    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.details.path = Some(path.into());
        self
    }

    #[must_use]
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.details.field = Some(field.into());
        self
    }

    #[must_use]
    pub fn with_location(mut self, line: usize, column: usize) -> Self {
        self.details.line = Some(line);
        self.details.column = Some(column);
        self
    }

    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.details.message
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.details.path.as_deref()
    }

    #[must_use]
    pub fn field(&self) -> Option<&str> {
        self.details.field.as_deref()
    }

    #[must_use]
    pub const fn line(&self) -> Option<usize> {
        self.details.line
    }

    #[must_use]
    pub const fn column(&self) -> Option<usize> {
        self.details.column
    }

    #[must_use]
    pub fn issues(&self) -> &[ValidationIssue] {
        &self.details.issues
    }

    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        self.kind.exit_code()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.details.path {
            write!(formatter, "{}: ", path.display())?;
        }
        write!(formatter, "{}", self.details.message)
    }
}

impl std::error::Error for Error {}
