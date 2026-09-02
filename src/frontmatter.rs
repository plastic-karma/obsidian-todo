use std::fmt::Write as _;
use std::path::Path;

use serde_yaml_ng::{Mapping, Value};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{date_from_yaml, validate_name, validate_project_slug, Project, Task};
use crate::recurrence::{RecurrenceMode, RecurrenceRule};

pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_YAML_DEPTH: usize = 64;
const MAX_YAML_NODES: usize = 100_000;
const CORE_TASK_KEYS: [&str; 8] = [
    "name",
    "state",
    "projects",
    "tags",
    "due_date",
    "recurrence",
    "recurrence_from",
    "last_completed_date",
];

#[derive(Debug, Clone, PartialEq)]
pub struct FrontMatterDocument {
    pub properties: Mapping,
    pub body: String,
}

pub fn parse_document(bytes: &[u8]) -> Result<FrontMatterDocument> {
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::validation(
            "record_too_large",
            format!("Markdown record exceeds the {MAX_RECORD_BYTES}-byte limit"),
        ));
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Err(Error::validation(
            "utf8_bom_not_allowed",
            "Front matter must begin at the first byte; a UTF-8 BOM is not allowed",
        ));
    }
    let source = std::str::from_utf8(bytes).map_err(|source| {
        Error::validation(
            "invalid_utf8",
            format!("Markdown records must be UTF-8: {source}"),
        )
    })?;
    reject_conflict_markers(source)?;
    let opening_end = first_line_end(bytes).ok_or_else(|| {
        Error::validation(
            "missing_frontmatter",
            "Markdown record must begin with a front-matter delimiter",
        )
    })?;
    if !matches!(&bytes[..opening_end], b"---\n" | b"---\r\n") {
        return Err(Error::validation(
            "missing_frontmatter",
            "Front matter must begin with '---' at byte zero",
        ));
    }

    let mut line_start = opening_end;
    let mut closing_start = None;
    let mut body_start = None;
    while line_start < bytes.len() {
        let Some(relative_end) = bytes[line_start..].iter().position(|byte| *byte == b'\n') else {
            break;
        };
        let line_end = line_start + relative_end + 1;
        let line = &bytes[line_start..line_end];
        if matches!(line, b"---\n" | b"---\r\n") {
            closing_start = Some(line_start);
            body_start = Some(line_end);
            break;
        }
        line_start = line_end;
    }
    let (closing_start, body_start) = closing_start.zip(body_start).ok_or_else(|| {
        Error::validation(
            "missing_frontmatter_delimiter",
            "Front matter requires a closing '---' line terminated by a newline",
        )
    })?;

    let yaml = std::str::from_utf8(&bytes[opening_end..closing_start]).map_err(|source| {
        Error::validation(
            "invalid_utf8",
            format!("Front matter must be UTF-8: {source}"),
        )
    })?;
    reject_unsafe_yaml_tokens(yaml)?;
    let properties: Mapping = serde_yaml_ng::from_str(yaml).map_err(|source| {
        let mut error = Error::validation(
            if source.to_string().contains("duplicate entry") {
                "duplicate_yaml_key"
            } else {
                "invalid_yaml_syntax"
            },
            format!("Invalid YAML front matter: {source}"),
        );
        if let Some(location) = source.location() {
            error = error.with_location(location.line(), location.column());
        }
        error
    })?;
    validate_yaml_value(&Value::Mapping(properties.clone()), 0, &mut 0)?;
    Ok(FrontMatterDocument {
        properties,
        body: source[body_start..].to_owned(),
    })
}

pub fn parse_task(id: &str, path: &Path, bytes: &[u8], config: &Config) -> Result<Task> {
    let document = parse_document(bytes)?;
    let mut properties = document.properties;
    reject_frontmatter_id(&properties)?;
    let name = take_required_string(&mut properties, "name")?;
    let state = take_required_string(&mut properties, "state")?;
    let project_links = take_required_string_list(&mut properties, "projects")?;
    let tags = take_required_string_list(&mut properties, "tags")?;
    let due_date = take_optional(&mut properties, "due_date")
        .as_ref()
        .map(|value| date_from_yaml(value, "due_date"))
        .transpose()?;
    let recurrence = take_optional(&mut properties, "recurrence")
        .map(|value| value_as_string(value, "recurrence"))
        .transpose()?
        .map(|value| RecurrenceRule::parse(&value))
        .transpose()?;
    let recurrence_from = take_optional(&mut properties, "recurrence_from")
        .map(|value| value_as_string(value, "recurrence_from"))
        .transpose()?
        .map(|value| RecurrenceMode::parse(&value))
        .transpose()?;
    let last_completed_date = take_optional(&mut properties, "last_completed_date")
        .as_ref()
        .map(|value| date_from_yaml(value, "last_completed_date"))
        .transpose()?;
    let projects = project_links
        .iter()
        .map(|link| parse_project_link(link, config))
        .collect::<Result<Vec<_>>>()?;

    let task = Task {
        id: id.to_ascii_uppercase(),
        path: path.to_path_buf(),
        name,
        state,
        projects,
        tags,
        due_date,
        recurrence,
        recurrence_from,
        last_completed_date,
        body: document.body,
        extra_properties: properties,
    };
    task.validate(config)?;
    Ok(task)
}

pub fn parse_project(slug: &str, path: &Path, bytes: &[u8]) -> Result<Project> {
    validate_project_slug(slug)?;
    let document = parse_document(bytes)?;
    let mut properties = document.properties;
    reject_frontmatter_id(&properties)?;
    let name = take_required_string(&mut properties, "name")?;
    validate_name(&name, "name")?;
    Ok(Project {
        slug: slug.to_owned(),
        path: path.to_path_buf(),
        name,
        body: document.body,
        extra_properties: properties,
    })
}

pub fn serialize_task(task: &Task, config: &Config) -> Result<Vec<u8>> {
    task.validate(config)?;
    reject_reserved_extras(&task.extra_properties, &CORE_TASK_KEYS)?;
    let mut projects = task.projects.clone();
    projects.sort();
    let mut tags = task.tags.clone();
    tags.sort();

    let mut output = String::with_capacity(task.body.len().saturating_add(512));
    output.push_str("---\nname: ");
    output.push_str(&quoted(&task.name)?);
    output.push_str("\nstate: ");
    output.push_str(&task.state);
    output.push_str("\nprojects:");
    write_string_list(
        &mut output,
        projects.iter().map(|slug| config.project_link(slug)),
    )?;
    output.push_str("tags:");
    write_string_list(&mut output, tags.iter().cloned())?;
    if let Some(date) = task.due_date {
        writeln!(output, "due_date: {}", date.format("%Y-%m-%d")).map_err(fmt_error)?;
    }
    if let Some(rule) = &task.recurrence {
        writeln!(output, "recurrence: {}", quoted(&rule.to_string())?).map_err(fmt_error)?;
    }
    if let Some(mode) = task.recurrence_from {
        writeln!(output, "recurrence_from: {}", mode.as_str()).map_err(fmt_error)?;
    }
    if let Some(date) = task.last_completed_date {
        writeln!(output, "last_completed_date: {}", date.format("%Y-%m-%d")).map_err(fmt_error)?;
    }
    serialize_extras(&mut output, &task.extra_properties)?;
    output.push_str("---\n");
    output.push_str(&task.body);
    Ok(output.into_bytes())
}

pub fn serialize_project(project: &Project) -> Result<Vec<u8>> {
    validate_project_slug(&project.slug)?;
    validate_name(&project.name, "name")?;
    reject_reserved_extras(&project.extra_properties, &["name"])?;
    let mut output = String::with_capacity(project.body.len().saturating_add(128));
    output.push_str("---\nname: ");
    output.push_str(&quoted(&project.name)?);
    output.push('\n');
    serialize_extras(&mut output, &project.extra_properties)?;
    output.push_str("---\n");
    output.push_str(&project.body);
    Ok(output.into_bytes())
}

fn first_line_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|index| index + 1)
}

fn reject_conflict_markers(source: &str) -> Result<()> {
    for (index, line) in source.lines().enumerate() {
        if ["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
            .iter()
            .any(|marker| line.starts_with(marker))
        {
            return Err(Error::new(
                crate::error::ErrorKind::Concurrent,
                "unresolved_conflict",
                "Markdown record contains an unresolved conflict marker",
            )
            .with_location(index + 1, 1));
        }
    }
    Ok(())
}

fn reject_unsafe_yaml_tokens(source: &str) -> Result<()> {
    for (line_index, line) in source.lines().enumerate() {
        let mut single_quoted = false;
        let mut double_quoted = false;
        let mut escaped = false;
        let mut token_start = true;
        for (column, character) in line.char_indices() {
            if double_quoted {
                if escaped {
                    escaped = false;
                } else if character == '\\' {
                    escaped = true;
                } else if character == '"' {
                    double_quoted = false;
                }
                continue;
            }
            if single_quoted {
                if character == '\'' {
                    single_quoted = false;
                }
                continue;
            }
            match character {
                '#' if token_start => break,
                '"' => double_quoted = true,
                '\'' => single_quoted = true,
                '&' | '*' | '!' if token_start => {
                    return Err(Error::validation(
                        "unsafe_yaml_construct",
                        "YAML tags, anchors, and aliases are not supported",
                    )
                    .with_location(line_index + 1, column + 1));
                }
                _ => {}
            }
            token_start = character.is_whitespace()
                || matches!(character, ':' | ',' | '[' | ']' | '{' | '}' | '-');
        }
    }
    Ok(())
}

fn validate_yaml_value(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_YAML_NODES {
        return Err(Error::validation(
            "yaml_too_complex",
            "YAML front matter contains too many values",
        ));
    }
    if depth > MAX_YAML_DEPTH {
        return Err(Error::validation(
            "yaml_too_deep",
            "YAML front matter exceeds the supported nesting depth",
        ));
    }
    match value {
        Value::Tagged(_) => Err(Error::validation(
            "unsafe_yaml_construct",
            "YAML custom tags are not supported",
        )),
        Value::Sequence(values) => {
            for value in values {
                validate_yaml_value(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Value::Mapping(values) => {
            for (key, value) in values {
                let Some(key) = key.as_str() else {
                    return Err(Error::validation(
                        "unsupported_yaml_key",
                        "YAML mapping keys must be strings",
                    ));
                };
                if key == "<<" {
                    return Err(Error::validation(
                        "unsafe_yaml_construct",
                        "YAML merge keys are not supported",
                    ));
                }
                validate_yaml_value(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}

fn take_required_string(properties: &mut Mapping, key: &str) -> Result<String> {
    let value = properties.shift_remove(key).ok_or_else(|| {
        Error::validation(
            "missing_property",
            format!("Required property {key:?} is missing"),
        )
        .with_field(key)
    })?;
    value_as_string(value, key)
}

fn value_as_string(value: Value, key: &str) -> Result<String> {
    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
        Error::validation(
            "invalid_property_type",
            format!("Property {key:?} must be a string"),
        )
        .with_field(key)
    })
}

fn take_required_string_list(properties: &mut Mapping, key: &str) -> Result<Vec<String>> {
    let value = properties.shift_remove(key).ok_or_else(|| {
        Error::validation(
            "missing_property",
            format!("Required property {key:?} is missing"),
        )
        .with_field(key)
    })?;
    let values = value.as_sequence().ok_or_else(|| {
        Error::validation(
            "invalid_property_type",
            format!("Property {key:?} must be a list"),
        )
        .with_field(key)
    })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                Error::validation(
                    "invalid_property_type",
                    format!("Every item in {key:?} must be a string"),
                )
                .with_field(key)
            })
        })
        .collect()
}

fn take_optional(properties: &mut Mapping, key: &str) -> Option<Value> {
    properties.shift_remove(key)
}

fn parse_project_link(value: &str, config: &Config) -> Result<String> {
    let prefix = config.project_link_target_prefix();
    let target = value
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
        .and_then(|value| value.strip_prefix(&prefix))
        .ok_or_else(|| invalid_project_link(value, config))?;
    if target.contains(['/', '|', '#', '^']) || target.ends_with(".md") {
        return Err(invalid_project_link(value, config));
    }
    validate_project_slug(target).map_err(|_| invalid_project_link(value, config))?;
    if config.project_link(target) != value {
        return Err(invalid_project_link(value, config));
    }
    Ok(target.to_owned())
}

fn invalid_project_link(value: &str, config: &Config) -> Error {
    Error::validation(
        "invalid_project_link",
        format!(
            "Project reference {value:?} must be an unaliased wikilink under {}",
            config.project_link_target_prefix()
        ),
    )
    .with_field("projects")
}

fn reject_frontmatter_id(properties: &Mapping) -> Result<()> {
    if properties.contains_key("id") {
        return Err(Error::validation(
            "redundant_id_property",
            "Record identity belongs only in its filename; remove the id property",
        )
        .with_field("id"));
    }
    Ok(())
}

fn reject_reserved_extras(properties: &Mapping, reserved: &[&str]) -> Result<()> {
    for key in reserved.iter().chain(std::iter::once(&"id")) {
        if properties.contains_key(*key) {
            return Err(Error::validation(
                "reserved_extra_property",
                format!("Extra properties contain reserved key {key:?}"),
            )
            .with_field(*key));
        }
    }
    validate_yaml_value(&Value::Mapping(properties.clone()), 0, &mut 0)
}

fn quoted(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|source| {
        Error::validation(
            "yaml_serialization_failed",
            format!("Could not quote a YAML string: {source}"),
        )
    })
}

fn write_string_list(
    output: &mut String,
    values: impl IntoIterator<Item = String>,
) -> Result<()> {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        output.push_str(" []\n");
        return Ok(());
    }
    output.push('\n');
    for value in values {
        let rendered = quoted(&value)?;
        writeln!(output, "  - {rendered}").map_err(fmt_error)?;
    }
    Ok(())
}

fn serialize_extras(output: &mut String, properties: &Mapping) -> Result<()> {
    for (key, value) in properties {
        let mut one = Mapping::new();
        one.insert(key.clone(), value.clone());
        let serialized = serde_yaml_ng::to_string(&one).map_err(|source| {
            Error::validation(
                "yaml_serialization_failed",
                format!("Could not preserve an unknown property: {source}"),
            )
        })?;
        output.push_str(&serialized);
    }
    Ok(())
}

fn fmt_error(_: std::fmt::Error) -> Error {
    Error::validation(
        "record_serialization_failed",
        "Could not serialize Markdown record",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01K4B0ZSBZZV25T1K0D3TA8JHR";

    fn config() -> Config {
        Config::defaults("Todo".to_owned())
    }

    fn task(source: &str) -> Task {
        parse_task(
            ID,
            Path::new("Tasks/example.md"),
            source.as_bytes(),
            &config(),
        )
        .expect("valid task")
    }

    #[test]
    fn accepts_lf_and_crlf_delimiters_and_rejects_malformed_forms() {
        assert_eq!(
            parse_document(b"---\r\nname: test\r\n---\r\nbody\r\n")
                .expect("CRLF")
                .body,
            "body\r\n"
        );
        for malformed in [
            b"name: test\n---\n".as_slice(),
            b"\n---\nname: test\n---\n".as_slice(),
            b"---\nname: test\n".as_slice(),
            b"---\nname: test\n---".as_slice(),
            b"\xef\xbb\xbf---\nname: test\n---\n".as_slice(),
        ] {
            assert!(parse_document(malformed).is_err());
        }
    }

    #[test]
    fn duplicate_keys_are_rejected_at_every_depth() {
        for source in [
            "---\nname: one\nname: two\n---\n",
            "---\nplugin:\n  nested: one\n  nested: two\n---\n",
        ] {
            let error = parse_document(source.as_bytes()).expect_err("duplicate");
            assert_eq!(error.code(), "duplicate_yaml_key");
        }
    }

    #[test]
    fn tags_anchors_aliases_and_merge_keys_are_rejected() {
        for source in [
            "---\nplugin: !thing value\n---\n",
            "---\nplugin: &anchor value\n---\n",
            "---\nplugin: *anchor\n---\n",
            "---\n'<<': { key: value }\n---\n",
        ] {
            assert!(parse_document(source.as_bytes()).is_err(), "{source}");
        }
    }

    #[test]
    fn metadata_edit_preserves_body_and_unknown_values() {
        let source = "---\nname: Original\nstate: open\nprojects: []\ntags: []\nplugin-map:\n  enabled: true\n  values: [1, two, null]\nplugin-scalar: 42\n---\nBody with --- inside.\r\n```yaml\r\nstate: fake\r\n```\r\n";
        let mut parsed = task(source);
        let original_extras = parsed.extra_properties.clone();
        let original_body = parsed.body.clone();
        parsed.state = "active".to_owned();
        let serialized = serialize_task(&parsed, &config()).expect("serialize");
        let reparsed =
            parse_task(ID, Path::new("Tasks/example.md"), &serialized, &config()).expect("reparse");
        assert_eq!(reparsed.extra_properties, original_extras);
        assert_eq!(reparsed.body, original_body);
        assert!(String::from_utf8(serialized)
            .expect("UTF-8")
            .starts_with("---\n"));
    }

    #[test]
    fn writer_orders_core_fields_and_quotes_links_and_recurrence() {
        let source = "---\ntags: [review]\nprojects: [\"[[Todo/Projects/work]]\"]\nstate: open\nname: Review\ndue_date: 2026-09-07\nrecurrence_from: schedule\nrecurrence: FREQ=WEEKLY;BYDAY=MO\n---\nbody\n";
        let serialized =
            String::from_utf8(serialize_task(&task(source), &config()).expect("write"))
                .expect("UTF-8");
        assert!(serialized.starts_with(
            "---\nname: \"Review\"\nstate: open\nprojects:\n  - \"[[Todo/Projects/work]]\"\ntags:\n  - \"review\"\ndue_date: 2026-09-07\nrecurrence: \"FREQ=WEEKLY;INTERVAL=1;BYDAY=MO\"\nrecurrence_from: schedule\n"
        ));
        assert!(!serialized.contains("\nid:"));
        assert!(!serialized.contains("# Review"));
    }

    #[test]
    fn project_round_trip_preserves_unknown_properties() {
        let source = b"---\nname: Work\nstatus: planning\nplugin: {color: blue}\n---\n# Notes\n";
        let project = parse_project("work", Path::new("Projects/work.md"), source).expect("parse");
        let output = serialize_project(&project).expect("serialize");
        let reparsed =
            parse_project("work", Path::new("Projects/work.md"), &output).expect("reparse");
        assert_eq!(reparsed.extra_properties, project.extra_properties);
        assert_eq!(reparsed.body, "# Notes\n");
    }
}
