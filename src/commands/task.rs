use std::collections::HashSet;

use chrono::NaiveDate;
use serde::Serialize;
use serde_yaml_ng::{Mapping, Value};
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::frontmatter::serialize_task;
use crate::model::{
    normalize_body, relative_path_string, validate_name, validate_project_slug, validate_tag,
    validate_unique_projects, Clock, Task,
};
use crate::recurrence::{RecurrenceMode, RecurrenceRule};
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct AddTask {
    pub name: String,
    pub state: Option<String>,
    pub projects: Vec<String>,
    pub tags: Vec<String>,
    pub due_date: Option<NaiveDate>,
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
    pub recurrence: Option<RecurrenceRule>,
    pub recurrence_from: Option<RecurrenceMode>,
    pub clear_recurrence: bool,
    pub body: Option<String>,
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
    pub due_date: Option<String>,
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
            due_date: task
                .due_date
                .map(|date| date.format("%Y-%m-%d").to_string()),
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
    store.with_exclusive_lock(|| {
        let name = request.name.trim().to_owned();
        validate_name(&name, "name")?;
        let known_projects = known_projects(store)?;
        ensure_requested_projects(&request.projects, &known_projects)?;
        let existing_ids = store.task_ids_unlocked()?;
        let mut extra_properties = Mapping::new();
        extra_properties.insert(
            Value::String("base".to_owned()),
            Value::String(store.config().todos_base_link()),
        );

        for _ in 0..32 {
            let id = Ulid::new().to_string().to_ascii_uppercase();
            if existing_ids.contains(id.as_str()) {
                continue;
            }
            let path = store.config().tasks_path()?.join(format!("{id}.md"));
            let task = Task {
                id: id.clone(),
                path,
                name: name.clone(),
                state: request
                    .state
                    .clone()
                    .unwrap_or_else(|| store.config().default_state.clone()),
                projects: request.projects.clone(),
                tags: request.tags.clone(),
                due_date: request.due_date,
                recurrence: request.recurrence.clone(),
                recurrence_from: request.recurrence_from,
                last_completed_date: None,
                body: normalize_body(&request.body),
                extra_properties: extra_properties.clone(),
            };
            task.validate(store.config())?;
            let bytes = serialize_task(&task, store.config())?;
            match store.create_task(&id, &bytes) {
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
    store.with_shared_lock(|| {
        let known_projects = known_projects(store)?;
        ensure_known_projects(&filter.projects, &known_projects)?;
        let mut tasks = store.load_selected_tasks_unlocked(|task| {
            validate_task_references(std::iter::once(task), &known_projects)?;
            Ok(matches_filter(task, filter, store, today))
        })?;
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
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        let known_projects = known_projects(store)?;
        apply_edit(&mut task, changes, &known_projects)?;
        task.validate(store.config())?;
        let bytes = serialize_task(&task, store.config())?;
        store.replace(&stored.snapshot, &bytes)?;
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
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
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
        store.replace(&stored.snapshot, &bytes)?;
        Ok(task)
    })
}

pub fn finish_series(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_nonterminal(&task, store)?;
        ensure_task_projects(store, &task)?;
        if task.recurrence.is_none() {
            return Err(Error::validation(
                "task_not_recurring",
                "finish-series requires a recurring task",
            ));
        }
        task.state = required_terminal_state(store, "done", "finish-series")?.to_owned();
        replace_task(store, stored.snapshot, task)
    })
}

pub fn cancel(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_nonterminal(&task, store)?;
        ensure_task_projects(store, &task)?;
        task.state = required_terminal_state(store, "cancelled", "cancel")?.to_owned();
        replace_task(store, stored.snapshot, task)
    })
}

pub fn reopen(store: &Store, id_or_prefix: &str) -> Result<Task> {
    store.with_exclusive_lock(|| {
        let stored = store.resolve_task_unlocked(id_or_prefix)?;
        let mut task = stored.task;
        ensure_task_projects(store, &task)?;
        if !task.terminal(store.config()) {
            return Err(Error::validation(
                "task_not_terminal",
                "reopen requires a task in a terminal state",
            )
            .with_field("state"));
        }
        task.state.clone_from(&store.config().default_state);
        replace_task(store, stored.snapshot, task)
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
        let stored = store.resolve_task_unlocked(full_id)?;
        store.delete(&stored.snapshot)?;
        Ok(stored.task)
    })
}

fn apply_edit(task: &mut Task, changes: &EditTask, known_projects: &HashSet<String>) -> Result<()> {
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
    } else if let Some(due_date) = changes.due_date {
        task.due_date = Some(due_date);
    }
    if let Some(body) = &changes.body {
        task.body = normalize_body(body);
    }
    Ok(())
}

fn validate_edit_request(changes: &EditTask) -> Result<()> {
    let has_change = changes.name.is_some()
        || changes.state.is_some()
        || !changes.add_projects.is_empty()
        || !changes.remove_projects.is_empty()
        || !changes.add_tags.is_empty()
        || !changes.remove_tags.is_empty()
        || changes.due_date.is_some()
        || changes.clear_due_date
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
    if changes.due_date.is_some() && changes.clear_due_date {
        return Err(Error::usage(
            "conflicting_changes",
            "--due-date conflicts with --clear-due-date",
        ));
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

fn replace_task(store: &Store, snapshot: crate::store::FileSnapshot, task: Task) -> Result<Task> {
    task.validate(store.config())?;
    let bytes = serialize_task(&task, store.config())?;
    store.replace(&snapshot, &bytes)?;
    Ok(task)
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
            due_date: None,
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
        assert_eq!(object.len(), 13);
        for field in [
            "due_date",
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
        scheduled.recurrence =
            Some(RecurrenceRule::parse("FREQ=WEEKLY;BYDAY=MO").expect("weekly recurrence"));
        scheduled.recurrence_from = Some(RecurrenceMode::Schedule);
        let scheduled = add(&store, &scheduled).expect("scheduled");
        let scheduled = complete(&store, &scheduled.id, None, &clock).expect("complete");
        assert_eq!(scheduled.due_date, Some(date("2026-09-14")));
        assert_eq!(scheduled.state, "open");
        assert_eq!(scheduled.last_completed_date, Some(date("2026-09-09")));

        let mut relative = minimal("Relative");
        relative.due_date = Some(date("2026-09-07"));
        relative.recurrence =
            Some(RecurrenceRule::parse("FREQ=DAILY;INTERVAL=3").expect("daily recurrence"));
        relative.recurrence_from = Some(RecurrenceMode::Completion);
        let relative = add(&store, &relative).expect("relative");
        let relative = complete(&store, &relative.id, None, &clock).expect("complete");
        assert_eq!(relative.due_date, Some(date("2026-09-12")));
        let finished = finish_series(&store, &relative.id).expect("finish");
        assert_eq!(finished.state, "done");
        assert!(finished.recurrence.is_some());
        let reopened = reopen(&store, &relative.id).expect("reopen");
        assert_eq!(reopened.state, "open");
        assert_eq!(reopened.due_date, Some(date("2026-09-12")));
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
}
