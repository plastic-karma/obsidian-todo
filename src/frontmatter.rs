use std::fmt::Write as _;
use std::path::Path;

use serde_yaml_ng::{Mapping, Value};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::{
    date_from_yaml, normalize_parent_id, validate_name, validate_project_slug, Project, Task,
};
use crate::recurrence::{RecurrenceMode, RecurrenceRule};

pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_YAML_DEPTH: usize = 64;
const MAX_YAML_NODES: usize = 100_000;
const CORE_TASK_KEYS: [&str; 9] = [
    "name",
    "state",
    "projects",
    "tags",
    "due_date",
    "recurrence",
    "recurrence_from",
    "last_completed_date",
    "parent",
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

    let yaml = &source[opening_end..closing_start];
    reject_unsafe_yaml_tokens(yaml)?;
    let value: Value = serde_yaml_ng::from_str(yaml).map_err(|source| {
        let message = source.to_string();
        let mut error = Error::validation(
            if message.contains("duplicate entry") {
                "duplicate_yaml_key"
            } else {
                "invalid_yaml_syntax"
            },
            format!("Invalid YAML front matter: {message}"),
        );
        if let Some(location) = source.location() {
            error = error.with_location(location.line().saturating_add(1), location.column());
        }
        error
    })?;
    let properties = match value {
        Value::Mapping(properties) => properties,
        Value::Null
            if yaml
                .lines()
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')) =>
        {
            Mapping::new()
        }
        _ => {
            return Err(Error::validation(
                "invalid_frontmatter_type",
                "YAML front matter must be a property mapping",
            )
            .with_location(2, 1));
        }
    };
    validate_yaml_mapping(&properties, 0, &mut 0)?;
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
    let parent = if config.schema_version == 2 {
        let parent = parent_from_properties(&properties)?;
        properties.shift_remove("parent");
        parent
    } else {
        None
    };
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
        parent,
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

pub(crate) fn parent_from_properties(properties: &Mapping) -> Result<Option<String>> {
    properties
        .get("parent")
        .map(|value| match value {
            Value::String(value) => normalize_parent_id(value),
            _ => Err(Error::validation(
                "invalid_parent_id",
                "Parent must be a string containing a full valid ULID",
            )
            .with_field("parent")),
        })
        .transpose()
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
    reject_conflict_markers(&task.body)?;
    task.validate(config)?;
    let reserved = if config.schema_version == 2 {
        &CORE_TASK_KEYS[..]
    } else {
        &CORE_TASK_KEYS[..CORE_TASK_KEYS.len() - 1]
    };
    reject_reserved_extras(&task.extra_properties, reserved)?;
    let mut projects = task.projects.iter().map(String::as_str).collect::<Vec<_>>();
    projects.sort_unstable();
    let mut tags = task.tags.iter().map(String::as_str).collect::<Vec<_>>();
    tags.sort_unstable();

    let mut output = String::with_capacity(task.body.len().saturating_add(512));
    output.push_str("---\nname: ");
    output.push_str(&quoted(&task.name)?);
    output.push_str("\nstate: ");
    write_state(&mut output, &task.state)?;
    output.push_str("\nprojects:");
    write_string_list(
        &mut output,
        projects.iter().map(|slug| config.project_link(slug)),
    )?;
    output.push_str("tags:");
    write_string_list(&mut output, tags)?;
    if let Some(parent) = &task.parent {
        writeln!(output, "parent: {}", quoted(&parent.to_ascii_uppercase())?).map_err(fmt_error)?;
    }
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
    finish_record(output, &task.path)
}

pub fn serialize_project(project: &Project) -> Result<Vec<u8>> {
    validate_project_slug(&project.slug)?;
    reject_conflict_markers(&project.body)?;
    validate_name(&project.name, "name")?;
    reject_reserved_extras(&project.extra_properties, &["name"])?;
    let mut output = String::with_capacity(project.body.len().saturating_add(128));
    output.push_str("---\nname: ");
    output.push_str(&quoted(&project.name)?);
    output.push('\n');
    serialize_extras(&mut output, &project.extra_properties)?;
    output.push_str("---\n");
    output.push_str(&project.body);
    finish_record(output, &project.path)
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
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut escaped = false;
    let mut block_scalar_indent = None;
    let mut plain_scalar_indent = None;
    let mut flow_depth = 0_usize;
    let mut block_indents = Vec::with_capacity(MAX_YAML_DEPTH);

    for (line_index, line) in source.lines().enumerate() {
        let quoted_continuation = single_quoted || double_quoted;
        let indentation = line
            .chars()
            .take_while(|character| *character == ' ')
            .count();
        if let Some(parent_indent) = block_scalar_indent {
            if line.trim().is_empty() || indentation > parent_indent {
                continue;
            }
            block_scalar_indent = None;
        }
        let trimmed = line.trim_start();
        let structural_indicator =
            matches!(trimmed, "-" | "?") || trimmed.starts_with("- ") || trimmed.starts_with("? ");
        let plain_continuation = if let Some(parent_indent) = plain_scalar_indent {
            if !structural_indicator && (line.trim().is_empty() || indentation > parent_indent) {
                true
            } else {
                plain_scalar_indent = None;
                false
            }
        } else {
            false
        };
        let starts_in_flow = flow_depth > 0;
        if !quoted_continuation
            && !plain_continuation
            && !starts_in_flow
            && !line.trim().is_empty()
            && !line.trim_start().starts_with('#')
        {
            record_yaml_indent(&mut block_indents, indentation, line_index)?;
        }

        let mut token_start = true;
        let mut value_position = false;
        let mut compact_depth = 0_usize;
        let mut plain_scalar = plain_continuation;
        let mut structural_line = false;
        for (column, (byte_index, character)) in line.char_indices().enumerate() {
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
                '"' if !plain_scalar => {
                    double_quoted = true;
                    value_position = false;
                }
                '\'' if !plain_scalar => {
                    single_quoted = true;
                    value_position = false;
                }
                '&' | '*' | '!' if token_start && !plain_scalar => {
                    return Err(Error::validation(
                        "unsafe_yaml_construct",
                        "YAML tags, anchors, and aliases are not supported",
                    )
                    .with_location(line_index + 2, column + 1));
                }
                ':' => {
                    let next = line[byte_index + character.len_utf8()..].chars().next();
                    let separates_value = next.is_none_or(|next| {
                        next.is_whitespace()
                            || (flow_depth > 0 && matches!(next, ',' | '[' | ']' | '{' | '}'))
                    });
                    if separates_value {
                        structural_line = true;
                        plain_scalar = false;
                        value_position = true;
                    }
                }
                '?' if token_start && !plain_scalar => {
                    value_position = line[byte_index + character.len_utf8()..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace);
                    if value_position {
                        compact_depth = compact_depth.saturating_add(1);
                        if compact_depth > MAX_YAML_DEPTH {
                            return Err(Error::validation(
                                "yaml_too_deep",
                                "YAML front matter exceeds the supported nesting depth",
                            )
                            .with_location(line_index + 2, column + 1));
                        }
                    } else {
                        plain_scalar = true;
                    }
                }
                '-' if token_start && !plain_scalar => {
                    value_position = line[byte_index + character.len_utf8()..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace);
                    if value_position {
                        compact_depth = compact_depth.saturating_add(1);
                        if compact_depth > MAX_YAML_DEPTH {
                            return Err(Error::validation(
                                "yaml_too_deep",
                                "YAML front matter exceeds the supported nesting depth",
                            )
                            .with_location(line_index + 2, column + 1));
                        }
                    } else {
                        plain_scalar = true;
                    }
                }
                '|' | '>'
                    if value_position
                        && is_block_scalar_suffix(&line[byte_index + character.len_utf8()..]) =>
                {
                    block_scalar_indent = Some(indentation);
                    break;
                }
                '[' | '{' if !plain_scalar => {
                    flow_depth = flow_depth.saturating_add(1);
                    if flow_depth > MAX_YAML_DEPTH {
                        return Err(Error::validation(
                            "yaml_too_deep",
                            "YAML front matter exceeds the supported nesting depth",
                        )
                        .with_location(line_index + 2, column + 1));
                    }
                    plain_scalar = false;
                    value_position = true;
                }
                ',' if flow_depth > 0 => {
                    plain_scalar = false;
                    value_position = true;
                }
                ']' | '}' if flow_depth > 0 => {
                    flow_depth -= 1;
                    plain_scalar = false;
                    value_position = false;
                }
                character if !character.is_whitespace() => {
                    plain_scalar = true;
                    value_position = false;
                }
                _ => {}
            }
            token_start = character.is_whitespace()
                || matches!(character, ':' | ',' | '[' | ']' | '{' | '}' | '-' | '?');
        }
        if plain_continuation && structural_line && !starts_in_flow {
            record_yaml_indent(&mut block_indents, indentation, line_index)?;
        }
        if plain_scalar && (!plain_continuation || structural_line) {
            plain_scalar_indent = Some(indentation);
        } else if structural_line {
            plain_scalar_indent = None;
        }
        escaped = false;
    }
    Ok(())
}

fn record_yaml_indent(
    block_indents: &mut Vec<usize>,
    indentation: usize,
    line_index: usize,
) -> Result<()> {
    while block_indents
        .last()
        .is_some_and(|parent| indentation <= *parent)
    {
        block_indents.pop();
    }
    block_indents.push(indentation);
    if block_indents.len() > MAX_YAML_DEPTH {
        return Err(Error::validation(
            "yaml_too_deep",
            "YAML front matter exceeds the supported nesting depth",
        )
        .with_location(line_index + 2, indentation + 1));
    }
    Ok(())
}

fn is_block_scalar_suffix(suffix: &str) -> bool {
    let suffix = suffix.trim();
    let modifiers = if let Some(comment) = suffix.find('#') {
        if comment != 0
            && !suffix[..comment]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace)
        {
            return false;
        }
        suffix[..comment].trim()
    } else {
        suffix
    };
    let mut indentation = false;
    let mut chomping = false;
    for modifier in modifiers.chars() {
        match modifier {
            '1'..='9' if !indentation => indentation = true,
            '+' | '-' if !chomping => chomping = true,
            _ => return false,
        }
    }
    true
}

fn validate_yaml_value(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    count_yaml_node(depth, nodes)?;
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
        Value::Mapping(values) => validate_yaml_mapping_entries(values, depth, nodes),
        Value::Number(number) if number.as_f64().is_some_and(|number| !number.is_finite()) => {
            Err(Error::validation(
                "invalid_yaml_number",
                "YAML numbers must have a finite JSON representation",
            ))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => Ok(()),
    }
}

fn validate_yaml_mapping(values: &Mapping, depth: usize, nodes: &mut usize) -> Result<()> {
    count_yaml_node(depth, nodes)?;
    validate_yaml_mapping_entries(values, depth, nodes)
}

fn validate_yaml_mapping_entries(values: &Mapping, depth: usize, nodes: &mut usize) -> Result<()> {
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

fn count_yaml_node(depth: usize, nodes: &mut usize) -> Result<()> {
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
    Ok(())
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
    validate_yaml_mapping(properties, 0, &mut 0)
}
fn write_state(output: &mut String, state: &str) -> Result<()> {
    const YAML_KEYWORDS: [&str; 9] = ["null", "true", "false", "yes", "no", "on", "off", "y", "n"];
    if state.as_bytes().first().is_some_and(u8::is_ascii_digit) || YAML_KEYWORDS.contains(&state) {
        output.push_str(&quoted(state)?);
    } else {
        output.push_str(state);
    }
    Ok(())
}

fn quoted(value: &str) -> Result<String> {
    serde_json::to_string(value).map_err(|source| {
        Error::validation(
            "yaml_serialization_failed",
            format!("Could not quote a YAML string: {source}"),
        )
    })
}

fn write_string_list<I, S>(output: &mut String, values: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut values = values.into_iter().peekable();
    if values.peek().is_none() {
        output.push_str(" []\n");
        return Ok(());
    }
    output.push('\n');
    for value in values {
        let rendered = quoted(value.as_ref())?;
        writeln!(output, "  - {rendered}").map_err(fmt_error)?;
    }
    Ok(())
}

fn serialize_extras(output: &mut String, properties: &Mapping) -> Result<()> {
    if properties.is_empty() {
        return Ok(());
    }
    let serialized = serde_yaml_ng::to_string(properties).map_err(|source| {
        Error::validation(
            "yaml_serialization_failed",
            format!("Could not preserve unknown properties: {source}"),
        )
    })?;
    output.push_str(&serialized);
    Ok(())
}

fn finish_record(output: String, path: &Path) -> Result<Vec<u8>> {
    if output.len() > MAX_RECORD_BYTES {
        return Err(Error::validation(
            "record_too_large",
            format!("Markdown record exceeds the {MAX_RECORD_BYTES}-byte limit"),
        )
        .with_path(path));
    }
    Ok(output.into_bytes())
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
    fn distinguishes_yaml_syntax_from_frontmatter_type_errors() {
        assert!(parse_document(b"---\n# empty properties\n---\n")
            .expect("empty mapping")
            .properties
            .is_empty());
        assert_eq!(
            parse_document(b"---\n- item\n---\n")
                .expect_err("sequence root")
                .code(),
            "invalid_frontmatter_type"
        );
        assert_eq!(
            parse_document(b"---\nvalues: [\n---\n")
                .expect_err("malformed YAML")
                .code(),
            "invalid_yaml_syntax"
        );
    }

    #[test]
    fn crlf_input_serializes_lf_front_matter_and_preserves_body_bytes() {
        let parsed =
            task("---\r\nname: Task\r\nstate: open\r\nprojects: []\r\ntags: []\r\n---\r\nbody\r\n");
        let serialized = String::from_utf8(serialize_task(&parsed, &config()).expect("serialize"))
            .expect("UTF-8");
        assert_eq!(
            serialized,
            "---\nname: \"Task\"\nstate: open\nprojects: []\ntags: []\n---\nbody\r\n"
        );
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
            "---\nplugin:\n  - key: value\n    other: &anchor payload\n    alias: *anchor\n---\n",
            "---\n? &anchor key\n: value\n---\n",
            "---\n'<<': { key: value }\n---\n",
        ] {
            assert!(parse_document(source.as_bytes()).is_err(), "{source}");
        }
        let tagged =
            parse_document(b"---\nplugin: !thing value\n---\n").expect_err("custom YAML tag");
        assert_eq!((tagged.line(), tagged.column()), (Some(2), Some(9)));
        let compact = parse_document(b"---\nplugin: {key:[&anchor value]}\n---\n")
            .expect_err("compact flow anchor");
        assert_eq!(compact.code(), "unsafe_yaml_construct");
    }

    #[test]
    fn block_and_multiline_quoted_scalars_do_not_trigger_token_rejection() {
        let source = b"---\nname: Safe\nliteral: |-\n  !not-a-tag\n  &not-an-anchor\nfolded: >-\n  *not-an-alias\nquoted: \"line one\n  !still-text\"\nplain: first line\n  !still-plain-text\n---\n";
        let project =
            parse_project("safe", Path::new("Projects/safe.md"), source).expect("safe scalars");
        let serialized = serialize_project(&project).expect("serialize safe scalars");
        let reparsed = parse_project("safe", Path::new("Projects/safe.md"), &serialized)
            .expect("reparse safe scalars");
        assert_eq!(reparsed.extra_properties, project.extra_properties);
    }

    #[test]
    fn non_finite_yaml_numbers_are_rejected_for_json_compatibility() {
        for number in [".nan", ".inf", "-.inf"] {
            let source = format!("---\nname: Safe\nplugin: {number}\n---\n");
            assert_eq!(
                parse_document(source.as_bytes())
                    .expect_err("non-finite number")
                    .code(),
                "invalid_yaml_number"
            );
        }
    }

    #[test]
    fn yaml_ambiguous_state_ids_round_trip_as_strings() {
        for state in ["0", "true", "null"] {
            let mut config = config();
            config.states.push(crate::config::State {
                id: state.to_owned(),
                name: state.to_owned(),
                terminal: false,
            });
            let source = format!(
                "---\nname: Task\nstate: {}\nprojects: []\ntags: []\n---\n",
                serde_json::to_string(state).expect("quote state")
            );
            let task = parse_task(
                ID,
                Path::new("Tasks/example.md"),
                source.as_bytes(),
                &config,
            )
            .expect("parse");
            let serialized = serialize_task(&task, &config).expect("serialize");
            assert_eq!(
                parse_task(ID, Path::new("Tasks/example.md"), &serialized, &config)
                    .expect("reparse")
                    .state,
                state
            );
        }
    }

    #[test]
    fn writers_refuse_new_conflict_markers() {
        let mut parsed = task("---\nname: Task\nstate: open\nprojects: []\ntags: []\n---\n");
        parsed.body = "<<<<<<< ours\ntext\n=======\nother\n>>>>>>> theirs\n".to_owned();
        let error = serialize_task(&parsed, &config()).expect_err("conflicted body");
        assert_eq!(error.code(), "unresolved_conflict");
        assert_eq!(error.exit_code(), 6);

        for marker in ["<<<<<<<", "|||||||", "=======", ">>>>>>>"] {
            let source = format!("---\nname: Safe\n---\n{marker}\n");
            assert_eq!(
                parse_document(source.as_bytes())
                    .expect_err("conflict marker")
                    .code(),
                "unresolved_conflict"
            );
        }
    }

    #[test]
    fn yaml_depth_and_node_counts_are_bounded() {
        let deeply_nested = format!(
            "---\nplugin: {}null{}\n---\n",
            "[".repeat(MAX_YAML_DEPTH + 1),
            "]".repeat(MAX_YAML_DEPTH + 1)
        );
        assert_eq!(
            parse_document(deeply_nested.as_bytes())
                .expect_err("excessive depth")
                .code(),
            "yaml_too_deep"
        );

        let mut deeply_indented = String::from("---\n");
        for depth in 0..=MAX_YAML_DEPTH {
            deeply_indented.push_str(&"  ".repeat(depth));
            deeply_indented.push_str("key:\n");
        }
        deeply_indented.push_str("---\n");
        assert_eq!(
            parse_document(deeply_indented.as_bytes())
                .expect_err("excessive block depth")
                .code(),
            "yaml_too_deep"
        );

        let mut many_nodes = String::with_capacity(MAX_YAML_NODES * 5 + 32);
        many_nodes.push_str("---\nplugin: [");
        for index in 0..MAX_YAML_NODES {
            if index != 0 {
                many_nodes.push(',');
            }
            many_nodes.push_str("null");
        }
        many_nodes.push_str("]\n---\n");
        assert_eq!(
            parse_document(many_nodes.as_bytes())
                .expect_err("excessive nodes")
                .code(),
            "yaml_too_complex"
        );
    }
    #[test]
    fn project_links_reject_every_noncanonical_form() {
        let valid =
            "---\nname: Task\nstate: open\nprojects: [\"[[Todo/Projects/work]]\"]\ntags: []\n---\n";
        assert_eq!(task(valid).projects, ["work"]);
        for link in [
            "[[Other/Projects/work]]",
            "[[Todo/Projects/work|Work]]",
            "[[Todo/Projects/work#heading]]",
            "[[Todo/Projects/work^block]]",
            "[[Todo/Projects/work.md]]",
            "[[Todo/Projects/Work]]",
            "[[Todo/Projects/nested/work]]",
        ] {
            let source = format!(
                "---\nname: Task\nstate: open\nprojects: [{}]\ntags: []\n---\n",
                serde_json::to_string(link).expect("quote link")
            );
            let error = parse_task(
                ID,
                Path::new("Tasks/example.md"),
                source.as_bytes(),
                &config(),
            )
            .expect_err("noncanonical link");
            assert_eq!(error.code(), "invalid_project_link", "{link}");
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
    #[test]
    fn oversized_input_and_serialized_records_are_rejected() {
        let input = vec![b'x'; MAX_RECORD_BYTES + 1];
        assert_eq!(
            parse_document(&input).expect_err("oversized input").code(),
            "record_too_large"
        );

        let mut parsed = task("---\nname: Task\nstate: open\nprojects: []\ntags: []\n---\n");
        parsed.body = "x".repeat(MAX_RECORD_BYTES);
        assert_eq!(
            serialize_task(&parsed, &config())
                .expect_err("oversized output")
                .code(),
            "record_too_large"
        );
    }

    #[test]
    fn shared_parent_record_conformance_preserves_extras_and_body() {
        #[derive(serde::Deserialize)]
        struct Corpus {
            record_cases: Vec<RecordCase>,
        }
        #[derive(serde::Deserialize)]
        struct RecordCase {
            name: String,
            schema_version: u32,
            id: String,
            markdown: String,
            expected_parent: Option<String>,
            expected_error: Option<String>,
        }
        let corpus: Corpus =
            serde_json::from_str(include_str!("../tests/fixtures/subtasks.json")).expect("corpus");
        for case in corpus.record_cases {
            let mut config = config();
            config.schema_version = case.schema_version;
            let path = std::path::PathBuf::from(format!("Tasks/{}.md", case.id));
            let parsed = parse_task(&case.id, &path, case.markdown.as_bytes(), &config);
            if let Some(expected) = case.expected_error {
                let error = parsed.expect_err(&case.name);
                assert_eq!(error.code(), expected, "{}", case.name);
                assert_eq!(error.field(), Some("parent"), "{}", case.name);
                continue;
            }
            let mut task = parsed.expect(&case.name);
            assert_eq!(task.parent, case.expected_parent, "{}", case.name);
            let original = parse_document(case.markdown.as_bytes()).expect("document");
            if case.schema_version == 1 {
                assert_eq!(
                    task.extra_properties.get("parent"),
                    original.properties.get("parent"),
                    "{}",
                    case.name
                );
            }
            task.name = "Edited name".to_owned();
            let serialized = serialize_task(&task, &config).expect(&case.name);
            let reparsed = parse_task(&case.id, &path, &serialized, &config).expect(&case.name);
            assert_eq!(reparsed, task, "{}", case.name);
            assert_eq!(
                reparsed.body.as_bytes(),
                original.body.as_bytes(),
                "{}",
                case.name
            );
            if let Some(parent) = &task.parent {
                let source = std::str::from_utf8(&serialized).expect("UTF-8");
                assert!(
                    source.contains(&format!("\nparent: \"{parent}\"\n")),
                    "{}",
                    case.name
                );
                let lines = source.lines().collect::<Vec<_>>();
                let parent_line = lines
                    .iter()
                    .position(|line| line.starts_with("parent:"))
                    .expect("parent");
                assert!(lines[..parent_line]
                    .iter()
                    .any(|line| line.starts_with("tags:")));
                assert!(!lines[..parent_line]
                    .iter()
                    .any(|line| line.starts_with("due_date:")));
            }
        }
    }

    #[test]
    fn v2_reserved_parent_extra_cannot_override_typed_relationship() {
        let mut parsed = task("---\nname: Task\nstate: open\nprojects: []\ntags: []\n---\n");
        parsed
            .extra_properties
            .insert(Value::String("parent".to_owned()), Value::Null);
        let error = serialize_task(&parsed, &config()).expect_err("reserved parent");
        assert_eq!(error.code(), "reserved_extra_property");
        assert_eq!(error.field(), Some("parent"));
        let mut legacy = config();
        legacy.schema_version = 1;
        let bytes = serialize_task(&parsed, &legacy).expect("legacy metadata");
        let reparsed = parse_task(ID, &parsed.path, &bytes, &legacy).expect("legacy");
        assert_eq!(reparsed.extra_properties.get("parent"), Some(&Value::Null));
        assert_eq!(reparsed.parent, None);
        parsed.parent = Some(ID.to_owned());
        assert_eq!(
            serialize_task(&parsed, &legacy)
                .expect_err("no typed v1 relationship")
                .code(),
            "unsupported_schema"
        );
    }
}
