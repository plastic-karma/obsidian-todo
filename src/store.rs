use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::config::{managed_paths_overlap, Config, CONFIG_PATH, EMBEDDED_SCHEMA, SCHEMA_PATH};
use crate::error::{Error, ErrorKind, Result};
use crate::frontmatter::{parse_project, parse_task, MAX_RECORD_BYTES};
use crate::model::{validate_id_prefix, validate_project_slug, validate_task_id, Project, Task};

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    config: Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePaths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub schema: PathBuf,
    pub tasks: PathBuf,
    pub projects: PathBuf,
}

#[derive(Debug)]
pub(crate) struct FileSnapshot {
    path: PathBuf,
    relative_path: PathBuf,
    bytes: Vec<u8>,
    hash: [u8; 32],
    length: u64,
    permissions: Permissions,
}

impl FileSnapshot {
    fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

#[derive(Debug)]
pub(crate) struct StoredTask {
    pub task: Task,
    pub snapshot: FileSnapshot,
}

#[derive(Debug)]
pub(crate) struct StoredProject {
    pub project: Project,
    pub snapshot: FileSnapshot,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let supplied = root.as_ref();
        let metadata = fs::symlink_metadata(supplied)
            .map_err(|source| Error::io("inspect the todo store root", supplied, &source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::validation(
                "invalid_store_root",
                "Todo store root must be a real directory",
            )
            .with_path(supplied));
        }
        let root = fs::canonicalize(supplied)
            .map_err(|source| Error::io("resolve the todo store root", supplied, &source))?;
        ensure_real_directory(&root, Path::new(".todo"))?;
        let config_path = root.join(CONFIG_PATH);
        ensure_regular_file(&config_path, CONFIG_PATH)?;
        let source = read_bounded(&config_path, MAX_CONFIG_BYTES, "configuration")?;
        let source = String::from_utf8(source).map_err(|_| {
            Error::validation("invalid_config_utf8", "Configuration must be UTF-8")
                .with_path(CONFIG_PATH)
        })?;
        let config = Config::parse(&source)?;
        let tasks_path = config.tasks_path()?;
        let projects_path = config.projects_path()?;
        validate_managed_directory_on_disk(&root, &tasks_path)?;
        validate_managed_directory_on_disk(&root, &projects_path)?;
        let tasks_absolute = fs::canonicalize(root.join(&tasks_path))
            .map_err(|source| Error::io("resolve the tasks directory", &root, &source))?;
        let projects_absolute = fs::canonicalize(root.join(&projects_path))
            .map_err(|source| Error::io("resolve the projects directory", &root, &source))?;
        let metadata_absolute = fs::canonicalize(root.join(".todo"))
            .map_err(|source| Error::io("resolve the metadata directory", &root, &source))?;
        if managed_paths_overlap(&tasks_absolute, &projects_absolute)
            || managed_paths_overlap(&tasks_absolute, &metadata_absolute)
            || managed_paths_overlap(&projects_absolute, &metadata_absolute)
        {
            return Err(Error::validation(
                "managed_paths_not_distinct",
                "Managed directories must be distinct from .todo and from one another",
            ));
        }
        validate_schema(&root, config.schema_version)?;
        Ok(Self { root, config })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.config
    }

    #[must_use]
    pub fn paths(&self) -> StorePaths {
        StorePaths {
            root: self.root.clone(),
            config: self.root.join(CONFIG_PATH),
            schema: self.root.join(SCHEMA_PATH),
            tasks: self.root.join(&self.config.tasks_directory),
            projects: self.root.join(&self.config.projects_directory),
        }
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        self.with_shared_lock(|| self.load_selected_tasks_unlocked(|_| Ok(true)))
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        self.with_shared_lock(|| {
            self.load_all_projects_unlocked()
                .map(|stored| stored.into_iter().map(|stored| stored.project).collect())
        })
    }

    pub fn get_task(&self, id_or_prefix: &str) -> Result<Task> {
        self.with_shared_lock(|| {
            self.resolve_task_unlocked(id_or_prefix)
                .map(|stored| stored.task)
        })
    }

    pub fn get_project(&self, slug: &str) -> Result<Project> {
        self.with_shared_lock(|| {
            self.load_project_unlocked(slug)
                .map(|stored| stored.project)
        })
    }

    pub(crate) fn load_selected_tasks_unlocked<F>(&self, select: F) -> Result<Vec<Task>>
    where
        F: Fn(&Task) -> Result<bool> + Sync,
    {
        const TASKS_PER_WORKER: usize = 256;
        const MAX_WORKERS: usize = 8;

        let candidates = self.task_candidates()?;
        let mut seen = HashMap::<&str, &Path>::with_capacity(candidates.len());
        for (id, relative) in &candidates {
            if let Some(first) = seen.insert(id, relative) {
                return Err(Error::validation(
                    "duplicate_task_id",
                    format!(
                        "Task ID {id} appears at {} and {}",
                        first.display(),
                        relative.display()
                    ),
                ));
            }
        }

        let available_workers =
            std::thread::available_parallelism().map_or(1, |workers| workers.get());
        let worker_count = available_workers
            .min(MAX_WORKERS)
            .min(candidates.len().div_ceil(TASKS_PER_WORKER))
            .max(1);
        if worker_count == 1 {
            return self.load_selected_task_chunk(&candidates, &select);
        }

        let chunk_size = candidates.len().div_ceil(worker_count);
        std::thread::scope(|scope| {
            let mut workers = Vec::with_capacity(worker_count);
            for chunk in candidates.chunks(chunk_size) {
                let select = &select;
                workers.push(scope.spawn(move || self.load_selected_task_chunk(chunk, select)));
            }

            let mut tasks = Vec::new();
            for worker in workers {
                let mut selected = worker.join().map_err(|_| {
                    Error::new(
                        ErrorKind::Io,
                        "task_scan_worker_failed",
                        "A task scan worker stopped unexpectedly",
                    )
                })??;
                tasks.append(&mut selected);
            }
            Ok(tasks)
        })
    }

    fn load_selected_task_chunk<F>(
        &self,
        candidates: &[(String, PathBuf)],
        select: &F,
    ) -> Result<Vec<Task>>
    where
        F: Fn(&Task) -> Result<bool> + Sync,
    {
        let mut tasks = Vec::new();
        for (id, relative) in candidates {
            let source = self.read_record(relative, MAX_RECORD_BYTES as u64, "task")?;
            let task = parse_task(id, relative, &source, &self.config)
                .map_err(|error| error_with_path(error, relative))?;
            if select(&task)? {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }
    pub(crate) fn task_ids_unlocked(&self) -> Result<HashSet<String>> {
        let candidates = self.task_candidates()?;
        let mut ids = HashSet::with_capacity(candidates.len());
        for (id, relative) in candidates {
            if !ids.insert(id.clone()) {
                return Err(Error::validation(
                    "duplicate_task_id",
                    format!("Task ID {id} appears more than once"),
                )
                .with_path(relative));
            }
        }
        Ok(ids)
    }

    pub(crate) fn load_all_projects_unlocked(&self) -> Result<Vec<StoredProject>> {
        let candidates = self.project_candidates()?;
        let mut projects = Vec::with_capacity(candidates.len());
        for (slug, relative) in candidates {
            let mut snapshot = self.read_snapshot(&relative, MAX_RECORD_BYTES as u64, "project")?;
            let source = snapshot.take_bytes();
            let project = parse_project(&slug, &relative, &source)
                .map_err(|error| error_with_path(error, &relative))?;
            projects.push(StoredProject { project, snapshot });
        }
        Ok(projects)
    }

    pub(crate) fn resolve_task_unlocked(&self, id_or_prefix: &str) -> Result<StoredTask> {
        let prefix = validate_id_prefix(id_or_prefix)?;
        let candidates = self.task_candidates()?;
        let mut seen = HashMap::<&str, &Path>::with_capacity(candidates.len());
        for (id, relative) in &candidates {
            if let Some(first) = seen.insert(id, relative) {
                return Err(Error::validation(
                    "duplicate_task_id",
                    format!(
                        "Task ID {id} appears at {} and {}",
                        first.display(),
                        relative.display()
                    ),
                ));
            }
        }
        let matches = candidates
            .into_iter()
            .filter(|(id, _)| id.starts_with(&prefix))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => Err(Error::not_found(
                "task_not_found",
                format!("No task matches ID prefix {prefix}"),
            )),
            [(id, relative)] => {
                let mut snapshot = self.read_snapshot(relative, MAX_RECORD_BYTES as u64, "task")?;
                let source = snapshot.take_bytes();
                let task = parse_task(id, relative, &source, &self.config)
                    .map_err(|error| error_with_path(error, relative))?;
                Ok(StoredTask { task, snapshot })
            }
            _ => {
                let mut ids = matches.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
                ids.sort();
                Err(Error::new(
                    ErrorKind::Ambiguous,
                    "ambiguous_task_id",
                    format!("ID prefix {prefix} matches: {}", ids.join(", ")),
                ))
            }
        }
    }

    pub(crate) fn load_project_unlocked(&self, slug: &str) -> Result<StoredProject> {
        validate_project_slug(slug)?;
        let relative = self.config.projects_path()?.join(format!("{slug}.md"));
        let path = self.root.join(&relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::not_found(
                    "project_not_found",
                    format!("Project {slug:?} does not exist"),
                )
                .with_path(relative));
            }
            Err(source) => return Err(Error::io("inspect a project record", &path, &source)),
        };
        if metadata.file_type().is_symlink() {
            return Err(Error::validation(
                "record_symlink",
                "Project records must not be symlinks",
            )
            .with_path(&relative));
        }
        if !metadata.is_file() {
            return Err(Error::validation(
                "invalid_record_file",
                "Project record must be a regular file",
            )
            .with_path(&relative));
        }
        let mut snapshot = self.read_snapshot(&relative, MAX_RECORD_BYTES as u64, "project")?;
        let source = snapshot.take_bytes();
        let project = parse_project(slug, &relative, &source)
            .map_err(|error| error_with_path(error, &relative))?;
        Ok(StoredProject { project, snapshot })
    }

    pub(crate) fn create_task(&self, id: &str, bytes: &[u8]) -> Result<PathBuf> {
        validate_task_id(id)?;
        let relative = self.config.tasks_path()?.join(format!("{id}.md"));
        self.create_file(&relative, bytes)?;
        Ok(relative)
    }

    pub(crate) fn create_project(&self, slug: &str, bytes: &[u8]) -> Result<PathBuf> {
        validate_project_slug(slug)?;
        let relative = self.config.projects_path()?.join(format!("{slug}.md"));
        self.create_file(&relative, bytes)?;
        Ok(relative)
    }

    pub(crate) fn replace(&self, snapshot: &FileSnapshot, bytes: &[u8]) -> Result<()> {
        std::str::from_utf8(bytes).map_err(|_| {
            Error::validation("invalid_utf8", "Replacement record must be UTF-8")
                .with_path(&snapshot.relative_path)
        })?;
        let parent = snapshot.path.parent().ok_or_else(|| {
            Error::validation("invalid_path", "Record path has no parent directory")
        })?;
        ensure_real_directory(
            &self.root,
            snapshot.relative_path.parent().ok_or_else(|| {
                Error::validation("invalid_path", "Record path has no parent directory")
            })?,
        )?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(|source| {
            Error::io("create an atomic-write temporary file", parent, &source)
        })?;
        temporary.write_all(bytes).map_err(|source| {
            Error::io("write an atomic replacement", temporary.path(), &source)
        })?;
        temporary.as_file_mut().flush().map_err(|source| {
            Error::io("flush an atomic replacement", temporary.path(), &source)
        })?;
        temporary
            .as_file_mut()
            .set_permissions(snapshot.permissions.clone())
            .map_err(|source| {
                Error::io(
                    "preserve record permissions on a temporary file",
                    temporary.path(),
                    &source,
                )
            })?;
        temporary.as_file_mut().sync_all().map_err(|source| {
            Error::io(
                "synchronize an atomic replacement",
                temporary.path(),
                &source,
            )
        })?;

        self.ensure_unchanged(snapshot)?;
        temporary.persist(&snapshot.path).map_err(|error| {
            Error::io("atomically replace a record", &snapshot.path, &error.error)
        })?;
        sync_directory(parent)
    }

    pub(crate) fn delete(&self, snapshot: &FileSnapshot) -> Result<()> {
        self.ensure_unchanged(snapshot)?;
        ensure_real_directory(
            &self.root,
            snapshot.relative_path.parent().ok_or_else(|| {
                Error::validation("invalid_path", "Record path has no parent directory")
            })?,
        )?;
        let metadata = fs::symlink_metadata(&snapshot.path).map_err(|source| {
            Error::io("inspect a record before deletion", &snapshot.path, &source)
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::validation(
                "record_symlink",
                "Refusing to delete a symlink or non-file record",
            )
            .with_path(&snapshot.relative_path));
        }
        fs::remove_file(&snapshot.path)
            .map_err(|source| Error::io("delete a record", &snapshot.path, &source))?;
        if let Some(parent) = snapshot.path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    pub(crate) fn with_exclusive_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let file = self.open_lock_file()?;
        FileExt::lock_exclusive(&file).map_err(|source| {
            Error::io(
                "acquire the store's exclusive advisory lock",
                &self.root.join(CONFIG_PATH),
                &source,
            )
        })?;
        let result = operation();
        let unlock_result = FileExt::unlock(&file).map_err(|source| {
            Error::io(
                "release the store's advisory lock",
                &self.root.join(CONFIG_PATH),
                &source,
            )
        });
        match result {
            Err(error) => Err(error),
            Ok(value) => unlock_result.map(|()| value),
        }
    }

    pub(crate) fn with_shared_lock<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let file = self.open_lock_file()?;
        FileExt::lock_shared(&file).map_err(|source| {
            Error::io(
                "acquire the store's shared advisory lock",
                &self.root.join(CONFIG_PATH),
                &source,
            )
        })?;
        let result = operation();
        let unlock_result = FileExt::unlock(&file).map_err(|source| {
            Error::io(
                "release the store's advisory lock",
                &self.root.join(CONFIG_PATH),
                &source,
            )
        });
        match result {
            Err(error) => Err(error),
            Ok(value) => unlock_result.map(|()| value),
        }
    }

    fn task_candidates(&self) -> Result<Vec<(String, PathBuf)>> {
        let tasks_directory = self.config.tasks_path()?;
        ensure_real_directory(&self.root, &tasks_directory)?;
        let absolute = self.root.join(&tasks_directory);
        let mut candidates = Vec::new();
        for entry in WalkDir::new(&absolute)
            .sort_by_file_name()
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden_name(entry.file_name()))
        {
            let entry = entry.map_err(|source| {
                Error::new(
                    ErrorKind::Io,
                    "task_scan_failed",
                    format!("Could not scan the tasks directory: {source}"),
                )
                .with_path(&tasks_directory)
            })?;
            if entry.depth() == 0 {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| Error::validation("path_escape", "Task scanner left the store root"))?
                .to_path_buf();
            if entry.file_type().is_symlink() {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "md")
                {
                    return Err(Error::validation(
                        "record_symlink",
                        "Task records must not be symlinks",
                    )
                    .with_path(relative));
                }
                continue;
            }
            if !entry.file_type().is_file()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "md")
            {
                continue;
            }
            let id = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    Error::validation("invalid_task_path", "Task filename must be valid UTF-8")
                        .with_path(&relative)
                })?;
            validate_task_id(id).map_err(|error| error_with_path(error, &relative))?;
            candidates.push((id.to_ascii_uppercase(), relative));
        }
        candidates.sort_by(|left, right| left.1.cmp(&right.1));
        Ok(candidates)
    }

    fn project_candidates(&self) -> Result<Vec<(String, PathBuf)>> {
        let projects_directory = self.config.projects_path()?;
        ensure_real_directory(&self.root, &projects_directory)?;
        let absolute = self.root.join(&projects_directory);
        let mut candidates = Vec::new();
        for entry in WalkDir::new(&absolute)
            .sort_by_file_name()
            .min_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !is_hidden_name(entry.file_name()))
        {
            let entry = entry.map_err(|source| {
                Error::new(
                    ErrorKind::Io,
                    "project_scan_failed",
                    format!("Could not scan the projects directory: {source}"),
                )
                .with_path(&projects_directory)
            })?;
            let relative = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| {
                    Error::validation("path_escape", "Project scanner left the store root")
                })?
                .to_path_buf();
            if entry.file_type().is_symlink() {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "md")
                {
                    return Err(Error::validation(
                        "record_symlink",
                        "Project records must not be symlinks",
                    )
                    .with_path(relative));
                }
                continue;
            }
            if !entry.file_type().is_file()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "md")
            {
                continue;
            }
            if entry.depth() != 1 {
                return Err(Error::validation(
                    "nested_project",
                    "Project records must be directly inside the projects directory",
                )
                .with_path(relative));
            }
            let slug = entry
                .path()
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| {
                    Error::validation(
                        "invalid_project_path",
                        "Project filename must be valid UTF-8",
                    )
                    .with_path(&relative)
                })?
                .to_owned();
            validate_project_slug(&slug).map_err(|error| error_with_path(error, &relative))?;
            candidates.push((slug, relative));
        }
        candidates.sort_by(|left, right| left.1.cmp(&right.1));
        Ok(candidates)
    }

    fn create_file(&self, relative: &Path, bytes: &[u8]) -> Result<()> {
        validate_relative_record_path(relative)?;
        std::str::from_utf8(bytes).map_err(|_| {
            Error::validation("invalid_utf8", "New record must be UTF-8").with_path(relative)
        })?;
        let absolute = self.root.join(relative);
        let parent = absolute.parent().ok_or_else(|| {
            Error::validation("invalid_path", "Record path has no parent directory")
        })?;
        let parent_relative = relative.parent().ok_or_else(|| {
            Error::validation("invalid_path", "Record path has no parent directory")
        })?;
        ensure_real_directory(&self.root, parent_relative)?;
        let mut temporary = NamedTempFile::new_in(parent)
            .map_err(|source| Error::io("create a new-record temporary file", parent, &source))?;
        temporary
            .write_all(bytes)
            .map_err(|source| Error::io("write a new record", temporary.path(), &source))?;
        temporary
            .as_file_mut()
            .flush()
            .map_err(|source| Error::io("flush a new record", temporary.path(), &source))?;
        temporary
            .as_file_mut()
            .sync_all()
            .map_err(|source| Error::io("synchronize a new record", temporary.path(), &source))?;
        temporary.persist_noclobber(&absolute).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                Error::validation("record_already_exists", "Record already exists")
                    .with_path(relative)
            } else {
                Error::io("install a new record", &absolute, &error.error)
            }
        })?;
        sync_directory(parent)
    }

    fn read_snapshot(
        &self,
        relative: &Path,
        limit: u64,
        description: &str,
    ) -> Result<FileSnapshot> {
        let (path, bytes, metadata) = self.read_record_file(relative, limit, description)?;
        let hash = content_hash(&bytes);
        Ok(FileSnapshot {
            path,
            relative_path: relative.to_path_buf(),
            bytes,
            hash,
            length: metadata.len(),
            permissions: metadata.permissions(),
        })
    }

    fn read_record(&self, relative: &Path, limit: u64, description: &str) -> Result<Vec<u8>> {
        self.read_record_file(relative, limit, description)
            .map(|(_, bytes, _)| bytes)
    }

    fn read_record_file(
        &self,
        relative: &Path,
        limit: u64,
        description: &str,
    ) -> Result<(PathBuf, Vec<u8>, fs::Metadata)> {
        validate_relative_record_path(relative)?;
        let path = self.root.join(relative);
        let (mut file, metadata) = open_regular_nofollow(&path, relative)?;
        if metadata.len() > limit {
            return Err(Error::validation(
                "record_too_large",
                format!("The {description} exceeds the {limit}-byte limit"),
            )
            .with_path(relative));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
        Read::by_ref(&mut file)
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| Error::io(&format!("read the {description}"), &path, &source))?;
        if exceeds_limit(bytes.len(), limit) {
            return Err(Error::validation(
                "record_too_large",
                format!("The {description} exceeds the {limit}-byte limit"),
            )
            .with_path(relative));
        }
        Ok((path, bytes, metadata))
    }

    fn ensure_unchanged(&self, snapshot: &FileSnapshot) -> Result<()> {
        let (file, _) = open_regular_nofollow(&snapshot.path, &snapshot.relative_path)?;
        let mut current =
            Vec::with_capacity(usize::try_from(snapshot.length).unwrap_or(MAX_RECORD_BYTES));
        file.take((MAX_RECORD_BYTES + 1) as u64)
            .read_to_end(&mut current)
            .map_err(|source| {
                Error::io(
                    "rehash a record before replacement",
                    &snapshot.path,
                    &source,
                )
            })?;
        if current.len() > MAX_RECORD_BYTES || content_hash(&current) != snapshot.hash {
            return Err(Error::new(
                ErrorKind::Concurrent,
                "concurrent_modification",
                "Record changed after it was read; external content was left untouched",
            )
            .with_path(&snapshot.relative_path));
        }
        Ok(())
    }

    fn open_lock_file(&self) -> Result<File> {
        let path = self.root.join(CONFIG_PATH);
        let mut options = open_options_nofollow();
        options.read(true);
        let file = options
            .open(&path)
            .map_err(|source| Error::io("open the store configuration lock", &path, &source))?;
        let metadata = file
            .metadata()
            .map_err(|source| Error::io("inspect the store configuration lock", &path, &source))?;
        if !metadata.is_file() {
            return Err(Error::validation(
                "invalid_store_file",
                "Store configuration lock must remain a regular file",
            )
            .with_path(CONFIG_PATH));
        }
        Ok(file)
    }
}

fn open_regular_nofollow(path: &Path, relative: &Path) -> Result<(File, fs::Metadata)> {
    let file = open_options_nofollow()
        .read(true)
        .open(path)
        .map_err(|source| Error::io("open a record without following symlinks", path, &source))?;
    let metadata = file
        .metadata()
        .map_err(|source| Error::io("inspect an open record", path, &source))?;
    if !metadata.is_file() {
        return Err(
            Error::validation("invalid_record_file", "Record must be a regular file")
                .with_path(relative),
        );
    }
    Ok((file, metadata))
}

fn open_options_nofollow() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}

fn is_hidden_name(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

fn content_hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn validate_relative_record_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::validation(
            "path_escape",
            "Record path must be a normalized path relative to the store",
        )
        .with_path(path));
    }
    Ok(())
}

fn ensure_real_directory(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(Error::validation(
                "invalid_managed_path",
                "Managed directory path must be normalized",
            )
            .with_path(relative));
        };
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::validation(
                    "managed_directory_missing",
                    "Configured managed directory does not exist",
                )
                .with_path(relative)
            } else {
                Error::io("inspect a managed directory", &current, &source)
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(Error::validation(
                "managed_path_symlink",
                "Managed directory paths must not contain symlinks",
            )
            .with_path(relative));
        }
        if !metadata.is_dir() {
            return Err(Error::validation(
                "managed_path_not_directory",
                "Configured managed path is not a directory",
            )
            .with_path(relative));
        }
    }
    Ok(())
}

fn validate_managed_directory_on_disk(root: &Path, relative: &Path) -> Result<()> {
    ensure_real_directory(root, relative)
}

fn ensure_regular_file(path: &Path, display_path: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::not_found("store_not_found", "Todo store configuration is missing")
                .with_path(display_path)
        } else {
            Error::io("inspect a store file", path, &source)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::validation(
            "invalid_store_file",
            "Store metadata files must be regular files, not symlinks",
        )
        .with_path(display_path));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: u64, description: &str) -> Result<Vec<u8>> {
    let mut file = open_options_nofollow()
        .read(true)
        .open(path)
        .map_err(|source| Error::io(&format!("open the {description}"), path, &source))?;
    let metadata = file
        .metadata()
        .map_err(|source| Error::io(&format!("inspect the {description}"), path, &source))?;
    if !metadata.is_file() {
        return Err(Error::validation(
            "invalid_store_file",
            format!("The {description} must be a regular file"),
        )
        .with_path(path));
    }
    if metadata.len() > limit {
        return Err(Error::validation(
            "file_too_large",
            format!("The {description} exceeds the {limit}-byte limit"),
        )
        .with_path(path));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    Read::by_ref(&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| Error::io(&format!("read the {description}"), path, &source))?;
    if exceeds_limit(bytes.len(), limit) {
        return Err(Error::validation(
            "file_too_large",
            format!("The {description} exceeds the {limit}-byte limit"),
        )
        .with_path(path));
    }
    Ok(bytes)
}

fn exceeds_limit(length: usize, limit: u64) -> bool {
    match u64::try_from(length) {
        Ok(length) => length > limit,
        Err(_) => true,
    }
}

fn validate_schema(root: &Path, expected_version: u32) -> Result<()> {
    let path = root.join(SCHEMA_PATH);
    ensure_regular_file(&path, SCHEMA_PATH)?;
    let bytes = read_bounded(&path, MAX_CONFIG_BYTES, "schema")?;
    let schema: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        Error::validation(
            "invalid_schema_file",
            format!("schema.json is invalid JSON: {source}"),
        )
        .with_path(SCHEMA_PATH)
        .with_location(source.line(), source.column())
    })?;
    let embedded: serde_json::Value = serde_json::from_str(EMBEDDED_SCHEMA).map_err(|source| {
        Error::validation(
            "invalid_embedded_schema",
            format!("The schema embedded in this build is invalid JSON: {source}"),
        )
    })?;
    let version = schema
        .get("x-obsidian-todo-schema-version")
        .and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(expected_version)) || schema != embedded {
        return Err(Error::unsupported(
            "schema_version_mismatch",
            "schema.json does not match the configured schema version",
        )
        .with_path(SCHEMA_PATH));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let directory = File::open(path)
            .map_err(|source| Error::io("open a directory for synchronization", path, &source))?;
        directory
            .sync_all()
            .map_err(|source| Error::io("synchronize a directory", path, &source))?;
    }
    Ok(())
}

fn error_with_path(error: Error, path: &Path) -> Error {
    if error.path().is_some() {
        error
    } else {
        error.with_path(path)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use crate::commands::init::{initialize, InitOptions};

    use super::*;
    const FIRST_ID: &str = "01K4B0ZSBZZV25T1K0D3TA8JHR";
    const SECOND_ID: &str = "01K4B0ZSBZZV25T1K0D3TA8JHS";
    const DISTINCT_ID: &str = "01J3B0ZSBZZV25T1K0D3TA8JHR";

    fn task_record(name: &str) -> Vec<u8> {
        format!("---\nname: {name}\nstate: open\nprojects: []\ntags: []\n---\n").into_bytes()
    }

    fn initialized() -> TempDir {
        let temp = TempDir::new().expect("temp");
        initialize(&InitOptions {
            store_path: &temp.path().join("Todo"),
            vault_root: Some(temp.path()),
            current_directory: temp.path(),
            adopt_empty_layout: false,
            dry_run: false,
        })
        .expect("initialize");
        temp
    }

    #[test]
    fn opens_initialized_store() {
        let temp = initialized();
        let store = Store::open(temp.path().join("Todo")).expect("open");
        assert_eq!(store.config().schema_version, 1);
        assert!(store.paths().tasks.ends_with("Tasks"));
    }

    #[test]
    fn rejects_schema_that_only_claims_the_supported_version() {
        let temp = initialized();
        let root = temp.path().join("Todo");
        fs::write(
            root.join(SCHEMA_PATH),
            r#"{"x-obsidian-todo-schema-version":1}"#,
        )
        .expect("tamper with schema");
        let error = Store::open(root).expect_err("incomplete schema must fail");
        assert_eq!(error.code(), "schema_version_mismatch");
        assert_eq!(error.exit_code(), 7);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_managed_directory_symlink() {
        use std::os::unix::fs::symlink;

        let temp = initialized();
        let root = temp.path().join("Todo");
        fs::remove_dir(root.join("Tasks")).expect("remove tasks");
        fs::create_dir(temp.path().join("outside")).expect("outside");
        symlink(temp.path().join("outside"), root.join("Tasks")).expect("symlink");
        let error = Store::open(root).expect_err("symlink must fail");
        assert_eq!(error.code(), "managed_path_symlink");
    }

    #[test]
    fn detects_external_change_before_atomic_replacement() {
        let temp = initialized();
        let root = temp.path().join("Todo");
        let store = Store::open(&root).expect("open");
        let id = FIRST_ID;
        let original = b"---\nname: Original\nstate: open\nprojects: []\ntags: []\n---\nbody\n";
        let relative = store.create_task(id, original).expect("create");
        let snapshot = store
            .read_snapshot(&relative, MAX_RECORD_BYTES as u64, "task")
            .expect("snapshot");
        let external = b"---\nname: External\nstate: open\nprojects: []\ntags: []\n---\nbody\n";
        fs::write(root.join(&relative), external).expect("external edit");
        let error = store
            .replace(&snapshot, b"replacement")
            .expect_err("concurrent edit");
        assert_eq!(error.code(), "concurrent_modification");
        assert_eq!(fs::read(root.join(relative)).expect("read"), external);
        let task_entries = fs::read_dir(root.join("Tasks"))
            .expect("list task directory")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("task entries");
        assert_eq!(task_entries.len(), 1, "temporary file was not removed");
    }

    #[cfg(unix)]
    #[test]
    fn record_symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let temp = initialized();
        let root = temp.path().join("Todo");
        let outside = temp.path().join("outside-task.md");
        fs::write(&outside, task_record("Outside")).expect("outside task");
        symlink(&outside, root.join(format!("Tasks/{FIRST_ID}.md"))).expect("record symlink");
        let store = Store::open(root).expect("open");
        let error = store.get_task(FIRST_ID).expect_err("symlink must fail");
        assert_eq!(error.code(), "record_symlink");
        assert_eq!(
            fs::read(outside).expect("outside content"),
            task_record("Outside")
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_project_record_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = initialized();
        let root = temp.path().join("Todo");
        symlink(
            temp.path().join("missing-project.md"),
            root.join("Projects/work.md"),
        )
        .expect("project symlink");
        let store = Store::open(root).expect("open");
        let error = store.get_project("work").expect_err("symlink must fail");
        assert_eq!(error.code(), "record_symlink");
    }

    #[test]
    fn lowercase_task_basenames_resolve_case_insensitively() {
        let temp = initialized();
        let root = temp.path().join("Todo");
        let lowercase = FIRST_ID.to_ascii_lowercase();
        fs::write(
            root.join(format!("Tasks/{lowercase}.md")),
            task_record("Lowercase"),
        )
        .expect("lowercase task");
        let store = Store::open(root).expect("open");
        let task = store.get_task(&FIRST_ID[..6]).expect("resolve task");
        assert_eq!(task.id, FIRST_ID);
        assert!(task.path.ends_with(format!("{lowercase}.md")));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replacement_preserves_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = initialized();
        let root = temp.path().join("Todo");
        let store = Store::open(&root).expect("open");
        let relative = store
            .create_task(FIRST_ID, &task_record("Original"))
            .expect("create");
        let absolute = root.join(&relative);
        fs::set_permissions(&absolute, fs::Permissions::from_mode(0o640)).expect("set permissions");
        let snapshot = store
            .read_snapshot(&relative, MAX_RECORD_BYTES as u64, "task")
            .expect("snapshot");
        store
            .replace(&snapshot, &task_record("Replacement"))
            .expect("replace");
        assert_eq!(
            fs::metadata(absolute)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }

    #[test]
    fn resolves_unique_absent_and_ambiguous_task_prefixes() {
        let temp = initialized();
        let store = Store::open(temp.path().join("Todo")).expect("open");
        for (id, name) in [
            (FIRST_ID, "First"),
            (SECOND_ID, "Second"),
            (DISTINCT_ID, "Distinct"),
        ] {
            store
                .create_task(id, &task_record(name))
                .expect("create task");
        }

        let unique = store
            .resolve_task_unlocked("01j3b0")
            .expect("case-insensitive unique prefix");
        assert_eq!(unique.task.id, DISTINCT_ID);
        let absent = store
            .resolve_task_unlocked("01A000")
            .expect_err("absent prefix");
        assert_eq!(absent.code(), "task_not_found");
        let ambiguous = store
            .resolve_task_unlocked("01K4B0")
            .expect_err("ambiguous prefix");
        assert_eq!(ambiguous.code(), "ambiguous_task_id");
        assert_eq!(ambiguous.exit_code(), 4);
        fs::create_dir(store.root().join("Tasks/nested")).expect("nested tasks");
        fs::write(
            store
                .root()
                .join(format!("Tasks/nested/{}.md", FIRST_ID.to_ascii_lowercase())),
            task_record("Duplicate"),
        )
        .expect("duplicate task");
        let duplicate = store
            .resolve_task_unlocked(&DISTINCT_ID[..6])
            .expect_err("duplicate identity invalidates resolution");
        assert_eq!(duplicate.code(), "duplicate_task_id");
    }

    #[cfg(unix)]
    #[test]
    fn shared_and_exclusive_operations_work_with_read_only_configuration() {
        use std::os::unix::fs::PermissionsExt;

        let temp = initialized();
        let root = temp.path().join("Todo");
        let config_path = root.join(CONFIG_PATH);
        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o444))
            .expect("make config read-only");
        let store = Store::open(root).expect("open");
        assert!(store
            .list_tasks()
            .expect("read with shared lock")
            .is_empty());
        store
            .with_exclusive_lock(|| store.create_task(FIRST_ID, &task_record("Writable")))
            .expect("mutate with exclusive lock");
        assert_eq!(store.list_tasks().expect("read created task").len(), 1);
    }
}
