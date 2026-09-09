//! Attachments are ordinary files associated exclusively through Markdown body links.
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use ulid::Ulid;

use crate::config::{managed_paths_overlap, Config};
use crate::error::{Error, Result, ValidationIssue};

pub const DIRECTORY: &str = "Attachments";
pub const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct AttachmentView {
    pub path: String,
    pub display_name: String,
    pub byte_size: Option<u64>,
    pub availability: &'static str,
}

#[derive(Debug, Clone)]
pub struct AttachmentLink {
    pub path: String,
    pub display_name: String,
    pub range: Range<usize>,
}

pub(crate) struct StagedAttachment {
    pub path: String,
    pub bytes: Vec<u8>,
}

pub fn ensure_enabled(config: &Config) -> Result<()> {
    for configured in [&config.tasks_directory, &config.projects_directory] {
        if managed_paths_overlap(Path::new(DIRECTORY), Path::new(configured)) {
            return Err(Error::validation("attachments_disabled", "Attachments overlaps a configured record directory; ordinary task operations remain available"));
        }
    }
    Ok(())
}

pub fn validate_selector(selector: &str) -> Result<&Path> {
    let path = Path::new(selector);
    if !selector.starts_with("Attachments/")
        || selector.contains('\\')
        || selector.chars().any(char::is_control)
        || selector
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(Error::validation(
            "unsafe_attachment_path",
            "Use an explicit normalized store-relative Attachments/... file path",
        )
        .with_path(path));
    }
    Ok(path)
}

/// Inspect every component without following symlinks. Missing targets remain listable.
pub(crate) fn safe_metadata(root: &Path, relative: &Path) -> Result<Option<fs::Metadata>> {
    let mut current = root.to_path_buf();
    let count = relative.components().count();
    for (index, component) in relative.components().enumerate() {
        if !matches!(component, Component::Normal(_)) {
            return Err(Error::validation(
                "unsafe_attachment_path",
                "Attachment path is not normalized",
            )
            .with_path(relative));
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink()
                    || (index + 1 < count && !meta.is_dir())
                    || (index + 1 == count && !meta.is_file())
                {
                    return Err(Error::validation(
                        "unsafe_attachment_path",
                        "Attachment paths must be regular files without symlinks",
                    )
                    .with_path(relative));
                }
                if index + 1 == count {
                    return Ok(Some(meta));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(Error::io("inspect attachment", &current, &error)),
        }
    }
    Ok(None)
}

pub fn inspect(root: &Path, selector: &str, display_name: &str) -> Result<AttachmentView> {
    let path = validate_selector(selector)?;
    let metadata = safe_metadata(root, path)?;
    Ok(AttachmentView {
        path: selector.to_owned(),
        display_name: display_name.to_owned(),
        byte_size: metadata.as_ref().map(fs::Metadata::len),
        availability: if metadata.is_some() {
            "available"
        } else {
            "missing"
        },
    })
}

pub fn sanitize_filename(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut safe = safe
        .trim_matches(|c: char| c.is_whitespace() || c == '.')
        .to_owned();
    // Leave ample room for filesystem filename limits without cutting a UTF-8 scalar.
    while safe.len() > 200 {
        safe.pop();
    }
    if safe.is_empty() {
        "attachment".to_owned()
    } else {
        safe
    }
}

pub(crate) fn stage(sources: &[PathBuf]) -> Result<Vec<StagedAttachment>> {
    sources
        .iter()
        .map(|source| {
            let absolute = if source.is_absolute() {
                source.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| Error::io("resolve attachment source", source, &e))?
                    .join(source)
            };
            // Source files are explicitly user supplied, but symlinks anywhere in them are refused.
            let mut current = PathBuf::new();
            for component in absolute.components() {
                current.push(component);
                let meta = fs::symlink_metadata(&current).map_err(|_| {
                    Error::validation(
                        "attachment_source_invalid",
                        "Attachment source is missing or unreadable",
                    )
                    .with_path(source)
                })?;
                if meta.file_type().is_symlink() {
                    return Err(Error::validation(
                        "attachment_source_invalid",
                        "Attachment source must not traverse symlinks",
                    )
                    .with_path(source));
                }
            }
            let mut options = OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
            }
            let file = options.open(&absolute).map_err(|_| {
                Error::validation(
                    "attachment_source_invalid",
                    "Attachment source is missing or unreadable",
                )
                .with_path(source)
            })?;
            let meta = file
                .metadata()
                .map_err(|e| Error::io("inspect attachment source", source, &e))?;
            if !meta.is_file() {
                return Err(Error::validation(
                    "attachment_source_invalid",
                    "Attachment source must be a regular file",
                )
                .with_path(source));
            }
            if meta.len() > MAX_ATTACHMENT_BYTES {
                return Err(too_large(source));
            }
            let mut bytes = Vec::new();
            file.take(MAX_ATTACHMENT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| Error::io("read attachment source", source, &e))?;
            if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
                return Err(too_large(source));
            }
            let filename = source.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
                Error::validation(
                    "attachment_source_invalid",
                    "Attachment filename must be UTF-8",
                )
                .with_path(source)
            })?;
            Ok(StagedAttachment {
                path: format!(
                    "Attachments/{}/{}",
                    Ulid::new(),
                    sanitize_filename(filename)
                ),
                bytes,
            })
        })
        .collect()
}

fn too_large(path: &Path) -> Error {
    Error::validation(
        "attachment_too_large",
        "Attachment exceeds the 20 MiB (20971520 byte) limit",
    )
    .with_path(path)
}

pub fn is_image(path: &str) -> bool {
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "svg"
            | "bmp"
            | "heic"
            | "heif"
            | "tiff"
            | "tif"
            | "avif"
    )
}

pub fn markdown_link(task_path: &Path, selector: &str, display_name: &str) -> Result<String> {
    validate_selector(selector)?;
    let parent = task_path.parent().unwrap_or(Path::new(""));
    let source: Vec<_> = parent.components().collect();
    let target: Vec<_> = Path::new(selector).components().collect();
    let shared = source
        .iter()
        .zip(&target)
        .take_while(|(a, b)| a == b)
        .count();
    let relative = format!(
        "{}{}",
        "../".repeat(source.len() - shared),
        target[shared..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    );
    let mut encoded = String::new();
    for byte in relative.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    let label = display_name
        .replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]");
    Ok(format!(
        "{}[{label}]({encoded})",
        if is_image(selector) { "!" } else { "" }
    ))
}

pub fn append_links(body: &str, task_path: &Path, selectors: &[String]) -> Result<String> {
    let mut output = body.to_owned();
    if selectors.is_empty() {
        return Ok(output);
    }
    if !output.is_empty() && !output.ends_with('\n') {
        output.push('\n');
    }
    if !output.is_empty() && !output.ends_with("\n\n") {
        output.push('\n');
    }
    output.push_str("## Attachments\n\n");
    for selector in selectors {
        let name = selector.rsplit('/').next().unwrap_or(selector);
        output.push_str("- ");
        output.push_str(&markdown_link(task_path, selector, name)?);
        output.push('\n');
    }
    Ok(output)
}

fn decode(value: &str) -> Option<String> {
    let mut output = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let digits = value.get(index + 1..index + 3)?;
            output.push(u8::from_str_radix(digits, 16).ok()?);
            index += 3;
        } else if bytes[index] == b'\\' && index + 1 < bytes.len() {
            output.push(bytes[index + 1]);
            index += 2;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn resolve(task_path: &Path, target: &str, wiki: bool, store_prefix: &str) -> Option<String> {
    let target = target.split('#').next()?.split('?').next()?;
    let decoded = decode(target)?;
    let target = if wiki && !store_prefix.is_empty() {
        decoded
            .strip_prefix(&format!("{store_prefix}/"))
            .unwrap_or(&decoded)
    } else {
        &decoded
    };
    if target.contains(':') || target.starts_with('/') || target.contains('\\') {
        return None;
    }
    let explicit =
        target.starts_with("Attachments/") || target.starts_with("../") || target.starts_with("./");
    if !explicit {
        return None;
    }
    let mut parts: Vec<String> = if wiki && target.starts_with("Attachments/") {
        Vec::new()
    } else {
        task_path
            .parent()?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    };
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part.to_owned()),
        }
    }
    let result = parts.join("/");
    validate_selector(&result).ok()?;
    Some(result)
}

fn escaped(bytes: &[u8], index: usize) -> bool {
    bytes[..index]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

fn closing_bracket(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 1;
    let mut index = open + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                index += 2;
                continue;
            }
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn unescape_label(label: &str) -> String {
    let mut output = String::new();
    let mut chars = label.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\' && chars.peek().is_some_and(char::is_ascii_punctuation) {
            if let Some(next) = chars.next() {
                output.push(next);
            }
        } else {
            output.push(character);
        }
    }
    output
}

/// Conservative inline parser: explicit paths only; never interpret code examples.
pub fn links(body: &str, task_path: &Path) -> Vec<AttachmentLink> {
    links_with_prefix(body, task_path, "")
}

pub fn links_with_prefix(body: &str, task_path: &Path, store_prefix: &str) -> Vec<AttachmentLink> {
    parse_links(body, task_path, store_prefix).0
}

fn parse_links(
    body: &str,
    task_path: &Path,
    store_prefix: &str,
) -> (Vec<AttachmentLink>, Vec<String>) {
    let mut result = Vec::new();
    let mut unsupported = Vec::new();
    let mut code_until = 0;
    let mut offset = 0;
    let mut fence: Option<(u8, usize)> = None;
    for line in body.split_inclusive('\n') {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let bytes = trimmed.as_bytes();
        let run = bytes
            .first()
            .map_or(0, |first| bytes.iter().take_while(|b| *b == first).count());
        if indent <= 3 && run >= 3 && matches!(bytes.first(), Some(b'`' | b'~')) {
            let marker = bytes[0];
            if let Some((open, length)) = fence {
                if marker == open && run >= length && trimmed[run..].trim().is_empty() {
                    fence = None;
                }
            } else {
                fence = Some((marker, run));
            }
            offset += line.len();
            continue;
        }
        if fence.is_some() || indent >= 4 || line.starts_with('\t') {
            offset += line.len();
            continue;
        }
        let bytes = line.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if offset + index < code_until {
                index = (code_until - offset).min(bytes.len());
                continue;
            }
            if bytes[index] == b'`' && !escaped(bytes, index) {
                let run = bytes[index..].iter().take_while(|b| **b == b'`').count();
                let delimiter = "`".repeat(run);
                let remaining = &body[offset + index + run..];
                if let Some((end, _)) = remaining.match_indices(&delimiter).find(|(end, _)| {
                    (*end == 0 || remaining.as_bytes()[*end - 1] != b'`')
                        && remaining.as_bytes().get(*end + run) != Some(&b'`')
                }) {
                    code_until = offset + index + run + end + run;
                    continue;
                }
            }
            let start = index;
            let bracket = if bytes[index] == b'!' && bytes.get(index + 1) == Some(&b'[') {
                index + 1
            } else {
                index
            };
            if bytes.get(bracket) != Some(&b'[') || escaped(bytes, start) {
                index += 1;
                continue;
            }
            if bytes.get(bracket + 1) == Some(&b'[') {
                if let Some(end) = line[bracket + 2..].find("]]") {
                    let end = bracket + 2 + end;
                    let inside = &line[bracket + 2..end];
                    let mut pieces = inside.splitn(2, '|');
                    let target = pieces.next().unwrap_or("");
                    if let Some(path) = resolve(task_path, target, true, store_prefix) {
                        let display_name = pieces
                            .next()
                            .map(str::to_owned)
                            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path).to_owned());
                        result.push(AttachmentLink {
                            path,
                            display_name,
                            range: offset + start..offset + end + 2,
                        });
                    } else if attachment_like(target) {
                        unsupported.push(target.to_owned());
                    }
                    index = end + 2;
                    continue;
                }
            } else if let Some(close) = closing_bracket(bytes, bracket) {
                if bytes.get(close + 1) == Some(&b'(') {
                    let mut end = close + 2;
                    let mut depth = 1;
                    while end < bytes.len() {
                        if !escaped(bytes, end) {
                            if bytes[end] == b'(' {
                                depth += 1;
                            }
                            if bytes[end] == b')' {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                        }
                        end += 1;
                    }
                    if depth == 0 {
                        let inside = line[close + 2..end].trim();
                        let target = if let Some(rest) = inside.strip_prefix('<') {
                            rest.split('>').next().unwrap_or("")
                        } else {
                            inside.split_whitespace().next().unwrap_or("")
                        };
                        if let Some(path) = resolve(task_path, target, false, store_prefix) {
                            let display_name = unescape_label(&line[bracket + 1..close]);
                            result.push(AttachmentLink {
                                path,
                                display_name,
                                range: offset + start..offset + end + 1,
                            });
                        } else if attachment_like(target) {
                            unsupported.push(target.to_owned());
                        }
                        index = end + 1;
                        continue;
                    }
                }
            }
            index = bracket + 1;
        }
        offset += line.len();
    }
    (result, unsupported)
}

fn attachment_like(target: &str) -> bool {
    if target.contains("://") || target.starts_with("mailto:") {
        return false;
    }
    target.contains("Attachments/")
        || is_image(target)
        || matches!(
            target.rsplit('.').next(),
            Some("pdf" | "zip" | "docx" | "xlsx" | "pptx")
        )
}

pub fn unlink(body: &str, task_path: &Path, selector: &str) -> String {
    unlink_with_prefix(body, task_path, selector, "")
}

pub fn unlink_with_prefix(
    body: &str,
    task_path: &Path,
    selector: &str,
    store_prefix: &str,
) -> String {
    let mut output = body.to_owned();
    for link in links_with_prefix(body, task_path, store_prefix)
        .into_iter()
        .rev()
        .filter(|link| link.path == selector)
    {
        output.replace_range(link.range, "");
    }
    output
}

pub fn list(root: &Path, task_path: &Path, body: &str) -> Result<Vec<AttachmentView>> {
    list_with_prefix(root, task_path, body, "")
}

pub fn list_with_prefix(
    root: &Path,
    task_path: &Path,
    body: &str,
    store_prefix: &str,
) -> Result<Vec<AttachmentView>> {
    let mut seen = HashSet::new();
    links_with_prefix(body, task_path, store_prefix)
        .into_iter()
        .filter(|link| seen.insert(link.path.clone()))
        .map(|link| inspect(root, &link.path, &link.display_name))
        .collect()
}

pub(crate) fn diagnostics(
    root: &Path,
    task_path: &Path,
    body: &str,
    store_prefix: &str,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let mut seen = HashSet::new();
    let (recognized, unsupported) = parse_links(body, task_path, store_prefix);
    for target in unsupported {
        issues.push(ValidationIssue::warning("attachment_link_unsupported", format!("Attachment link {target:?} is unsafe, shortened, or unsupported; use an explicit path into Attachments/")).at_path(task_path));
    }
    for link in recognized {
        if !seen.insert(link.path.clone()) {
            continue;
        }
        match inspect(root, &link.path, &link.display_name) {
            Ok(view) if view.availability == "missing" => issues.push(
                ValidationIssue::warning(
                    "attachment_missing",
                    format!("Attachment {} is missing", link.path),
                )
                .at_path(task_path)
                .with_suggestion(
                    "Restore the file or remove its link; ordinary task editing remains available",
                ),
            ),
            Ok(view)
                if view
                    .byte_size
                    .is_some_and(|size| size > MAX_ATTACHMENT_BYTES) =>
            {
                issues.push(
                    ValidationIssue::warning(
                        "attachment_too_large",
                        format!(
                            "Attachment {} exceeds the 20 MiB import/download limit",
                            link.path
                        ),
                    )
                    .at_path(task_path),
                )
            }
            Err(error) => issues
                .push(ValidationIssue::warning(error.code(), error.message()).at_path(task_path)),
            _ => {}
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_link_conformance() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/attachments/links.json"))
                .expect("fixtures");
        for case in fixtures["cases"].as_array().expect("cases") {
            let path = Path::new(case["task_path"].as_str().expect("path"));
            let body = case["body"].as_str().expect("body");
            let prefix = case["store_prefix"].as_str().unwrap_or("");
            let mut seen = HashSet::new();
            let actual: Vec<_> = links_with_prefix(body, path, prefix)
                .into_iter()
                .filter_map(|link| seen.insert(link.path.clone()).then_some(link.path))
                .collect();
            assert_eq!(serde_json::json!(actual), case["paths"], "{}", case["name"]);
            assert_eq!(
                unlink_with_prefix(
                    body,
                    path,
                    case["unlink_path"].as_str().expect("selector"),
                    prefix
                ),
                case["unlinked"].as_str().expect("unlinked"),
                "{}",
                case["name"]
            );
        }
        for case in fixtures["filenames"].as_array().expect("filenames") {
            assert_eq!(
                sanitize_filename(case["input"].as_str().expect("input")),
                case["output"].as_str().expect("output")
            );
        }
    }
    #[test]
    fn attachment_publication_never_clobbers_and_task_snapshot_failure_retains_bytes() {
        use crate::commands::init::{initialize, InitOptions};
        use crate::commands::task::{add, AddTask};
        use crate::frontmatter::serialize_task;
        use crate::store::Store;

        let vault = tempfile::TempDir::new().expect("vault");
        let root = vault.path().join("Todo");
        initialize(&InitOptions {
            store_path: &root,
            vault_root: Some(vault.path()),
            current_directory: vault.path(),
            adopt_empty_layout: false,
            dry_run: false,
        })
        .expect("init");
        let store = Store::open(&root).expect("open");
        let task = add(
            &store,
            &AddTask {
                name: "Original".to_owned(),
                state: None,
                projects: vec![],
                tags: vec![],
                parent: None,
                url: None,
                due_date: None,
                due_time: None,
                recurrence: None,
                recurrence_from: None,
                body: String::new(),
            },
        )
        .expect("task");
        let staged = StagedAttachment {
            path: "Attachments/manual/receipt.pdf".to_owned(),
            bytes: vec![0, 255, 3],
        };
        store
            .with_exclusive_lock(|| {
                let stored = store.resolve_task_unlocked(&task.id)?;
                store.create_attachment(&staged)?;
                let replacement = StagedAttachment {
                    path: staged.path.clone(),
                    bytes: vec![1, 2],
                };
                assert_eq!(
                    store
                        .create_attachment(&replacement)
                        .expect_err("no clobber")
                        .code(),
                    "record_already_exists"
                );
                let mut with_link = stored.task;
                with_link.body = append_links(
                    &with_link.body,
                    &with_link.path,
                    std::slice::from_ref(&staged.path),
                )?;
                let bytes = serialize_task(&with_link, store.config())?;
                // An external editor changes the selected task after file publication.
                let external = fs::read_to_string(root.join(&task.path))
                    .expect("task bytes")
                    .replace("Original", "External");
                fs::write(root.join(&task.path), &external).expect("external edit");
                assert_eq!(
                    store
                        .replace(&stored.snapshot, &bytes)
                        .expect_err("snapshot refusal")
                        .code(),
                    "concurrent_modification"
                );
                assert_eq!(
                    fs::read_to_string(root.join(&task.path)).expect("preserved external task"),
                    external
                );
                assert_eq!(
                    fs::read(root.join(&staged.path)).expect("retained orphan bytes"),
                    staged.bytes
                );
                Ok(())
            })
            .expect("transaction");
    }

    #[test]
    fn generated_unicode_links_roundtrip_and_rebase() {
        let target = "Attachments/id/café [photo].png";
        for task in ["Tasks/task.md", "Tasks/nested/deep/task.md"] {
            let generated = markdown_link(Path::new(task), target, "café [photo]").expect("link");
            assert!(generated.starts_with("!["));
            assert_eq!(links(&generated, Path::new(task))[0].path, target);
        }
    }
}
