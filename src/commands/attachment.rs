use std::path::{Path, PathBuf};

use crate::attachments::{self, AttachmentView};
use crate::error::{Error, Result};
use crate::frontmatter::serialize_task;
use crate::store::Store;

pub fn list(store: &Store, task_id: &str) -> Result<Vec<AttachmentView>> {
    attachments::ensure_enabled(store.config())?;
    store.with_shared_lock(|| {
        let task = store.resolve_task_unlocked(task_id)?.task;
        attachments::list_with_prefix(
            store.root(),
            &task.path,
            &task.body,
            &store.config().obsidian_link_prefix,
        )
    })
}

pub fn add(store: &Store, task_id: &str, sources: &[PathBuf]) -> Result<Vec<AttachmentView>> {
    attachments::ensure_enabled(store.config())?;
    if sources.is_empty() {
        return Err(Error::usage(
            "attachment_source_invalid",
            "At least one attachment source is required",
        ));
    }
    let staged = attachments::stage(sources)?;
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(task_id)?;
        let mut task = stored.task;
        let selectors = staged.iter().map(|a| a.path.clone()).collect::<Vec<_>>();
        task.body = attachments::append_links(&task.body, &task.path, &selectors)?;
        let bytes = serialize_task(&task, store.config())?;
        for attachment in &staged {
            store.create_attachment(attachment)?;
        }
        store.replace(&stored.snapshot, &bytes)?;
        // Results describe staged bytes; post-publication filesystem reads cannot turn
        // a successful publication into an apparently retryable source-validation error.
        Ok(staged
            .iter()
            .map(|a| AttachmentView {
                path: a.path.clone(),
                display_name: Path::new(&a.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                byte_size: Some(a.bytes.len() as u64),
                availability: "available",
            })
            .collect())
    })
}

pub fn link(store: &Store, task_id: &str, selector: &str) -> Result<AttachmentView> {
    attachments::ensure_enabled(store.config())?;
    let path = attachments::validate_selector(selector)?;
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(task_id)?;
        let mut task = stored.task;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let view = attachments::inspect(store.root(), selector, &name)?;
        if view.availability == "missing" {
            return Err(missing(selector));
        }
        if !attachments::links_with_prefix(
            &task.body,
            &task.path,
            &store.config().obsidian_link_prefix,
        )
        .iter()
        .any(|link| link.path == selector)
        {
            task.body = attachments::append_links(&task.body, &task.path, &[selector.to_owned()])?;
            let bytes = serialize_task(&task, store.config())?;
            store.replace_checked(&stored.snapshot, &bytes, || {
                if attachments::inspect(store.root(), selector, &name)?.availability == "missing" {
                    return Err(missing(selector));
                }
                Ok(())
            })?;
        }
        Ok(view)
    })
}

pub fn unlink(store: &Store, task_id: &str, selector: &str) -> Result<AttachmentView> {
    attachments::ensure_enabled(store.config())?;
    attachments::validate_selector(selector)?;
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(task_id)?;
        let mut task = stored.task;
        let recognized = attachments::links_with_prefix(
            &task.body,
            &task.path,
            &store.config().obsidian_link_prefix,
        )
        .into_iter()
        .find(|link| link.path == selector)
        .ok_or_else(|| not_linked(selector))?;
        let view = attachments::inspect(store.root(), selector, &recognized.display_name)?;
        task.body = attachments::unlink_with_prefix(
            &task.body,
            &task.path,
            selector,
            &store.config().obsidian_link_prefix,
        );
        let bytes = serialize_task(&task, store.config())?;
        store.replace(&stored.snapshot, &bytes)?;
        Ok(view)
    })
}

pub fn path(store: &Store, task_id: &str, selector: &str) -> Result<PathBuf> {
    attachments::ensure_enabled(store.config())?;
    attachments::validate_selector(selector)?;
    store.with_shared_lock(|| {
        let task = store.resolve_task_unlocked(task_id)?.task;
        if !attachments::links_with_prefix(
            &task.body,
            &task.path,
            &store.config().obsidian_link_prefix,
        )
        .iter()
        .any(|link| link.path == selector)
        {
            return Err(not_linked(selector));
        }
        if attachments::inspect(store.root(), selector, selector)?.availability == "missing" {
            return Err(missing(selector));
        }
        Ok(store.root().join(selector))
    })
}

fn missing(selector: &str) -> Error {
    Error::not_found("attachment_not_found", "Attachment file is missing").with_path(selector)
}

fn not_linked(selector: &str) -> Error {
    Error::not_found("attachment_not_linked", "No recognized link to this attachment; shortened or unsupported links need an explicit Attachments/... path").with_path(selector)
}
