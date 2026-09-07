use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use clap::{error::ErrorKind as ClapErrorKind, Args, Parser, Subcommand};
use serde_json::json;

use obsidian_todo::commands::attachment;
use obsidian_todo::commands::init::{initialize, InitOptions};
use obsidian_todo::commands::project::{
    self, CreateProject, EditProject, ProjectSummary, ProjectView,
};
use obsidian_todo::commands::task::{self, AddTask, EditTask, TaskFilter, TaskView};
use obsidian_todo::discovery::{discover, DiscoveryOptions};
use obsidian_todo::error::{Error, Result, ValidationSummary};
use obsidian_todo::frontmatter::MAX_RECORD_BYTES;
use obsidian_todo::model::{relative_path_string, Clock, SystemClock, Task};
use obsidian_todo::recurrence::{parse_date, RecurrenceMode, RecurrenceRule};
use obsidian_todo::store::{upgrade, Store};
use obsidian_todo::validate::validate_store;

use crate::output::{
    human_issue, json_path, write_error, write_success, ColorChoice, CommandOutput, OutputFormat,
};

#[derive(Debug, Parser)]
#[command(
    name = "otodo",
    version,
    about = "Manage structured todo notes in an Obsidian vault",
    color = clap::ColorChoice::Never,
    after_help = "Normal operations read and write only the selected todo store and never invoke Git. Sparse checkout limits visible files but is not a credential or confidentiality boundary.\n\nExamples:\n  otodo init Todo --vault-root .\n  otodo --root Todo project create work --name Work\n  otodo --root Todo add \"Review plan\" --project work --tag review\n  otodo --root Todo list --format json\n  otodo --root Todo validate"
)]
pub struct Cli {
    /// Todo store root (takes precedence over discovery and OBSIDIAN_TODO_ROOT)
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    /// Output format; JSON is the stable scripting interface
    #[arg(long, value_enum, default_value_t = OutputFormat::Human, global = true)]
    pub format: OutputFormat,

    /// Color policy (JSON output is always color-free)
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto, global = true)]
    pub color: ColorChoice,

    /// Override the local current date for deterministic automation
    #[arg(long, value_parser = parse_cli_date, global = true)]
    pub today: Option<NaiveDate>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Report executable features without discovering or opening a store
    #[command(after_help = "Example:\n  otodo --format json capabilities")]
    Capabilities,

    /// Explicitly upgrade a store after stopping all writers and synchronization
    #[command(
        after_help = "Stop ALL local/external writers and synchronization, including old clients and clients with pending work, and safeguard that work before applying or resuming. Advisory locks cannot stop stale old writers. Schema and config are replaced separately; rerun this explicit command to resume an interrupted upgrade.\n\nExample:\n  otodo --root Todo upgrade --to 2 --dry-run"
    )]
    Upgrade {
        /// Target store schema version (only 2 is supported)
        #[arg(long)]
        to: u32,
        /// Validate and report the upgrade without changing any file
        #[arg(long)]
        dry_run: bool,
    },

    /// Initialize a self-contained todo store inside an existing vault
    #[command(after_help = "Example:\n  otodo init Todo --vault-root .")]
    Init {
        /// Store path to create, relative to the current directory unless absolute
        store_path: PathBuf,

        /// Existing Obsidian vault root; inferred from an ancestor .obsidian when omitted
        #[arg(long)]
        vault_root: Option<PathBuf>,

        /// Adopt an existing layout only when .todo, Tasks, and Projects are empty
        #[arg(long)]
        adopt_empty_layout: bool,

        /// Report all planned paths without changing the filesystem
        #[arg(long)]
        dry_run: bool,
    },

    /// Print the resolved todo store root and managed paths
    #[command(after_help = "Example:\n  otodo --root Todo root")]
    Root,

    /// Create a task
    #[command(
        after_help = "Example:\n  otodo --root Todo add \"Review plan\" --project work --tag review"
    )]
    Add(AddArguments),

    /// List tasks with composable filters
    #[command(after_help = "Example:\n  otodo --root Todo list --state active --format json")]
    List(ListArguments),

    /// Show one task by full ID or unambiguous prefix
    #[command(after_help = "Example:\n  otodo --root Todo show 01K4B0")]
    Show { id_or_prefix: String },

    /// Atomically edit one task
    #[command(after_help = "Example:\n  otodo --root Todo edit 01K4B0 --state active")]
    Edit(EditArguments),

    /// Complete an occurrence or one-off task
    #[command(after_help = "Example:\n  otodo --root Todo complete 01K4B0 --on 2026-09-09")]
    Complete {
        id_or_prefix: String,
        /// Completion date; defaults to --today or the local date
        #[arg(long, value_parser = parse_cli_date)]
        on: Option<NaiveDate>,
    },

    /// End a recurring series in the done state
    #[command(after_help = "Example:\n  otodo --root Todo finish-series 01K4B0")]
    FinishSeries { id_or_prefix: String },

    /// Move a nonterminal task to the cancelled state
    #[command(after_help = "Example:\n  otodo --root Todo cancel 01K4B0")]
    Cancel { id_or_prefix: String },

    /// Move a terminal task back to the configured default state
    #[command(after_help = "Example:\n  otodo --root Todo reopen 01K4B0")]
    Reopen { id_or_prefix: String },

    /// Intentionally and permanently delete a task
    #[command(
        after_help = "Example:\n  otodo --root Todo delete 01K4B0ZSBZZV25T1K0D3TA8JHR --yes"
    )]
    Delete {
        /// Full 26-character task ID; prefixes are not accepted
        full_id: String,
        /// Confirm non-interactive hard deletion
        #[arg(long)]
        yes: bool,
    },

    /// Import and manage ordinary file links in a task body
    Attachment {
        #[command(subcommand)]
        command: AttachmentCommand,
    },

    /// Manage first-class projects
    #[command(after_help = "Example:\n  otodo --root Todo project list")]
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },

    /// Inspect the complete store and report every discoverable problem
    #[command(after_help = "Example:\n  otodo --root Todo validate")]
    Validate,
}

#[derive(Debug, Subcommand)]
pub enum AttachmentCommand {
    /// Copy local files into fresh attachment directories and link them
    Add {
        task_id: String,
        #[arg(required = true, num_args = 1..)]
        sources: Vec<PathBuf>,
    },
    /// Link an existing store-relative Attachments/... file
    Link { task_id: String, path: String },
    /// List recognized attachments, including missing targets
    List { task_id: String },
    /// Remove this task's links, retaining the stored file
    Unlink { task_id: String, path: String },
    /// Print an existing linked attachment's absolute local path
    Path { task_id: String, path: String },
}

#[derive(Debug, Args)]
pub struct AddArguments {
    pub name: String,
    /// Import a local file (repeat for multiple files; at most 20 MiB each)
    #[arg(long = "attach")]
    pub attachments: Vec<PathBuf>,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long = "project")]
    pub projects: Vec<String>,
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    /// Full parent task ID (v2 stores only)
    #[arg(long)]
    pub parent: Option<String>,
    #[arg(long, value_parser = parse_cli_date)]
    pub due_date: Option<NaiveDate>,
    #[arg(long)]
    pub recurrence: Option<String>,
    #[arg(long)]
    pub recurrence_from: Option<String>,
    #[command(flatten)]
    pub body: BodyArguments,
}

#[derive(Debug, Args)]
pub struct ListArguments {
    /// Include terminal tasks (terminal tasks are excluded by default)
    #[arg(long = "all")]
    pub include_terminal: bool,
    /// Match any repeated state
    #[arg(long = "state")]
    pub states: Vec<String>,
    /// Require every repeated project
    #[arg(long = "project")]
    pub projects: Vec<String>,
    /// Require every repeated tag
    #[arg(long = "tag")]
    pub tags: Vec<String>,
    #[arg(long, value_parser = parse_cli_date)]
    pub due_on: Option<NaiveDate>,
    #[arg(long, value_parser = parse_cli_date)]
    pub due_before: Option<NaiveDate>,
    #[arg(long, value_parser = parse_cli_date)]
    pub due_after: Option<NaiveDate>,
    #[arg(long)]
    pub overdue: bool,
    #[arg(long, conflicts_with = "non_recurring")]
    pub recurring: bool,
    #[arg(long, conflicts_with = "recurring")]
    pub non_recurring: bool,
    /// Select direct children of a full task ID (v2 stores only)
    #[arg(long, conflicts_with = "roots")]
    pub parent: Option<String>,
    /// Select tasks without a parent (v2 stores only)
    #[arg(long, conflicts_with = "parent")]
    pub roots: bool,
    /// Literal case-insensitive substring of task name or full ID
    #[arg(long)]
    pub query: Option<String>,
    /// Return compact task candidates instead of full task records
    #[arg(long)]
    pub summary: bool,
    /// Maximum compact matches, from 1 through 1000
    #[arg(long, requires = "summary", value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub limit: Option<u32>,
}

#[derive(Debug, Args)]
pub struct EditArguments {
    pub id_or_prefix: String,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub state: Option<String>,
    #[arg(long = "add-project")]
    pub add_projects: Vec<String>,
    #[arg(long = "remove-project")]
    pub remove_projects: Vec<String>,
    #[arg(long = "add-tag")]
    pub add_tags: Vec<String>,
    #[arg(long = "remove-tag")]
    pub remove_tags: Vec<String>,
    /// Set or replace the full parent task ID (v2 stores only)
    #[arg(long, conflicts_with = "clear_parent")]
    pub parent: Option<String>,
    /// Detach this task from its parent (v2 stores only)
    #[arg(long, conflicts_with = "parent")]
    pub clear_parent: bool,
    #[arg(long, value_parser = parse_cli_date, conflicts_with = "clear_due_date")]
    pub due_date: Option<NaiveDate>,
    #[arg(long)]
    pub clear_due_date: bool,
    #[arg(long, conflicts_with = "clear_recurrence")]
    pub recurrence: Option<String>,
    #[arg(long, conflicts_with = "clear_recurrence")]
    pub recurrence_from: Option<String>,
    #[arg(long)]
    pub clear_recurrence: bool,
    #[command(flatten)]
    pub body: BodyArguments,
}

#[derive(Debug, Args)]
pub struct BodyArguments {
    /// Markdown body text
    #[arg(long, conflicts_with = "body_file")]
    pub body: Option<String>,
    /// Read Markdown body from a UTF-8 file, or '-' for standard input
    #[arg(long, value_name = "PATH", conflicts_with = "body")]
    pub body_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a project with a stable slug
    #[command(after_help = "Example:\n  otodo --root Todo project create work --name Work")]
    Create {
        slug: String,
        #[arg(long)]
        name: String,
        #[command(flatten)]
        body: BodyArguments,
    },
    /// List projects and current task reference counts
    #[command(after_help = "Example:\n  otodo --root Todo project list")]
    List,
    /// Show a project by exact slug
    #[command(after_help = "Example:\n  otodo --root Todo project show work")]
    Show { slug: String },
    /// Edit a project's display name or body without changing its slug
    #[command(
        after_help = "Example:\n  otodo --root Todo project edit work --name \"Office Work\""
    )]
    Edit {
        slug: String,
        #[arg(long)]
        name: Option<String>,
        #[command(flatten)]
        body: BodyArguments,
    },
    /// Delete an unreferenced project
    #[command(after_help = "Example:\n  otodo --root Todo project delete work --yes")]
    Delete {
        slug: String,
        #[arg(long)]
        yes: bool,
    },
}

pub fn run() -> u8 {
    let arguments = env::args_os().collect::<Vec<_>>();
    let format = requested_format(&arguments);
    run_with_arguments(&arguments, format)
}

fn run_with_arguments(arguments: &[OsString], format: OutputFormat) -> u8 {
    match Cli::try_parse_from(arguments) {
        Ok(cli) => {
            let selected_format = cli.format;
            match execute(&cli, &SystemClock) {
                Ok(output) => {
                    if let Err(error) =
                        write_success(&output, selected_format, &mut io::stdout().lock())
                    {
                        write_error(&error, selected_format, &mut io::stderr().lock());
                        error.exit_code()
                    } else {
                        0
                    }
                }
                Err(error) => {
                    write_error(&error, selected_format, &mut io::stderr().lock());
                    error.exit_code()
                }
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                ClapErrorKind::DisplayHelp | ClapErrorKind::DisplayVersion
            ) =>
        {
            let _ = io::stdout().write_all(error.to_string().as_bytes());
            0
        }
        Err(error) => {
            let application_error = Error::usage("usage_error", error.to_string());
            write_error(&application_error, format, &mut io::stderr().lock());
            application_error.exit_code()
        }
    }
}

pub fn execute(cli: &Cli, clock: &dyn Clock) -> Result<CommandOutput> {
    if matches!(cli.command, Command::Capabilities) {
        return Ok(CommandOutput::new(
            "Store schema versions: 1, 2\nFeatures: subtasks, task_candidates, store_upgrade, attachments"
                .to_owned(),
            json!({
                "version": 1,
                "store_schema_versions": [1, 2],
                "features": ["subtasks", "task_candidates", "store_upgrade", "attachments"],
            }),
        ));
    }
    let current_directory = env::current_dir()
        .map_err(|source| Error::io("read the current directory", Path::new("."), &source))?;
    if let Command::Init {
        store_path,
        vault_root,
        adopt_empty_layout,
        dry_run,
    } = &cli.command
    {
        let plan = initialize(&InitOptions {
            store_path,
            vault_root: vault_root.as_deref(),
            current_directory: &current_directory,
            adopt_empty_layout: *adopt_empty_layout,
            dry_run: *dry_run,
        })?;
        let action = if plan.dry_run {
            "Would initialize"
        } else {
            "Initialized"
        };
        return Ok(CommandOutput::new(
            format!("{action} todo store at {}", plan.store_root.display()),
            json!({
                "version": 1,
                "dry_run": plan.dry_run,
                "vault_root": json_path(&plan.vault_root),
                "root": json_path(&plan.store_root),
                "directories": plan.directories.iter().map(|path| json_path(path)).collect::<Vec<_>>(),
                "files": plan.files.iter().map(|path| json_path(path)).collect::<Vec<_>>(),
                "obsidian_link_prefix": plan.obsidian_link_prefix,
            }),
        ));
    }

    let root = if matches!(cli.command, Command::Validate) {
        validation_root(cli, &current_directory)?
    } else {
        discover_root(cli, &current_directory)?
    };
    if let Command::Upgrade { to, dry_run } = &cli.command {
        let result = upgrade(&root, *to, *dry_run)?;
        return Ok(CommandOutput::new(
            format!(
                "Store upgrade {}: {} -> {}{}\nSchema: {}\nConfig: {}",
                result.status,
                result.from,
                result.to,
                if result.dry_run { " (dry run)" } else { "" },
                root.join(".todo/schema.json").display(),
                root.join(".todo/config.toml").display(),
            ),
            json!({ "version": 1, "upgrade": result }),
        ));
    }
    if matches!(cli.command, Command::Validate) {
        let report = validate_store(&root);
        if !report.valid {
            let summary = ValidationSummary {
                valid: report.valid,
                errors: report.errors,
                warnings: report.warnings,
                tasks: report.tasks,
                projects: report.projects,
            };
            return Err(Error::from_issues(report.issues).with_validation_summary(summary));
        }
        let mut human = format!(
            "Valid todo store: {} task(s), {} project(s), {} warning(s)",
            report.tasks, report.projects, report.warnings
        );
        for issue in &report.issues {
            human.push_str("\n  ");
            human.push_str(&human_issue(issue));
            if let Some(suggestion) = &issue.suggestion {
                human.push_str(&format!("\n    fix: {suggestion}"));
            }
        }
        return Ok(CommandOutput::new(
            human,
            json!({
                "version": 1,
                "valid": report.valid,
                "errors": report.errors,
                "warnings": report.warnings,
                "tasks": report.tasks,
                "projects": report.projects,
                "issues": report.issues,
            }),
        ));
    }

    let store = Store::open(&root)?;
    match &cli.command {
        Command::Init { .. }
        | Command::Validate
        | Command::Capabilities
        | Command::Upgrade { .. } => {
            unreachable!("handled before store opening")
        }
        Command::Root => {
            let paths = store.paths();
            Ok(CommandOutput::new(
                root.display().to_string(),
                json!({
                    "version": 1,
                    "root": json_path(&paths.root),
                    "config": json_path(&paths.config),
                    "tasks": json_path(&paths.tasks),
                    "projects": json_path(&paths.projects),
                }),
            ))
        }
        Command::Add(arguments) => {
            let recurrence = arguments
                .recurrence
                .as_deref()
                .map(RecurrenceRule::parse)
                .transpose()?;
            let recurrence_from = arguments
                .recurrence_from
                .as_deref()
                .map(RecurrenceMode::parse)
                .transpose()?;
            let body = read_body(&arguments.body)?.unwrap_or_default();
            let task = task::add_with_attachments(
                &store,
                &AddTask {
                    name: arguments.name.clone(),
                    state: arguments.state.clone(),
                    projects: arguments.projects.clone(),
                    tags: arguments.tags.clone(),
                    parent: arguments.parent.clone(),
                    due_date: arguments.due_date,
                    recurrence,
                    recurrence_from,
                    body,
                },
                &arguments.attachments,
            )?;
            task_output(&store, &task, format!("{} {}", task.id, task.name))
        }
        Command::Attachment { command } => {
            let views = match command {
                AttachmentCommand::Add { task_id, sources } => {
                    attachment::add(&store, task_id, sources)?
                }
                AttachmentCommand::Link { task_id, path } => {
                    vec![attachment::link(&store, task_id, path)?]
                }
                AttachmentCommand::List { task_id } => attachment::list(&store, task_id)?,
                AttachmentCommand::Unlink { task_id, path } => {
                    vec![attachment::unlink(&store, task_id, path)?]
                }
                AttachmentCommand::Path { task_id, path } => {
                    let absolute = attachment::path(&store, task_id, path)?;
                    return Ok(CommandOutput::new(
                        absolute.display().to_string(),
                        json!({"version": 1, "path": json_path(&absolute)}),
                    ));
                }
            };
            let human = if views.is_empty() {
                "No recognized attachments. Use explicit Attachments/... links for manually placed files.".to_owned()
            } else {
                views
                    .iter()
                    .map(|view| {
                        format!(
                            "{}  {}  {} bytes  {}",
                            view.path,
                            view.display_name,
                            view.byte_size
                                .map_or_else(|| "unknown".to_owned(), |size| size.to_string()),
                            view.availability
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            Ok(CommandOutput::new(
                human,
                json!({"version": 1, "attachments": views}),
            ))
        }
        Command::List(arguments) => {
            let today = cli.today.unwrap_or_else(|| clock.today());
            let tasks = task::list(
                &store,
                &TaskFilter {
                    include_terminal: arguments.include_terminal,
                    states: arguments.states.clone(),
                    projects: arguments.projects.clone(),
                    tags: arguments.tags.clone(),
                    due_on: arguments.due_on,
                    due_before: arguments.due_before,
                    due_after: arguments.due_after,
                    overdue: arguments.overdue,
                    recurring: arguments.recurring,
                    non_recurring: arguments.non_recurring,
                    parent: arguments.parent.clone(),
                    roots: arguments.roots,
                    query: arguments.query.clone(),
                },
                today,
            )?;
            if arguments.summary {
                Ok(task_summary_output(&store, &tasks, arguments.limit))
            } else {
                task_list_output(&store, &tasks)
            }
        }
        Command::Show { id_or_prefix } => {
            let task = task::show(&store, id_or_prefix)?;
            let human = human_task_details(&store, &task)?;
            task_output(&store, &task, human)
        }
        Command::Edit(arguments) => {
            let recurrence = arguments
                .recurrence
                .as_deref()
                .map(RecurrenceRule::parse)
                .transpose()?;
            let recurrence_from = arguments
                .recurrence_from
                .as_deref()
                .map(RecurrenceMode::parse)
                .transpose()?;
            let body = read_body(&arguments.body)?;
            let task = task::edit(
                &store,
                &arguments.id_or_prefix,
                &EditTask {
                    name: arguments.name.clone(),
                    state: arguments.state.clone(),
                    add_projects: arguments.add_projects.clone(),
                    remove_projects: arguments.remove_projects.clone(),
                    add_tags: arguments.add_tags.clone(),
                    remove_tags: arguments.remove_tags.clone(),
                    parent: arguments.parent.clone(),
                    clear_parent: arguments.clear_parent,
                    due_date: arguments.due_date,
                    clear_due_date: arguments.clear_due_date,
                    recurrence,
                    recurrence_from,
                    clear_recurrence: arguments.clear_recurrence,
                    body,
                },
            )?;
            task_output(&store, &task, format!("Updated {} {}", task.id, task.name))
        }
        Command::Complete { id_or_prefix, on } => {
            let task = task::complete(&store, id_or_prefix, on.or(cli.today), clock)?;
            task_output(
                &store,
                &task,
                format!("Completed {} {}", task.id, task.name),
            )
        }
        Command::FinishSeries { id_or_prefix } => {
            let task = task::finish_series(&store, id_or_prefix)?;
            task_output(
                &store,
                &task,
                format!("Finished series {} {}", task.id, task.name),
            )
        }
        Command::Cancel { id_or_prefix } => {
            let task = task::cancel(&store, id_or_prefix)?;
            task_output(
                &store,
                &task,
                format!("Cancelled {} {}", task.id, task.name),
            )
        }
        Command::Reopen { id_or_prefix } => {
            let task = task::reopen(&store, id_or_prefix)?;
            task_output(&store, &task, format!("Reopened {} {}", task.id, task.name))
        }
        Command::Delete { full_id, yes } => {
            let task = task::delete(&store, full_id, *yes)?;
            task_output(
                &store,
                &task,
                format!(
                    "Deleted {} {}; recovery requires external Git history or backups",
                    task.id, task.name
                ),
            )
        }
        Command::Project { command } => execute_project(&store, command),
    }
}

pub fn discover_root(cli: &Cli, current_directory: &Path) -> Result<PathBuf> {
    let environment_root = env::var_os("OBSIDIAN_TODO_ROOT");
    discover(&DiscoveryOptions {
        explicit_root: cli.root.as_deref(),
        environment_root: environment_root.as_deref(),
        current_directory,
    })
}
fn validation_root(cli: &Cli, current_directory: &Path) -> Result<PathBuf> {
    let (supplied, source_name) = if let Some(root) = &cli.root {
        (root.clone(), "--root")
    } else if let Some(root) = env::var_os("OBSIDIAN_TODO_ROOT") {
        if root.is_empty() {
            return Err(Error::usage(
                "invalid_root",
                "OBSIDIAN_TODO_ROOT cannot be empty",
            ));
        }
        (PathBuf::from(root), "OBSIDIAN_TODO_ROOT")
    } else {
        return discover_root(cli, current_directory);
    };
    let candidate = if supplied.is_absolute() {
        supplied
    } else {
        current_directory.join(supplied)
    };
    match fs::symlink_metadata(&candidate) {
        Ok(_) => Ok(candidate),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Err(Error::not_found(
            "store_not_found",
            format!("{source_name} does not identify an existing path"),
        )
        .with_path(candidate)),
        Err(source) => Err(Error::io(
            "inspect the requested validation root",
            &candidate,
            &source,
        )),
    }
}

fn execute_project(store: &Store, command: &ProjectCommand) -> Result<CommandOutput> {
    match command {
        ProjectCommand::Create { slug, name, body } => {
            let project = project::create(
                store,
                &CreateProject {
                    slug: slug.clone(),
                    name: name.clone(),
                    body: read_body(body)?.unwrap_or_default(),
                },
            )?;
            project_output(
                &project,
                format!("Created project {} {}", project.slug, project.name),
            )
        }
        ProjectCommand::List => {
            let projects = project::list(store)?;
            let human = human_project_list(&projects);
            Ok(CommandOutput::new(
                human,
                json!({ "version": 1, "projects": projects }),
            ))
        }
        ProjectCommand::Show { slug } => {
            let project = project::show(store, slug)?;
            let view = ProjectView::from_project(&project)?;
            let human = format!(
                "slug: {}\npath: {}\nname: {}\nextra_properties: {}\nbody:\n{}",
                project.slug,
                project.path.display(),
                project.name,
                view.extra_properties,
                project.body
            );
            project_output(&project, human)
        }
        ProjectCommand::Edit { slug, name, body } => {
            let project = project::edit(
                store,
                slug,
                &EditProject {
                    name: name.clone(),
                    body: read_body(body)?,
                },
            )?;
            project_output(
                &project,
                format!("Updated project {} {}", project.slug, project.name),
            )
        }
        ProjectCommand::Delete { slug, yes } => {
            let project = project::delete(store, slug, *yes)?;
            project_output(
                &project,
                format!("Deleted project {} {}", project.slug, project.name),
            )
        }
    }
}

fn task_output(store: &Store, task: &Task, human: String) -> Result<CommandOutput> {
    let view = TaskView::from_task(task, store)?;
    Ok(CommandOutput::new(
        human,
        json!({ "version": 1, "task": view }),
    ))
}

fn task_list_output(store: &Store, tasks: &[Task]) -> Result<CommandOutput> {
    let views = tasks
        .iter()
        .map(|task| TaskView::from_task(task, store))
        .collect::<Result<Vec<_>>>()?;
    let human = tasks
        .iter()
        .map(|task| {
            let due = task.due_date.map_or_else(
                || "-".to_owned(),
                |date| date.format("%Y-%m-%d").to_string(),
            );
            let recurring = if task.recurrence.is_some() {
                " ↻"
            } else {
                ""
            };
            let projects = if task.projects.is_empty() {
                "-".to_owned()
            } else {
                task.projects.join(",")
            };
            format!(
                "{}  {:<10}  {}{}  [{}]  {}",
                task.id, task.state, due, recurring, projects, task.name
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(CommandOutput::new(
        human,
        json!({ "version": 1, "tasks": views }),
    ))
}

fn task_summary_output(store: &Store, tasks: &[Task], limit: Option<u32>) -> CommandOutput {
    let count = limit.map_or(tasks.len(), |limit| (limit as usize).min(tasks.len()));
    let has_more = count < tasks.len();
    let tasks = &tasks[..count];
    let views = tasks
        .iter()
        .map(|task| {
            json!({
                "id": task.id,
                "path": relative_path_string(&task.path),
                "name": task.name,
                "state": task.state,
                "terminal": task.terminal(store.config()),
                "parent": task.parent,
            })
        })
        .collect::<Vec<_>>();
    let mut human = tasks
        .iter()
        .map(|task| format!("{}  {:<10}  {}", task.id, task.state, task.name))
        .collect::<Vec<_>>()
        .join("\n");
    if has_more {
        human.push_str("\nMore matching tasks; refine the query or increase --limit.");
    }
    CommandOutput::new(
        human,
        json!({ "version": 1, "tasks": views, "has_more": has_more }),
    )
}

fn human_task_details(store: &Store, task: &Task) -> Result<String> {
    let view = TaskView::from_task(task, store)?;
    Ok(format!(
        "id: {}\npath: {}\nname: {}\nstate: {}\nterminal: {}\nprojects: {}\ntags: {}\nparent: {}\ndue_date: {}\nrecurrence: {}\nrecurrence_from: {}\nlast_completed_date: {}\nextra_properties: {}\nbody:\n{}",
        view.id,
        view.path,
        view.name,
        view.state,
        view.terminal,
        view.projects.join(", "),
        view.tags.join(", "),
        task.parent.as_deref().unwrap_or("-"),
        view.due_date.as_deref().unwrap_or("-"),
        view.recurrence.as_deref().unwrap_or("-"),
        view.recurrence_from.as_deref().unwrap_or("-"),
        view.last_completed_date.as_deref().unwrap_or("-"),
        view.extra_properties,
        view.body
    ))
}

fn project_output(project: &obsidian_todo::model::Project, human: String) -> Result<CommandOutput> {
    let view = ProjectView::from_project(project)?;
    Ok(CommandOutput::new(
        human,
        json!({ "version": 1, "project": view }),
    ))
}

fn human_project_list(projects: &[ProjectSummary]) -> String {
    projects
        .iter()
        .map(|project| {
            format!(
                "{}  {:>4}  {}",
                project.project.slug, project.referencing_tasks, project.project.name
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_body(arguments: &BodyArguments) -> Result<Option<String>> {
    if let Some(body) = &arguments.body {
        return Ok(Some(body.clone()));
    }
    let Some(path) = &arguments.body_file else {
        return Ok(None);
    };
    let bytes = if path == Path::new("-") {
        read_bounded_body(io::stdin().lock(), path, "standard input")?
    } else {
        let file =
            fs::File::open(path).map_err(|source| Error::io("open a body file", path, &source))?;
        read_bounded_body(file, path, "body file")?
    };
    String::from_utf8(bytes).map(Some).map_err(|_| {
        Error::validation("invalid_body_utf8", "Task and project bodies must be UTF-8")
            .with_path(path)
    })
}
fn read_bounded_body(reader: impl Read, path: &Path, description: &str) -> Result<Vec<u8>> {
    let mut reader = reader.take((MAX_RECORD_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| Error::io(&format!("read a body from {description}"), path, &source))?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err(Error::validation(
            "body_too_large",
            format!("Body exceeds the {MAX_RECORD_BYTES}-byte record limit"),
        )
        .with_path(path));
    }
    Ok(bytes)
}

fn parse_cli_date(value: &str) -> std::result::Result<NaiveDate, String> {
    parse_date(value, "date").map_err(|error| error.message().to_owned())
}

fn requested_format(arguments: &[OsString]) -> OutputFormat {
    let mut requested = OutputFormat::Human;
    let mut arguments = arguments.iter().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--" {
            break;
        }
        if argument == "--format" {
            if arguments
                .next()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("json"))
            {
                requested = OutputFormat::Json;
            }
            continue;
        }
        if argument
            .to_str()
            .and_then(|value| value.strip_prefix("--format="))
            .is_some_and(|value| value.eq_ignore_ascii_case("json"))
        {
            requested = OutputFormat::Json;
        }
    }
    requested
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_json_format_in_either_clap_spelling() {
        assert_eq!(
            requested_format(&["otodo".into(), "--format".into(), "json".into()]),
            OutputFormat::Json
        );
        assert_eq!(
            requested_format(&["otodo".into(), "root".into(), "--format=json".into()]),
            OutputFormat::Json
        );

        assert_eq!(
            requested_format(&[
                "otodo".into(),
                "add".into(),
                "--".into(),
                "--format".into(),
                "json".into(),
            ]),
            OutputFormat::Human
        );
    }

    #[test]
    fn all_commands_have_help() {
        for arguments in [
            vec!["otodo", "--help"],
            vec!["otodo", "init", "--help"],
            vec!["otodo", "capabilities", "--help"],
            vec!["otodo", "upgrade", "--help"],
            vec!["otodo", "root", "--help"],
            vec!["otodo", "add", "--help"],
            vec!["otodo", "list", "--help"],
            vec!["otodo", "show", "--help"],
            vec!["otodo", "edit", "--help"],
            vec!["otodo", "complete", "--help"],
            vec!["otodo", "finish-series", "--help"],
            vec!["otodo", "cancel", "--help"],
            vec!["otodo", "reopen", "--help"],
            vec!["otodo", "delete", "--help"],
            vec!["otodo", "project", "--help"],
            vec!["otodo", "project", "create", "--help"],
            vec!["otodo", "project", "list", "--help"],
            vec!["otodo", "project", "show", "--help"],
            vec!["otodo", "project", "edit", "--help"],
            vec!["otodo", "project", "delete", "--help"],
            vec!["otodo", "validate", "--help"],
        ] {
            let error = Cli::try_parse_from(arguments).expect_err("help exits through clap");
            assert_eq!(error.kind(), ClapErrorKind::DisplayHelp);
            assert!(
                error.to_string().contains("Example"),
                "command help lacks an example: {error}"
            );
        }
    }

    #[test]
    fn top_level_help_documents_globals_examples_and_sparse_boundary() {
        let help = Cli::try_parse_from(["otodo", "--help"])
            .expect_err("help")
            .to_string();
        for required in [
            "--root",
            "--format",
            "--color",
            "--today",
            "Examples:",
            "never invoke Git",
            "not a credential or confidentiality boundary",
        ] {
            assert!(help.contains(required), "missing {required:?}:\n{help}");
        }
    }
    #[test]
    fn body_reader_stops_at_record_size_limit() {
        let error = read_bounded_body(std::io::repeat(b'x'), Path::new("-"), "test input")
            .expect_err("oversized body");
        assert_eq!(error.code(), "body_too_large");
        assert_eq!(error.path(), Some(Path::new("-")));
    }
}
