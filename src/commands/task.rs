use std::collections::{HashMap, HashSet};

use chrono::{NaiveDate, NaiveTime};
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::frontmatter::serialize_task;
use crate::model::{
    normalize_body, normalize_parent_id, relative_path_string, validate_name,
    validate_project_slug, validate_tag, validate_unique_projects, Clock, ParentRecord, Task,
};
use crate::recurrence::{RecurrenceMode, RecurrenceRule};
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct AddTask {
    pub name: String,
    pub state: Option<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub parent: Option<String>,
    pub url: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub due_time: Option<NaiveTime>,
    pub recurrence: Option<RecurrenceRule>,
    pub recurrence_from: Option<RecurrenceMode>,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct EditTask {
    pub name: Option<String>,
    pub state: Option<String>,
    pub add_projects: Vec<String>,
    pub remove_projects: Vec<String>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
    pub due_date: Option<NaiveDate>,
    pub clear_due_date: bool,
    pub due_time: Option<NaiveTime>,
    pub clear_due_time: bool,
    pub recurrence: Option<RecurrenceRule>,
    pub recurrence_from: Option<RecurrenceMode>,
    pub clear_recurrence: bool,
    pub body: Option<String>,
    pub parent: Option<String>,
    pub clear_parent: bool,
    pub url: Option<String>,
    pub clear_url: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub include_terminal: bool,
    pub states: Vec<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub due_on: Option<NaiveDate>,
    pub due_before: Option<NaiveDate>,
    pub due_after: Option<NaiveDate>,
    pub overdue: bool,
    pub recurring: bool,
    pub non_recurring: bool,
    pub parent: Option<String>,
    pub roots: bool,
    pub query: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub id: String,
    pub path: String,
    pub name: String,
    pub state: String,
    pub terminal: bool,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub parent: Option<String>,
    pub url: Option<String>,
    pub due_date: Option<String>,
    pub due_time: Option<String>,
    pub recurrence: Option<String>,
    pub recurrence_from: Option<String>,
    pub last_completed_date: Option<String>,
    pub body: String,
    pub extra_properties: serde_json::Value,
}

impl TaskView {
    pub fn from_task(task: &Task, store: &Store) -> Result<Self> {
        let extra_properties = serde_json::to_value(&task.extra_properties).map_err(|source| {
            Error::validation(
                "extra_property_json_failed",
                format!("Could not represent task properties as JSON: {source}"),
            )
            .with_path(&task.path)
        })?;
        let mut projects = task.projects.clone();
        projects.sort_unstable();
        let mut tags = task.tags.clone();
        tags.sort_unstable();
        Ok(Self {
            id: task.id.clone(),
            path: relative_path_string(&task.path),
            name: task.name.clone(),
            state: task.state.clone(),
            terminal: task.terminal(store.config()),
            projects,
            tags,
            parent: task.parent.clone(),
            url: task.url.clone(),
            due_date: task
                .due_date
                .map(|date| date.format("%Y-%m-%d").to_string()),
            due_time: task.due_time.map(|time| time.format("%H:%M").to_string()),
            recurrence: task.recurrence.as_ref().map(ToString::to_string),
            recurrence_from: task.recurrence_from.map(|mode| mode.as_str().to_owned()),
            last_completed_date: task
                .last_completed_date
                .map(|date| date.format("%Y-%m-%d").to_string()),
            body: task.body.clone(),
            extra_properties,
        })
    }
}

pub fn add(store: &Store, request: &AddTask) -> Result<Task> {
    add_with_attachments(store, request, &[])
}

/// Stage every file before publishing; task publication is the final single-record mutation.
pub fn add_with_attachments(
    store: &Store,
    request: &AddTask,
    sources: &[std::path::PathBuf],
) -> Result<Task> {
    if !sources.is_empty() {
        crate::attachments::ensure_enabled(store.config())?;
    }
    let staged = crate::attachments::stage(sources)?;
    require_parent_schema(store, request.parent.is_some())?;
    let parent = request
        .parent
        .as_deref()
        .map(normalize_parent_id)
        .transpose()?;
    store.with_exclusive_lock(|| {
        let name = request.name.trim().to_owned();
        validate_name(&name, "name")?;
        let known_projects = known_projects(store)?;
        ensure_requested_projects(&request.projects, &known_projects)?;
        let relations = store.relations_unlocked()?;
        let existing_ids: HashSet<&str> =
            relations.iter().map(|record| record.id.as_str()).collect();
        let mut extra_properties = Mapping::new();
        extra_properties.insert(
            Value::String("base".to_owned()),
            Value::String(store.config().todos_base_link()),
        );

        let mut attachments_published = false;
        for _ in 0..32 {
            let id = Ulid::new().to_string().to_ascii_uppercase();
            if existing_ids.contains(id.as_str()) {
                continue;
            }
            let path = store.config().tasks_path()?.join(format!("{id}.md"));
            let mut task = Task {
                id: id.clone(),
                path,
                name: name.clone(),
                state: request
                    .state
                    .clone()
                    .unwrap_or_else(|| store.config().default_state.clone()),
                projects: request.projects.clone(),
                tags: request.tags.clone(),
                parent: parent.clone(),
                url: request.url.as_ref().map(|value| value.trim().to_owned()),
                due_date: request.due_date,
                due_time: request.due_time,
                recurrence: request.recurrence.clone(),
                recurrence_from: request.recurrence_from,
                last_completed_date: None,
                body: normalize_body(&request.body),
                extra_properties: extra_properties.clone(),
            };
            task.validate(store.config())?;
            ensure_parent_destination(&task, &relations)?;
            task.body = crate::attachments::append_links(
                &task.body,
                &task.path,
                &staged.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
            )?;
            let bytes = serialize_task(&task, store.config())?;
            if !attachments_published {
                for attachment in &staged {
                    store.create_attachment(attachment)?;
                }
                attachments_published = true;
            }
            match store.create_task_checked(&id, &bytes, &relations) {
                Ok(_) => return Ok(task),
                Err(error) if error.code() == "record_already_exists" => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::new(
            crate::error::ErrorKind::Concurrent,
            "task_id_collision",
            "Could not generate a unique task ID after repeated collisions",
        ))
    })
}

pub fn list(store: &Store, filter: &TaskFilter, today: NaiveDate) -> Result<Vec<Task>> {
    validate_filter(store, filter)?;
    let mut normalized_filter = filter.clone();
    normalized_filter.query = filter.query.as_ref().map(|query| query.to_lowercase());
    let filter = &normalized_filter;
    store.with_shared_lock(|| {
        let known_projects = known_projects(store)?;
        ensure_known_projects(&filter.projects, &known_projects)?;
        let mut tasks = store.load_selected_tasks_unlocked(|task| {
            validate_task_references(std::iter::once(task), &known_projects)?;
            Ok(matches_filter(task, filter, store, today))
        })?;
        if let Some(parent) = &filter.parent {
            store.resolve_task_unlocked(parent)?;
        }
        tasks.sort_by(|left, right| {
            match (left.due_date, right.due_date) {
                (Some(left), Some(right)) => left.cmp(&right),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            .then_with(|| {
                store
                    .config()
                    .state_order(&left.state)
                    .cmp(&store.config().state_order(&right.state))
            })
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
        });
        Ok(tasks)
    })
}

pub fn show(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_shared_lock(|| {
        let task = store.resolve_task_unlocked(id_or_prefix)?.task;
        let known = known_projects(store)?;
        validate_task_references(std::iter::once(&task), &known)?;
        Ok(task)
    })
}

pub fn edit(store: &Store, id_or_prefix: &str, changes: &EditTask) -> Result<Task> {
    validate_edit_request(changes)?;
    require_parent_schema(store, changes.parent.is_some() || changes.clear_parent)?;
    store.with_exclusive_lock(|| {
        let relations = store.relations_unlocked()?;
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_selected_relation(&task, &relations)?;
        let known_projects = known_projects(store)?;
        apply_edit(&mut task, changes, &known_projects)?;
        task.validate(store.config())?;
        if changes.parent.is_some() {
            ensure_parent_destination(&task, &relations)?;
        }
        let bytes = serialize_task(&task, store.config())?;
        store.replace_checked(&stored.snapshot, &bytes, || {
            store.ensure_relations_unchanged(&relations)
        })?;
        Ok(task)
    })
}

pub fn complete(
    store: &Store,
    id_or_prefix: &str,
    completed_on: Option<NaiveDate>,
    clock: &dyn Clock,
) -> Result<Task> {
    let completed_on = completed_on.unwrap_or_else(|| clock.today());
    store.with_exclusive_lock(|| {
        let relations = store.relations_unlocked()?;
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_selected_relation(&task, &relations)?;
        ensure_nonterminal(&task, store)?;
        ensure_task_projects(store, &task)?;
        let done = required_terminal_state(store, "done", "complete")?;
        if let Some(rule) = &task.recurrence {
            let due = task.due_date.ok_or_else(|| {
                Error::validation(
                    "recurrence_requires_due_date",
                    "Recurring task has no due_date",
                )
                .with_field("due_date")
            })?;
            let mode = task.recurrence_from.ok_or_else(|| {
                Error::validation(
                    "recurrence_requires_mode",
                    "Recurring task has no recurrence_from",
                )
                .with_field("recurrence_from")
            })?;
            if task
                .last_completed_date
                .is_some_and(|last| completed_on < last)
            {
                return Err(Error::validation(
                    "completion_date_moved_backwards",
                    "Completion date cannot be earlier than last_completed_date",
                )
                .with_field("last_completed_date"));
            }
            task.due_date = Some(rule.next_due(due, completed_on, mode)?);
            task.last_completed_date = Some(completed_on);
            task.state.clone_from(&store.config().default_state);
        } else {
            task.state = done.to_owned();
        }
        task.validate(store.config())?;
        let bytes = serialize_task(&task, store.config())?;
        store.replace_checked(&stored.snapshot, &bytes, || {
            store.ensure_relations_unchanged(&relations)
        })?;
        Ok(task)
    })
}

pub fn finish_series(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_exclusive_lock(|| {
        let relations = store.relations_unlocked()?;
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_selected_relation(&task, &relations)?;
        ensure_nonterminal(&task, store)?;
        ensure_task_projects(store, &task)?;
        if task.recurrence.is_none() {
            return Err(Error::validation(
                "task_not_recurring",
                "finish-series requires a recurring task",
            ));
        }
        task.state = required_terminal_state(store, "done", "finish-series")?.to_owned();
        replace_task(store, stored.snapshot, task, &relations)
    })
}

pub fn cancel(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_exclusive_lock(|| {
        let relations = store.relations_unlocked()?;
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_selected_relation(&task, &relations)?;
        ensure_nonterminal(&task, store)?;
        ensure_task_projects(store, &task)?;
        task.state = required_terminal_state(store, "cancelled", "cancel")?.to_owned();
        replace_task(store, stored.snapshot, task, &relations)
    })
}

pub fn reopen(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_exclusive_lock(|| {
        let relations = store.relations_unlocked()?;
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_selected_relation(&task, &relations)?;
        ensure_task_projects(store, &task)?;
        if !task.terminal(store.config()) {
            return Err(Error::validation(
                "task_not_terminal",
                "reopen requires a task in a terminal state",
            )
            .with_field("state"));
        }
        task.state.clone_from(&store.config().default_state);
        replace_task(store, stored.snapshot, task, &relations)
    })
}

pub fn delete(store: &Store, full_id: &str, confirmed: bool) -> Result<Task> {
    if !confirmed {
        return Err(Error::usage(
            "confirmation_required",
            "Task deletion requires --yes",
        ));
    }
    if full_id.len() != 26 {
        return Err(Error::usage(
            "full_id_required",
            "Task deletion requires the full 26-character ID; prefixes are forbidden",
        ));
    }
    store.with_exclusive_lock(|| {
        let relations = store.relations_unlocked()?;
        let stored = store.resolve_task_unlocked(full_id)?;
        ensure_selected_relation(&stored.task, &relations)?;
        let mut children: Vec<&str> = relations
            .iter()
            .filter(|record| record.parent.as_deref() == Some(stored.task.id.as_str()))
            .map(|record| record.id.as_str())
            .collect();
        children.sort_unstable();
        if !children.is_empty() {
            return Err(Error::validation(
                "task_in_use",
                format!(
                    "Task {} has direct children: {}",
                    stored.task.id,
                    children.join(", ")
                ),
            )
            .with_path(&stored.task.path)
            .with_field("parent"));
        }
        store.delete_checked(&stored.snapshot, || {
            store.ensure_relations_unchanged(&relations)
        })?;
        Ok(stored.task)
    })
}

fn apply_edit(task: &mut Task, changes: &EditTask, known_projects: &HashSet<String>) -> Result<()> {
    if changes.clear_parent {
        task.parent = None;
    } else if let Some(parent) = &changes.parent {
        task.parent = Some(normalize_parent_id(parent)?);
    }
    if changes.clear_url {
        task.url = None;
    } else if let Some(url) = &changes.url {
        task.url = Some(url.trim().to_owned());
    }
    if let Some(name) = &changes.name {
        let name = name.trim().to_owned();
        validate_name(&name, "name")?;
        task.name = name;
    }
    if let Some(state) = &changes.state {
        task.state.clone_from(state);
    }
    for project in &changes.remove_projects {
        let Some(index) = task.projects.iter().position(|value| value == project) else {
            return Err(Error::validation(
                "project_not_on_task",
                format!("Task does not reference project {project:?}"),
            )
            .with_field("projects"));
        };
        task.projects.remove(index);
    }
    for project in &changes.add_projects {
        if task.projects.contains(project) {
            return Err(Error::validation(
                "project_already_on_task",
                format!("Task already references project {project:?}"),
            )
            .with_field("projects"));
        }
        task.projects.push(project.clone());
    }
    ensure_requested_projects(&task.projects, known_projects)?;
    for tag in &changes.remove_tags {
        let Some(index) = task.tags.iter().position(|value| value == tag) else {
            return Err(Error::validation(
                "tag_not_on_task",
                format!("Task does not have tag {tag:?}"),
            )
            .with_field("tags"));
        };
        task.tags.remove(index);
    }
    for tag in &changes.add_tags {
        validate_tag(tag)?;
        if task.tags.contains(tag) {
            return Err(Error::validation(
                "tag_already_on_task",
                format!("Task already has tag {tag:?}"),
            )
            .with_field("tags"));
        }
        task.tags.push(tag.clone());
    }
    if changes.clear_recurrence {
        task.recurrence = None;
        task.recurrence_from = None;
        task.last_completed_date = None;
    } else {
        if task.recurrence.is_none()
            && changes.recurrence.is_some()
            && changes.recurrence_from.is_none()
        {
            return Err(Error::usage(
                "recurrence_mode_required",
                "Adding recurrence requires an explicit --recurrence-from mode",
            ));
        }
        if let Some(recurrence) = &changes.recurrence {
            task.recurrence = Some(recurrence.clone());
        }
        if let Some(mode) = changes.recurrence_from {
            task.recurrence_from = Some(mode);
        }
    }
    if changes.clear_due_date {
        task.due_date = None;
        task.due_time = None;
    } else if let Some(due_date) = changes.due_date {
        task.due_date = Some(due_date);
    }
    if changes.clear_due_time {
        task.due_time = None;
    } else if let Some(due_time) = changes.due_time {
        task.due_time = Some(due_time);
    }
    if let Some(body) = &changes.body {
        task.body = normalize_body(body);
    }
    Ok(())
}

fn validate_edit_request(changes: &EditTask) -> Result<()> {
    let has_change = changes.name.is_some()
        || changes.parent.is_some()
        || changes.clear_parent
        || changes.url.is_some()
        || changes.clear_url
        || changes.state.is_some()
        || !changes.add_projects.is_empty()
        || !changes.remove_projects.is_empty()
        || !changes.add_tags.is_empty()
        || !changes.remove_tags.is_empty()
        || changes.due_date.is_some()
        || changes.clear_due_date
        || changes.due_time.is_some()
        || changes.clear_due_time
        || changes.recurrence.is_some()
        || changes.recurrence_from.is_some()
        || changes.clear_recurrence
        || changes.body.is_some();
    if !has_change {
        return Err(Error::usage(
            "no_changes",
            "Task edit requires at least one change",
        ));
    }
    if changes.parent.is_some() && changes.clear_parent {
        return Err(Error::usage(
            "conflicting_changes",
            "--parent conflicts with --clear-parent",
        ));
    }
    if changes.url.is_some() && changes.clear_url {
        return Err(Error::usage(
            "conflicting_changes",
            "--url conflicts with --clear-url",
        ));
    }
    if changes.due_date.is_some() && changes.clear_due_date {
        return Err(Error::usage(
            "conflicting_changes",
            "--due-date conflicts with --clear-due-date",
        ));
    }
    if changes.due_time.is_some() && changes.clear_due_time {
        return Err(Error::usage(
            "conflicting_changes",
            "--due-time conflicts with --clear-due-time",
        )
        .with_field("due_time"));
    }
    if changes.due_time.is_some() && changes.clear_due_date {
        return Err(Error::validation(
            "due_time_requires_due_date",
            "Cannot set due_time while clearing due_date",
        )
        .with_field("due_time"));
    }
    if changes.clear_recurrence
        && (changes.recurrence.is_some() || changes.recurrence_from.is_some())
    {
        return Err(Error::usage(
            "conflicting_changes",
            "--clear-recurrence conflicts with recurrence changes",
        ));
    }
    reject_overlapping_changes(&changes.add_projects, &changes.remove_projects, "project")?;
    reject_overlapping_changes(&changes.add_tags, &changes.remove_tags, "tag")?;
    Ok(())
}

fn reject_overlapping_changes(add: &[String], remove: &[String], kind: &str) -> Result<()> {
    if let Some(value) = add.iter().find(|value| remove.contains(value)) {
        return Err(Error::usage(
            "conflicting_changes",
            format!("Cannot add and remove the same {kind} {value:?}"),
        ));
    }
    Ok(())
}

fn validate_filter(store: &Store, filter: &TaskFilter) -> Result<()> {
    if filter.parent.is_some() && filter.roots {
        return Err(Error::usage(
            "conflicting_filters",
            "--parent conflicts with --roots",
        ));
    }
    require_parent_schema(store, filter.parent.is_some() || filter.roots)?;
    if let Some(parent) = &filter.parent {
        normalize_parent_id(parent)?;
    }
    if filter.recurring && filter.non_recurring {
        return Err(Error::usage(
            "conflicting_filters",
            "--recurring conflicts with --non-recurring",
        ));
    }
    for state in &filter.states {
        if store.config().state(state).is_none() {
            return Err(Error::validation(
                "unknown_state",
                format!("State {state:?} is not configured"),
            )
            .with_field("state"));
        }
    }
    for tag in &filter.tags {
        validate_tag(tag)?;
    }
    for project in &filter.projects {
        validate_project_slug(project)?;
    }
    Ok(())
}

fn matches_filter(task: &Task, filter: &TaskFilter, store: &Store, today: NaiveDate) -> bool {
    if filter.roots && task.parent.is_some() {
        return false;
    }
    if let Some(parent) = &filter.parent {
        if task
            .parent
            .as_ref()
            .is_none_or(|value| !value.eq_ignore_ascii_case(parent))
        {
            return false;
        }
    }
    if let Some(query) = &filter.query {
        if !task.name.to_lowercase().contains(query) && !task.id.to_lowercase().contains(query) {
            return false;
        }
    }
    if !filter.include_terminal && task.terminal(store.config()) {
        return false;
    }
    if !filter.states.is_empty() && !filter.states.contains(&task.state) {
        return false;
    }
    if !filter
        .projects
        .iter()
        .all(|project| task.projects.contains(project))
        || !filter.tags.iter().all(|tag| task.tags.contains(tag))
    {
        return false;
    }
    if filter
        .due_on
        .is_some_and(|date| task.due_date != Some(date))
        || filter
            .due_before
            .is_some_and(|date| task.due_date.is_none_or(|due| due >= date))
        || filter
            .due_after
            .is_some_and(|date| task.due_date.is_none_or(|due| due <= date))
        || (filter.overdue
            && (task.terminal(store.config()) || task.due_date.is_none_or(|due| due >= today)))
        || (filter.recurring && task.recurrence.is_none())
        || (filter.non_recurring && task.recurrence.is_some())
    {
        return false;
    }
    true
}

fn known_projects(store: &Store) -> Result<HashSet<String>> {
    store
        .load_all_projects_unlocked()?
        .into_iter()
        .map(|stored| Ok(stored.project.slug))
        .collect()
}

fn ensure_requested_projects(projects: &[String], known: &HashSet<String>) -> Result<()> {
    validate_unique_projects(projects)?;
    ensure_known_projects(projects, known)
}

fn ensure_known_projects(projects: &[String], known: &HashSet<String>) -> Result<()> {
    for project in projects {
        if !known.contains(project) {
            return Err(Error::not_found(
                "project_not_found",
                format!("Project {project:?} does not exist"),
            )
            .with_field("projects"));
        }
    }
    Ok(())
}

fn validate_task_references<'a>(
    tasks: impl IntoIterator<Item = &'a Task>,
    known: &HashSet<String>,
) -> Result<()> {
    for task in tasks {
        for project in &task.projects {
            if !known.contains(project) {
                return Err(Error::validation(
                    "missing_project_reference",
                    format!("Task {} references missing project {project:?}", task.id),
                )
                .with_path(&task.path)
                .with_field("projects"));
            }
        }
    }
    Ok(())
}

fn ensure_task_projects(store: &Store, task: &Task) -> Result<()> {
    let known = known_projects(store)?;
    validate_task_references(std::iter::once(task), &known)
}

fn ensure_nonterminal(task: &Task, store: &Store) -> Result<()> {
    if task.terminal(store.config()) {
        return Err(Error::validation(
            "task_already_terminal",
            "Operation requires a task in a nonterminal state",
        )
        .with_field("state"));
    }
    Ok(())
}

fn required_terminal_state<'a>(store: &'a Store, id: &str, command: &str) -> Result<&'a str> {
    match store.config().state(id) {
        Some(state) if state.terminal => Ok(&state.id),
        _ => Err(Error::validation(
            "required_terminal_state_missing",
            format!(
                "Command {command} requires configured terminal state {id:?}; use edit --state instead"
            ),
        )),
    }
}

fn replace_task(
    store: &Store,
    snapshot: crate::store::FileSnapshot,
    task: Task,
    relations: &[ParentRecord],
) -> Result<Task> {
    task.validate(store.config())?;
    let bytes = serialize_task(&task, store.config())?;
    store.replace_checked(&snapshot, &bytes, || {
        store.ensure_relations_unchanged(relations)
    })?;
    Ok(task)
}

fn require_parent_schema(store: &Store, requested: bool) -> Result<()> {
    if requested && store.config().schema_version == 1 {
        return Err(Error::unsupported(
            "unsupported_schema",
            "Parent operations require store schema version 2; explicitly upgrade the store first",
        )
        .with_field("parent"));
    }
    Ok(())
}

fn ensure_selected_relation(task: &Task, relations: &[ParentRecord]) -> Result<()> {
    if relations.iter().any(|record| {
        record.id == task.id && record.path == task.path && record.parent == task.parent
    }) {
        Ok(())
    } else {
        Err(Error::new(
            crate::error::ErrorKind::Concurrent,
            "concurrent_modification",
            "Selected task changed during the relationship scan",
        )
        .with_path(&task.path))
    }
}

/// Validate the effective ancestry, not unrelated existing faults. Detach needs no
/// valid old ancestry, so a malformed graph remains explicitly repairable.
fn ensure_parent_destination(task: &Task, relations: &[ParentRecord]) -> Result<()> {
    let Some(parent) = &task.parent else {
        return Ok(());
    };
    let by_id: HashMap<&str, &ParentRecord> = relations
        .iter()
        .map(|record| (record.id.as_str(), record))
        .collect();
    if parent != &task.id && !by_id.contains_key(parent.as_str()) {
        return Err(Error::not_found(
            "task_not_found",
            format!("Parent task {parent} does not exist"),
        )
        .with_path(&task.path)
        .with_field("parent"));
    }
    let mut ancestry = vec![ParentRecord {
        id: task.id.clone(),
        path: task.path.clone(),
        parent: task.parent.clone(),
    }];
    let mut seen = HashSet::new();
    seen.insert(task.id.as_str());
    let mut current = Some(parent.as_str());
    while let Some(id) = current {
        if !seen.insert(id) {
            break;
        }
        let Some(record) = by_id.get(id) else {
            break;
        };
        ancestry.push((**record).clone());
        current = record.parent.as_deref();
    }
    crate::store::ensure_valid_relations(&ancestry)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::commands::init::{initialize, InitOptions};
    use crate::commands::project::{create as create_project, CreateProject};
    use crate::config::{Config, CONFIG_PATH};
    use crate::model::FixedClock;
    use crate::recurrence::parse_date;

    use super::*;

    fn store() -> (TempDir, Store) {
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
        let store = Store::open(root).expect("open");
        (temp, store)
    }

    fn minimal(name: &str) -> AddTask {
        AddTask {
            name: name.to_owned(),
            state: None,
            projects: Vec::new(),
            tags: Vec::new(),
            parent: None,
            url: None,
            due_date: None,
            due_time: None,
            recurrence: None,
            recurrence_from: None,
            body: String::new(),
        }
    }

    fn date(value: &str) -> NaiveDate {
        parse_date(value, "date").expect("date")
    }

    #[test]
    fn task_view_keeps_stable_null_and_array_fields() {
        let (_temp, store) = store();
        let task = add(&store, &minimal("Minimal")).expect("add");
        let value = serde_json::to_value(TaskView::from_task(&task, &store).expect("view"))
            .expect("serialize view");
        let object = value.as_object().expect("object");
        for field in [
            "parent",
            "url",
            "due_date",
            "due_time",
            "recurrence",
            "recurrence_from",
            "last_completed_date",
        ] {
            assert!(object[field].is_null(), "{field}");
        }
        assert_eq!(object["projects"], serde_json::json!([]));
        assert_eq!(object["tags"], serde_json::json!([]));
        assert_eq!(
            object["extra_properties"],
            serde_json::json!({"base": "[[Todo/todos.base]]"})
        );
    }

    #[test]
    fn due_time_edits_and_transitions_preserve_or_explicitly_clear_schedule() {
        let (temp, store) = store();
        let time = crate::model::parse_time("09:05").expect("time");
        let mut request = minimal("Timed");
        request.due_time = Some(time);
        let error = add(&store, &request).expect_err("time requires date");
        assert_eq!(error.code(), "due_time_requires_due_date");
        assert_eq!(error.field(), Some("due_time"));
        request.due_date = Some(date("2026-09-08"));
        let attachment = temp.path().join("receipt.txt");
        fs::write(&attachment, "receipt").expect("source");
        let task =
            add_with_attachments(&store, &request, &[attachment]).expect("timed attachment task");
        let saved = show(&store, &task.id).expect("stored task");
        assert_eq!(saved.due_time, Some(time));
        assert_eq!(crate::attachments::links(&saved.body, &saved.path).len(), 1);
        let json =
            serde_json::to_value(TaskView::from_task(&saved, &store).expect("view")).expect("JSON");
        assert_eq!(json["due_time"], "09:05");

        let mut child = minimal("Child");
        child.parent = Some(task.id.clone());
        assert_eq!(add(&store, &child).expect("child").due_time, None);
        let edited = edit(
            &store,
            &task.id,
            &EditTask {
                name: Some("Renamed".to_owned()),
                due_date: Some(date("2026-09-09")),
                ..EditTask::default()
            },
        )
        .expect("reschedule date only");
        assert_eq!(edited.due_time, Some(time));
        let completed = complete(&store, &task.id, None, &FixedClock::new(date("2026-09-09")))
            .expect("complete");
        assert_eq!(completed.due_time, Some(time));
        assert_eq!(
            reopen(&store, &task.id).expect("reopen").due_time,
            Some(time)
        );
        assert_eq!(
            cancel(&store, &task.id).expect("cancel").due_time,
            Some(time)
        );
        assert_eq!(
            reopen(&store, &task.id).expect("reopen cancelled").due_time,
            Some(time)
        );

        let before = fs::read(store.root().join(&task.path)).expect("snapshot");
        for (changes, code) in [
            (
                EditTask {
                    due_time: Some(time),
                    clear_due_time: true,
                    ..EditTask::default()
                },
                "conflicting_changes",
            ),
            (
                EditTask {
                    due_time: Some(time),
                    clear_due_date: true,
                    ..EditTask::default()
                },
                "due_time_requires_due_date",
            ),
            (
                EditTask {
                    due_time: chrono::NaiveTime::from_hms_opt(9, 5, 1),
                    ..EditTask::default()
                },
                "invalid_due_time",
            ),
        ] {
            let error = edit(&store, &task.id, &changes).expect_err("invalid change");
            assert_eq!(error.code(), code);
            assert_eq!(error.field(), Some("due_time"));
            assert_eq!(
                fs::read(store.root().join(&task.path)).expect("unchanged"),
                before
            );
        }
        let cleared = edit(
            &store,
            &task.id,
            &EditTask {
                clear_due_time: true,
                ..EditTask::default()
            },
        )
        .expect("clear time only");
        assert_eq!(cleared.due_time, None);
        assert_eq!(cleared.due_date, Some(date("2026-09-09")));
        let restored = edit(
            &store,
            &task.id,
            &EditTask {
                due_time: Some(time),
                ..EditTask::default()
            },
        )
        .expect("set time only");
        assert_eq!(restored.due_time, Some(time));
        let undated = edit(
            &store,
            &task.id,
            &EditTask {
                clear_due_date: true,
                ..EditTask::default()
            },
        )
        .expect("clear date and time");
        assert_eq!(undated.due_date, None);
        assert_eq!(
            show(&store, &task.id).expect("persisted clear").due_time,
            None
        );
        let before = fs::read(store.root().join(&task.path)).expect("undated snapshot");
        let error = edit(
            &store,
            &task.id,
            &EditTask {
                due_time: Some(time),
                ..EditTask::default()
            },
        )
        .expect_err("cannot orphan");
        assert_eq!(error.code(), "due_time_requires_due_date");
        assert_eq!(
            fs::read(store.root().join(&task.path)).expect("unchanged undated"),
            before
        );
    }

    #[test]
    fn add_edit_filter_sort_and_delete() {
        let (_temp, store) = store();
        create_project(
            &store,
            &CreateProject {
                slug: "work".to_owned(),
                name: "Work".to_owned(),
                body: String::new(),
            },
        )
        .expect("project");
        let mut first = minimal("Zulu");
        first.projects.push("work".to_owned());
        first.tags.push("review".to_owned());
        first.due_date = Some(date("2026-09-03"));
        let first = add(&store, &first).expect("first");
        let second = add(&store, &minimal("Alpha")).expect("second");
        let listed = list(
            &store,
            &TaskFilter {
                projects: vec!["work".to_owned()],
                tags: vec!["review".to_owned()],
                ..TaskFilter::default()
            },
            date("2026-09-02"),
        )
        .expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, first.id);
        let duplicate_filter = list(
            &store,
            &TaskFilter {
                projects: vec!["work".to_owned(), "work".to_owned()],
                ..TaskFilter::default()
            },
            date("2026-09-02"),
        )
        .expect("duplicate project filters");
        assert_eq!(duplicate_filter.len(), 1);
        let all = list(&store, &TaskFilter::default(), date("2026-09-02")).expect("list all");
        assert_eq!(all[0].id, first.id, "dated tasks sort first");
        assert_eq!(all[1].id, second.id);
        let edited = edit(
            &store,
            &first.id,
            &EditTask {
                state: Some("active".to_owned()),
                body: Some("Body\r\n".to_owned()),
                ..EditTask::default()
            },
        )
        .expect("edit");
        assert_eq!(edited.state, "active");
        assert_eq!(edited.body, "Body\n");
        assert!(delete(&store, &first.id[..6], true).is_err());
        delete(&store, &first.id, true).expect("delete");
    }

    #[test]
    fn recurring_completion_uses_both_modes_and_finish_reopen_preserves_rule() {
        let (_temp, store) = store();
        let clock = FixedClock::new(date("2026-09-09"));
        let mut scheduled = minimal("Scheduled");
        scheduled.due_date = Some(date("2026-09-07"));
        scheduled.due_time = Some(crate::model::parse_time("09:05").expect("time"));
        scheduled.recurrence =
            Some(RecurrenceRule::parse("FREQ=WEEKLY;BYDAY=MO").expect("weekly recurrence"));
        scheduled.recurrence_from = Some(RecurrenceMode::Schedule);
        let scheduled = add(&store, &scheduled).expect("scheduled");
        let scheduled = complete(&store, &scheduled.id, None, &clock).expect("complete");
        assert_eq!(scheduled.due_date, Some(date("2026-09-14")));
        assert_eq!(
            scheduled.due_time,
            Some(crate::model::parse_time("09:05").expect("time"))
        );
        assert_eq!(scheduled.state, "open");
        assert_eq!(scheduled.last_completed_date, Some(date("2026-09-09")));

        let mut relative = minimal("Relative");
        relative.due_date = Some(date("2026-09-07"));
        relative.due_time = Some(crate::model::parse_time("23:59").expect("time"));
        relative.recurrence =
            Some(RecurrenceRule::parse("FREQ=DAILY;INTERVAL=3").expect("daily recurrence"));
        relative.recurrence_from = Some(RecurrenceMode::Completion);
        let relative = add(&store, &relative).expect("relative");
        let relative = complete(&store, &relative.id, None, &clock).expect("complete");
        assert_eq!(relative.due_date, Some(date("2026-09-12")));
        assert_eq!(
            relative.due_time,
            Some(crate::model::parse_time("23:59").expect("time"))
        );
        let finished = finish_series(&store, &relative.id).expect("finish");
        assert_eq!(finished.state, "done");
        assert!(finished.recurrence.is_some());
        assert_eq!(finished.due_time, relative.due_time);
        let reopened = reopen(&store, &relative.id).expect("reopen");
        assert_eq!(reopened.state, "open");
        assert_eq!(reopened.due_date, Some(date("2026-09-12")));
        assert_eq!(reopened.due_time, relative.due_time);
    }

    #[test]
    fn complete_requires_a_done_state_for_recurring_tasks() {
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
        let mut config = Config::defaults("Todo".to_owned());
        config.states.retain(|state| state.id != "done");
        fs::write(
            root.join(CONFIG_PATH),
            config.to_toml().expect("serialize config"),
        )
        .expect("write config");
        let store = Store::open(root).expect("open");
        let mut request = minimal("Recurring");
        request.due_date = Some(date("2026-09-07"));
        request.recurrence = Some(RecurrenceRule::parse("FREQ=DAILY").expect("daily recurrence"));
        request.recurrence_from = Some(RecurrenceMode::Schedule);
        let task = add(&store, &request).expect("add");

        let error = complete(
            &store,
            &task.id,
            Some(date("2026-09-07")),
            &FixedClock::new(date("2026-01-01")),
        )
        .expect_err("missing done state");
        assert_eq!(error.code(), "required_terminal_state_missing");
        let unchanged = show(&store, &task.id).expect("unchanged task");
        assert_eq!(unchanged.state, "open");
        assert_eq!(unchanged.due_date, Some(date("2026-09-07")));
    }

    #[test]
    fn cancel_and_complete_require_nonterminal_tasks() {
        let (_temp, store) = store();
        let task = add(&store, &minimal("One-off")).expect("add");
        let cancelled = cancel(&store, &task.id).expect("cancel");
        assert_eq!(cancelled.state, "cancelled");
        assert!(cancel(&store, &task.id).is_err());
        let reopened = reopen(&store, &task.id).expect("reopen");
        let done = complete(
            &store,
            &reopened.id,
            Some(date("2026-09-02")),
            &FixedClock::new(date("2026-01-01")),
        )
        .expect("complete");
        assert_eq!(done.state, "done");
        assert_eq!(done.due_date, None);
    }

    #[test]
    fn explicit_repairs_and_unrelated_safe_mutations_preserve_bad_edges() {
        let (_temp, store) = store();
        let root = add(&store, &minimal("Root")).expect("root");
        let first = add(&store, &minimal("Cycle one")).expect("first");
        let second = add(&store, &minimal("Cycle two")).expect("second");
        let orphan = add(&store, &minimal("Orphan")).expect("orphan");
        let missing = "00000000000000000000000000";
        for (task, parent) in [
            (&first, second.id.as_str()),
            (&second, first.id.as_str()),
            (&orphan, missing),
        ] {
            let mut external = task.clone();
            external.parent = Some(parent.to_owned());
            fs::write(
                store.root().join(&task.path),
                serialize_task(&external, store.config()).expect("well typed bad edge"),
            )
            .expect("external graph");
        }
        assert_eq!(
            show(&store, &orphan.id)
                .expect("inspect orphan")
                .parent
                .as_deref(),
            Some(missing)
        );
        let untouched_cycle = fs::read(store.root().join(&second.path)).expect("cycle bytes");
        let edited = edit(
            &store,
            &orphan.id,
            &EditTask {
                name: Some("Still orphaned".to_owned()),
                ..EditTask::default()
            },
        )
        .expect("ordinary safe edit");
        assert_eq!(edited.parent.as_deref(), Some(missing));
        cancel(&store, &root.id).expect("independent lifecycle amid faults");
        let fixed = edit(
            &store,
            &first.id,
            &EditTask {
                parent: Some(root.id.clone()),
                ..EditTask::default()
            },
        )
        .expect("break cycle with new ancestry");
        assert_eq!(fixed.parent.as_deref(), Some(root.id.as_str()));
        assert_eq!(
            fs::read(store.root().join(&second.path)).expect("relative preserved"),
            untouched_cycle
        );
        let detached = edit(
            &store,
            &orphan.id,
            &EditTask {
                clear_parent: true,
                ..EditTask::default()
            },
        )
        .expect("detach orphan");
        assert_eq!(detached.parent, None);
        let tasks = list(
            &store,
            &TaskFilter {
                include_terminal: true,
                ..TaskFilter::default()
            },
            date("2026-09-06"),
        )
        .expect("repaired complete graph");
        assert_eq!(tasks.len(), 4);
    }

    #[test]
    fn replacing_parent_requires_valid_effective_ancestry_and_writes_nothing_on_error() {
        let (_temp, store) = store();
        let root = add(&store, &minimal("Root")).expect("root");
        let mut child = minimal("Child");
        child.parent = Some(root.id.clone());
        let child = add(&store, &child).expect("child");
        let original = fs::read(store.root().join(&root.path)).expect("original");
        let error = edit(
            &store,
            &root.id,
            &EditTask {
                parent: Some(child.id.clone()),
                ..EditTask::default()
            },
        )
        .expect_err("descendant parent");
        assert_eq!(error.code(), "parent_cycle");
        assert_eq!(
            fs::read(store.root().join(&root.path)).expect("unchanged root"),
            original
        );
        let error = edit(
            &store,
            &root.id,
            &EditTask {
                parent: Some(root.id.to_lowercase()),
                ..EditTask::default()
            },
        )
        .expect_err("self parent");
        assert_eq!(error.code(), "self_parent_reference");
        let error = edit(
            &store,
            &child.id,
            &EditTask {
                parent: Some("00000000000000000000000000".to_owned()),
                ..EditTask::default()
            },
        )
        .expect_err("absent destination");
        assert_eq!(error.code(), "task_not_found");
        assert_eq!(
            show(&store, &child.id).expect("unchanged child").parent,
            Some(root.id)
        );
    }
}
