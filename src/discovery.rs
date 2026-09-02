use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::CONFIG_PATH;
use crate::error::{Error, ErrorKind, Result};

#[derive(Debug, Clone)]
pub struct DiscoveryOptions<'a> {
    pub explicit_root: Option<&'a Path>,
    pub environment_root: Option<&'a OsStr>,
    pub current_directory: &'a Path,
}

pub fn discover(options: &DiscoveryOptions<'_>) -> Result<PathBuf> {
    if let Some(root) = options.explicit_root {
        return require_store(root, options.current_directory, "--root");
    }
    if let Some(root) = options.environment_root {
        if root.is_empty() {
            return Err(Error::usage(
                "invalid_root",
                "OBSIDIAN_TODO_ROOT cannot be empty",
            ));
        }
        return require_store(
            Path::new(root),
            options.current_directory,
            "OBSIDIAN_TODO_ROOT",
        );
    }

    let current = fs::canonicalize(options.current_directory).map_err(|source| {
        Error::io(
            "resolve the current directory",
            options.current_directory,
            &source,
        )
    })?;
    for ancestor in current.ancestors() {
        if is_store(ancestor) {
            return Ok(ancestor.to_path_buf());
        }
    }

    let mut children = direct_child_stores(&current)?;
    match children.len() {
        0 => Err(Error::not_found(
            "store_not_found",
            "No todo store found; pass --root or set OBSIDIAN_TODO_ROOT",
        )),
        1 => Ok(children.swap_remove(0)),
        _ => {
            children.sort();
            let paths = children
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::new(
                ErrorKind::Ambiguous,
                "ambiguous_store",
                format!("Multiple todo stores found: {paths}; pass --root"),
            ))
        }
    }
}

#[must_use]
pub fn is_store(path: &Path) -> bool {
    let metadata_directory = fs::symlink_metadata(path.join(".todo"));
    if !metadata_directory
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    {
        return false;
    }
    fs::symlink_metadata(path.join(CONFIG_PATH))
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

fn require_store(root: &Path, current: &Path, source_name: &str) -> Result<PathBuf> {
    let candidate = if root.is_absolute() {
        root.to_path_buf()
    } else {
        current.join(root)
    };
    let metadata = fs::symlink_metadata(&candidate).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::not_found(
                "store_not_found",
                format!("{source_name} does not identify an existing todo store"),
            )
            .with_path(&candidate)
        } else {
            Error::io("inspect the todo store", &candidate, &source)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::validation(
            "invalid_store_root",
            format!("{source_name} must identify a real directory, not a symlink"),
        )
        .with_path(candidate));
    }
    let canonical = fs::canonicalize(&candidate)
        .map_err(|source| Error::io("resolve the todo store", &candidate, &source))?;
    if !is_store(&canonical) {
        return Err(Error::not_found(
            "store_not_found",
            format!("{source_name} does not identify a todo store"),
        )
        .with_path(canonical));
    }
    Ok(canonical)
}

fn direct_child_stores(current: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(current)
        .map_err(|source| Error::io("inspect direct child directories", current, &source))?;
    let mut stores = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| Error::io("inspect a directory entry", current, &source))?;
        let file_type = entry
            .file_type()
            .map_err(|source| Error::io("inspect a directory entry", &entry.path(), &source))?;
        if file_type.is_dir() && is_store(&entry.path()) {
            let path = fs::canonicalize(entry.path()).map_err(|source| {
                Error::io("resolve a direct child todo store", &entry.path(), &source)
            })?;
            stores.push(path);
        }
    }
    Ok(stores)
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use tempfile::TempDir;

    use super::*;

    fn marker(root: &Path) {
        fs::create_dir_all(root.join(".todo")).expect("metadata directory");
        fs::write(root.join(CONFIG_PATH), "marker").expect("config marker");
    }

    #[test]
    fn explicit_root_has_highest_precedence() {
        let temp = TempDir::new().expect("temp");
        let explicit = temp.path().join("explicit");
        let environment = temp.path().join("environment");
        marker(&explicit);
        marker(&environment);
        let result = discover(&DiscoveryOptions {
            explicit_root: Some(&explicit),
            environment_root: Some(environment.as_os_str()),
            current_directory: temp.path(),
        })
        .expect("discover");
        assert_eq!(result, explicit.canonicalize().expect("canonical"));
    }

    #[test]
    fn nearest_ancestor_wins() {
        let temp = TempDir::new().expect("temp");
        marker(temp.path());
        let nested_store = temp.path().join("nested");
        marker(&nested_store);
        let work = nested_store.join("deep/work");
        fs::create_dir_all(&work).expect("work directory");
        let result = discover(&DiscoveryOptions {
            explicit_root: None,
            environment_root: None,
            current_directory: &work,
        })
        .expect("discover");
        assert_eq!(result, nested_store.canonicalize().expect("canonical"));
    }

    #[cfg(unix)]
    #[test]
    fn explicit_root_symlink_is_rejected() {
        let temp = TempDir::new().expect("temp");
        let real = temp.path().join("real");
        marker(&real);
        let linked = temp.path().join("linked");
        symlink(&real, &linked).expect("store symlink");
        let error = discover(&DiscoveryOptions {
            explicit_root: Some(&linked),
            environment_root: None,
            current_directory: temp.path(),
        })
        .expect_err("root symlink");
        assert_eq!(error.code(), "invalid_store_root");
    }

    #[test]
    fn ambiguous_direct_children_are_reported() {
        let temp = TempDir::new().expect("temp");
        marker(&temp.path().join("one"));
        marker(&temp.path().join("two"));
        let error = discover(&DiscoveryOptions {
            explicit_root: None,
            environment_root: None,
            current_directory: temp.path(),
        })
        .expect_err("ambiguous");
        assert_eq!(error.code(), "ambiguous_store");
        assert_eq!(error.exit_code(), 4);
        assert!(error.message().contains("one"));
        assert!(error.message().contains("two"));
    }
}
