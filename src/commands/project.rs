use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_yaml_ng::Mapping;

use crate::error::{Error, Result};
use crate::frontmatter::serialize_project;
use crate::model::{
    normalize_body, relative_path_string, validate_name, validate_project_slug, Project,
};
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct CreateProject {
    pub slug: String,
    pub name: String,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct EditProject {
    pub name: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectSummary {
    #[serde(flatten)]
    pub project: ProjectView,
    pub referencing_tasks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectView {
    pub slug: String,
    pub path: String,
    pub name: String,
    pub body: String,
    pub extra_properties: serde_json::Value,
}

impl ProjectView {
    pub fn from_project(project: &Project) -> Result<Self> {
        let extra_properties =
            serde_json::to_value(&project.extra_properties).map_err(|source| {
                Error::validation(
                    "extra_property_json_failed",
                    format!("Could not represent project properties as JSON: {source}"),
                )
                .with_path(&project.path)
            })?;
        Ok(Self {
            slug: project.slug.clone(),
            path: relative_path_string(&project.path),
            name: project.name.clone(),
            body: project.body.clone(),
            extra_properties,
        })
    }
}

pub fn create(store: &Store, request: &CreateProject) -> Result<Project> {
    store.with_exclusive_lock(|| {
        validate_project_slug(&request.slug)?;
        let name = request.name.trim().to_owned();
        validate_name(&name, "name")?;
        let body = normalize_body(&request.body);
        let mut project = Project {
            slug: request.slug.clone(),
            path: store
                .config()
                .projects_path()?
                .join(format!("{}.md", request.slug)),
            name,
            body,
            extra_properties: Mapping::new(),
        };
        let bytes = serialize_project(&project)?;
        project.path = store
            .create_project(&request.slug, &bytes)
            .map_err(|error| {
                if error.code() == "record_already_exists" {
                    Error::validation(
                        "project_already_exists",
                        format!("Project {:?} already exists", request.slug),
                    )
                    .with_path(&project.path)
                } else {
                    error
                }
            })?;
        Ok(project)
    })
}

pub fn list(store: &Store) -> Result<Vec<ProjectSummary>> {
    store.with_shared_lock(|| {
        let projects = store.load_all_projects_unlocked()?;
        let tasks = store.load_selected_tasks_unlocked(|_| Ok(true))?;
        let known = projects
            .iter()
            .map(|stored| stored.project.slug.as_str())
            .collect::<HashSet<_>>();
        validate_task_projects(&tasks, &known)?;
        let mut references = HashMap::<&str, usize>::new();
        for task in &tasks {
            for project in &task.projects {
                *references.entry(project).or_default() += 1;
            }
        }
        let mut summaries = projects
            .into_iter()
            .map(|stored| {
                Ok(ProjectSummary {
                    referencing_tasks: references
                        .get(stored.project.slug.as_str())
                        .copied()
                        .unwrap_or(0),
                    project: ProjectView::from_project(&stored.project)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        summaries.sort_by(|left, right| {
            left.project
                .name
                .cmp(&right.project.name)
                .then_with(|| left.project.slug.cmp(&right.project.slug))
        });
        Ok(summaries)
    })
}

pub fn show(store: &Store, slug: &str) -> Result<Project> {
    store.get_project(slug)
}

pub fn edit(store: &Store, slug: &str, changes: &EditProject) -> Result<Project> {
    if changes.name.is_none() && changes.body.is_none() {
        return Err(Error::usage(
            "no_changes",
            "Project edit requires at least one change",
        ));
    }
    store.with_exclusive_lock(|| {
        let stored = store.load_project_unlocked(slug)?;
        let mut project = stored.project;
        if let Some(name) = &changes.name {
            let name = name.trim().to_owned();
            validate_name(&name, "name")?;
            project.name = name;
        }
        if let Some(body) = &changes.body {
            project.body = normalize_body(body);
        }
        let bytes = serialize_project(&project)?;
        store.replace(&stored.snapshot, &bytes)?;
        Ok(project)
    })
}

pub fn delete(store: &Store, slug: &str, confirmed: bool) -> Result<Project> {
    if !confirmed {
        return Err(Error::usage(
            "confirmation_required",
            "Project deletion requires --yes",
        ));
    }
    store.with_exclusive_lock(|| {
        let stored = store.load_project_unlocked(slug)?;
        let tasks = store.load_selected_tasks_unlocked(|_| Ok(true))?;
        let mut references = tasks
            .iter()
            .filter(|task| task.projects.iter().any(|project| project == slug))
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        references.sort();
        if !references.is_empty() {
            return Err(Error::validation(
                "project_in_use",
                format!(
                    "Project {slug:?} is referenced by task(s): {}",
                    references.join(", ")
                ),
            )
            .with_path(&stored.project.path));
        }
        store.delete(&stored.snapshot)?;
        Ok(stored.project)
    })
}

pub(crate) fn validate_task_projects<'a>(
    tasks: impl IntoIterator<Item = &'a crate::model::Task>,
    known: &HashSet<&str>,
) -> Result<()> {
    for task in tasks {
        for project in &task.projects {
            if !known.contains(project.as_str()) {
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use crate::commands::init::{initialize, InitOptions};

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

    #[test]
    fn project_crud_preserves_slug_and_metadata_body_boundary() {
        let (_temp, store) = store();
        let created = create(
            &store,
            &CreateProject {
                slug: "work".to_owned(),
                name: "  Work  ".to_owned(),
                body: "Notes\r\n".to_owned(),
            },
        )
        .expect("create");
        assert_eq!(created.name, "Work");
        assert_eq!(created.body, "Notes\n");
        let edited = edit(
            &store,
            "work",
            &EditProject {
                name: Some("Office".to_owned()),
                body: None,
            },
        )
        .expect("edit");
        assert_eq!(edited.slug, "work");
        assert_eq!(edited.body, "Notes\n");
        assert_eq!(list(&store).expect("list")[0].project.name, "Office");
        delete(&store, "work", true).expect("delete");
        assert_eq!(
            show(&store, "work").expect_err("deleted").code(),
            "project_not_found"
        );
    }
}
