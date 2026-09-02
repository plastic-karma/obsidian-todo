use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use tempfile::NamedTempFile;

use crate::config::{Config, CONFIG_PATH, EMBEDDED_SCHEMA, SCHEMA_PATH};
use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct InitOptions<'a> {
    pub store_path: &'a Path,
    pub vault_root: Option<&'a Path>,
    pub current_directory: &'a Path,
    pub adopt_empty_layout: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitPlan {
    pub vault_root: PathBuf,
    pub store_root: PathBuf,
    pub directories: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub obsidian_link_prefix: String,
    pub dry_run: bool,
}

pub fn initialize(options: &InitOptions<'_>) -> Result<InitPlan> {
    let current = fs::canonicalize(options.current_directory).map_err(|source| {
        Error::io(
            "resolve the current directory",
            options.current_directory,
            &source,
        )
    })?;
    let vault = resolve_vault_root(options.vault_root, &current)?;
    let store = resolve_store_path(options.store_path, &current, &vault)?;
    inspect_existing_layout(&store, options.adopt_empty_layout)?;

    let prefix = relative_link_prefix(&store, &vault)?;
    let config = Config::defaults(prefix.clone());
    let config_source = config.to_toml()?;
    let metadata_directory = store.join(".todo");
    let directories = vec![
        metadata_directory.clone(),
        store.join(&config.tasks_directory),
        store.join(&config.projects_directory),
    ];
    let files = vec![store.join(CONFIG_PATH), store.join(SCHEMA_PATH)];
    let plan = InitPlan {
        vault_root: vault,
        store_root: store.clone(),
        directories: directories.clone(),
        files: files.clone(),
        obsidian_link_prefix: prefix,
        dry_run: options.dry_run,
    };

    if options.dry_run {
        return Ok(plan);
    }

    for directory in &directories {
        fs::create_dir_all(directory).map_err(|source| {
            Error::io("create an initialization directory", directory, &source)
        })?;
    }
    create_new_atomically(&files[0], config_source.as_bytes())?;
    if let Err(error) = create_new_atomically(&files[1], EMBEDDED_SCHEMA.as_bytes()) {
        let _ = fs::remove_file(&files[0]);
        return Err(error);
    }

    sync_directory(&metadata_directory)?;
    sync_directory(&store)?;
    validate_initialized_layout(&plan, &config)?;
    Ok(plan)
}

fn resolve_vault_root(explicit: Option<&Path>, current: &Path) -> Result<PathBuf> {
    if let Some(explicit) = explicit {
        let candidate = if explicit.is_absolute() {
            explicit.to_path_buf()
        } else {
            current.join(explicit)
        };
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|source| Error::io("inspect the vault root", &candidate, &source))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Error::validation(
                "invalid_vault_root",
                "Vault root must be a real directory, not a symlink",
            )
            .with_path(candidate));
        }
        return fs::canonicalize(&candidate)
            .map_err(|source| Error::io("resolve the vault root", &candidate, &source));
    }

    for ancestor in current.ancestors() {
        let marker = ancestor.join(".obsidian");
        if fs::symlink_metadata(&marker).is_ok_and(|metadata| metadata.is_dir()) {
            return Ok(ancestor.to_path_buf());
        }
    }
    Err(Error::not_found(
        "vault_not_found",
        "Could not find an ancestor containing .obsidian; pass --vault-root",
    ))
}

fn resolve_store_path(store: &Path, current: &Path, vault: &Path) -> Result<PathBuf> {
    let candidate = if store.is_absolute() {
        normalize_absolute(store)?
    } else {
        normalize_absolute(&current.join(store))?
    };

    let resolved = if candidate.exists() {
        let metadata = fs::symlink_metadata(&candidate)
            .map_err(|source| Error::io("inspect the store path", &candidate, &source))?;
        if metadata.file_type().is_symlink() {
            return Err(Error::validation(
                "store_path_symlink",
                "Store path must not be a symlink",
            )
            .with_path(candidate));
        }
        fs::canonicalize(&candidate)
            .map_err(|source| Error::io("resolve the store path", &candidate, &source))?
    } else {
        resolve_through_existing_ancestor(&candidate)?
    };

    if !resolved.starts_with(vault) {
        return Err(Error::validation(
            "store_outside_vault",
            "Store path must resolve inside the vault root",
        )
        .with_path(resolved));
    }
    Ok(resolved)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Error::validation(
            "invalid_path",
            "Internal path resolution expected an absolute path",
        ));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Error::validation(
                        "invalid_path",
                        "Path traversal escapes the filesystem root",
                    ));
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn resolve_through_existing_ancestor(candidate: &Path) -> Result<PathBuf> {
    let mut ancestor = candidate.to_path_buf();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return Err(Error::validation(
                "invalid_store_path",
                "Store path has no existing ancestor",
            )
            .with_path(candidate));
        };
        suffix.push(name.to_os_string());
        if !ancestor.pop() {
            return Err(Error::validation(
                "invalid_store_path",
                "Store path has no existing ancestor",
            )
            .with_path(candidate));
        }
    }
    let metadata = fs::symlink_metadata(&ancestor)
        .map_err(|source| Error::io("inspect a store path ancestor", &ancestor, &source))?;
    if !metadata.is_dir() {
        return Err(Error::validation(
            "invalid_store_path",
            "Store path traverses a non-directory",
        )
        .with_path(ancestor));
    }
    let mut resolved = fs::canonicalize(&ancestor)
        .map_err(|source| Error::io("resolve a store path ancestor", &ancestor, &source))?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn inspect_existing_layout(store: &Path, adopt: bool) -> Result<()> {
    if store.join(CONFIG_PATH).exists() {
        return Err(Error::validation(
            "store_already_initialized",
            "A todo store is already initialized at this path",
        )
        .with_path(store.join(CONFIG_PATH)));
    }
    if !store.exists() {
        return Ok(());
    }
    let mut entries = fs::read_dir(store)
        .map_err(|source| Error::io("inspect the target store directory", store, &source))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| Error::io("inspect the target store directory", store, &source))?;
    if entries.is_empty() {
        return Ok(());
    }
    if !adopt {
        return Err(Error::validation(
            "store_not_empty",
            "Target store directory is not empty; use --adopt-empty-layout only for an empty managed layout",
        )
        .with_path(store));
    }
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if !matches!(name.to_str(), Some(".todo" | "Tasks" | "Projects")) {
            return Err(Error::validation(
                "store_contains_unmanaged_content",
                "Only empty .todo, Tasks, and Projects directories can be adopted",
            )
            .with_path(entry.path()));
        }
        let metadata = entry.metadata().map_err(|source| {
            Error::io("inspect an existing layout entry", &entry.path(), &source)
        })?;
        if !metadata.is_dir() || entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            return Err(Error::validation(
                "store_contains_unmanaged_content",
                "The adopted layout may contain only real directories",
            )
            .with_path(entry.path()));
        }
        if fs::read_dir(entry.path())
            .map_err(|source| {
                Error::io(
                    "inspect an existing managed directory",
                    &entry.path(),
                    &source,
                )
            })?
            .next()
            .is_some()
        {
            return Err(Error::validation(
                "managed_directory_not_empty",
                "Existing managed directories must be empty when adopted",
            )
            .with_path(entry.path()));
        }
    }
    Ok(())
}

fn relative_link_prefix(store: &Path, vault: &Path) -> Result<String> {
    let relative = store.strip_prefix(vault).map_err(|_| {
        Error::validation(
            "store_outside_vault",
            "Store path must resolve inside the vault root",
        )
    })?;
    relative
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(ToOwned::to_owned).ok_or_else(|| {
                Error::validation(
                    "non_utf8_store_path",
                    "Store path must be UTF-8 for Obsidian links",
                )
            }),
            _ => Err(Error::validation(
                "invalid_store_path",
                "Store path must be normalized",
            )),
        })
        .collect::<Result<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

fn create_new_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| {
        Error::validation("invalid_path", "Output file must have a parent directory")
    })?;
    let mut temporary = NamedTempFile::new_in(parent)
        .map_err(|source| Error::io("create a temporary output file", parent, &source))?;
    temporary
        .write_all(bytes)
        .map_err(|source| Error::io("write a temporary output file", temporary.path(), &source))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|source| Error::io("sync a temporary output file", temporary.path(), &source))?;
    temporary.persist_noclobber(path).map_err(|error| {
        if error.error.kind() == std::io::ErrorKind::AlreadyExists {
            Error::validation("file_already_exists", "Output file already exists").with_path(path)
        } else {
            Error::io("install a new output file", path, &error.error)
        }
    })?;
    sync_directory(parent)
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

fn validate_initialized_layout(plan: &InitPlan, config: &Config) -> Result<()> {
    for directory in &plan.directories {
        let metadata = fs::symlink_metadata(directory)
            .map_err(|source| Error::io("validate an initialized directory", directory, &source))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::validation(
                "invalid_initialized_layout",
                "Initialized managed paths must be real directories",
            )
            .with_path(directory));
        }
    }
    let written = fs::read_to_string(plan.store_root.join(CONFIG_PATH)).map_err(|source| {
        Error::io(
            "read the initialized configuration",
            &plan.store_root.join(CONFIG_PATH),
            &source,
        )
    })?;
    let parsed = Config::parse(&written)?;
    if &parsed != config {
        return Err(Error::validation(
            "initialized_config_mismatch",
            "Initialized configuration did not round-trip",
        ));
    }
    let schema: serde_json::Value = serde_json::from_slice(
        &fs::read(plan.store_root.join(SCHEMA_PATH)).map_err(|source| {
            Error::io(
                "read the initialized schema",
                &plan.store_root.join(SCHEMA_PATH),
                &source,
            )
        })?,
    )
    .map_err(|source| {
        Error::validation(
            "invalid_schema_file",
            format!("Initialized schema is invalid JSON: {source}"),
        )
    })?;
    if schema
        .get("x-obsidian-todo-schema-version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(config.schema_version))
    {
        return Err(Error::validation(
            "schema_version_mismatch",
            "Initialized schema version does not match configuration",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn dry_run_makes_no_changes() {
        let temp = TempDir::new().expect("temp");
        fs::create_dir(temp.path().join(".obsidian")).expect("obsidian marker");
        let store = temp.path().join("Todo");
        let plan = initialize(&InitOptions {
            store_path: &store,
            vault_root: None,
            current_directory: temp.path(),
            adopt_empty_layout: false,
            dry_run: true,
        })
        .expect("dry run");
        assert!(plan.dry_run);
        assert!(!store.exists());
        assert_eq!(plan.obsidian_link_prefix, "Todo");
    }

    #[test]
    fn initializes_only_inside_vault() {
        let temp = TempDir::new().expect("temp");
        fs::write(temp.path().join("unrelated.md"), b"unchanged").expect("unrelated note");
        let store = temp.path().join("Area/Todo");
        let plan = initialize(&InitOptions {
            store_path: &store,
            vault_root: Some(temp.path()),
            current_directory: temp.path(),
            adopt_empty_layout: false,
            dry_run: false,
        })
        .expect("initialize");
        assert_eq!(plan.obsidian_link_prefix, "Area/Todo");
        assert_eq!(
            fs::read(temp.path().join("unrelated.md")).expect("unrelated note"),
            b"unchanged"
        );
        let config = Config::parse(
            &fs::read_to_string(store.join(CONFIG_PATH)).expect("written configuration"),
        )
        .expect("valid config");
        assert_eq!(config.obsidian_link_prefix, "Area/Todo");
        assert!(!temp.path().join(".git").exists());
    }

    #[test]
    fn rejects_store_outside_vault() {
        let temp = TempDir::new().expect("temp");
        let vault = temp.path().join("vault");
        fs::create_dir(&vault).expect("vault");
        let outside = temp.path().join("outside");
        let error = initialize(&InitOptions {
            store_path: &outside,
            vault_root: Some(&vault),
            current_directory: temp.path(),
            adopt_empty_layout: false,
            dry_run: false,
        })
        .expect_err("outside vault");
        assert_eq!(error.code(), "store_outside_vault");
        assert!(!outside.exists());
    }

    #[test]
    fn adopts_only_empty_default_layout() {
        let temp = TempDir::new().expect("temp");
        let store = temp.path().join("Todo");
        for directory in [".todo", "Tasks", "Projects"] {
            fs::create_dir_all(store.join(directory)).expect("layout directory");
        }
        initialize(&InitOptions {
            store_path: &store,
            vault_root: Some(temp.path()),
            current_directory: temp.path(),
            adopt_empty_layout: true,
            dry_run: false,
        })
        .expect("adopt empty layout");
        assert!(store.join(CONFIG_PATH).is_file());
    }
}
