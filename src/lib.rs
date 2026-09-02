#![forbid(unsafe_code)]

pub mod commands;
pub mod config;
pub mod discovery;
pub mod error;
pub mod frontmatter;
pub mod model;
pub mod recurrence;
pub mod store;
pub mod validate;

pub use commands::init::{initialize, InitOptions, InitPlan};
pub use commands::project::{
    create as create_project, delete as delete_project, edit as edit_project,
    list as list_projects, show as show_project, CreateProject, EditProject, ProjectSummary,
    ProjectView,
};
pub use commands::task::{
    add as add_task, cancel as cancel_task, complete as complete_task, delete as delete_task,
    edit as edit_task, finish_series, list as list_tasks, reopen as reopen_task, show as show_task,
    AddTask, EditTask, TaskFilter, TaskView,
};
pub use config::{Config, State};
pub use discovery::{discover, DiscoveryOptions};
pub use error::{Error, ErrorKind, Result, ValidationIssue};
pub use model::{Clock, FixedClock, Project, SystemClock, Task};
pub use recurrence::{Frequency, RecurrenceMode, RecurrenceRule};
pub use store::{Store, StorePaths};
pub use validate::{validate_store, ValidationReport};

pub mod cli;
pub mod output;
