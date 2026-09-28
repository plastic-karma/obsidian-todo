use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::Serialize;

use crate::error::{Error, ErrorKind, Result};
use crate::validate::validate_store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Ours,
    Theirs,
}

pub struct SyncConflict {
    pub path: PathBuf,
    pub description: String,
    pub ours: Option<Vec<u8>>,
    pub theirs: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct SyncResult {
    pub branch: String,
    pub upstream: String,
    pub committed: bool,
    pub conflicts_resolved: usize,
}

/// Synchronize the containing branch, committing only changes under `root`.
/// Callers must quiesce other writers. No store lock spans Git or the resolver.
/// Failed pushes retain commits; resolver failures abort only this invocation's merge.
pub fn synchronize(
    root: &Path,
    choice: Option<ConflictChoice>,
    mut resolve: impl FnMut(&SyncConflict) -> Result<ConflictChoice>,
) -> Result<SyncResult> {
    let root = fs::canonicalize(root)
        .map_err(|error| precondition(format!("Cannot resolve store root: {error}")))?;
    let discovery = Git {
        worktree: root.clone(),
    };
    let discovered = discovery.output(&["rev-parse", "--show-toplevel"])?;
    if !discovered.status.success() {
        return Err(precondition(
            "The store must be inside an existing Git worktree",
        ));
    }
    let git = Git {
        worktree: PathBuf::from(os_bytes(trim_lf(&discovered.stdout))?),
    };
    let relative = root
        .strip_prefix(&git.worktree)
        .map_err(|_| precondition("The store is not beneath the Git worktree"))?;
    let store_path = if relative.as_os_str().is_empty() {
        Path::new(".")
    } else {
        relative
    };
    git.ensure_idle()?;
    let branch_output = git.output(&["symbolic-ref", "--quiet", "HEAD"])?;
    if !branch_output.status.success() {
        return Err(precondition(
            "Sync requires a branch; detached HEAD is not supported",
        ));
    }
    let branch_ref = text_line(&branch_output.stdout, "branch name")?;
    if !git
        .output(&["rev-parse", "--verify", "HEAD^{commit}"])?
        .status
        .success()
    {
        return Err(precondition(
            "Sync requires an existing commit; the branch is unborn",
        ));
    }
    let upstream_output = git.checked(&[
        "for-each-ref",
        "--format=%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream:short)",
        &branch_ref,
    ])?;
    let upstream_fields: Vec<_> = trim_lf(&upstream_output.stdout)
        .split(|byte| *byte == 0)
        .collect();
    if upstream_fields.len() != 3 || upstream_fields.iter().any(|field| field.is_empty()) {
        return Err(precondition(
            "Configure a branch upstream before using /sync",
        ));
    }
    let remote = text(upstream_fields[0], "upstream remote")?;
    let remote_ref = text(upstream_fields[1], "upstream branch")?;
    let upstream = text(upstream_fields[2], "upstream name")?;
    if !remote_ref.starts_with("refs/heads/") || remote.starts_with('-') {
        return Err(precondition(
            "Sync requires a named upstream branch and a safe remote name",
        ));
    }
    git.ensure_store_changes(relative)?;
    git.checked(&[
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--",
        &remote,
        &remote_ref,
    ])?;
    let fetched = git.checked(&["rev-parse", "--verify", "FETCH_HEAD^{commit}"])?;
    let fetched = text_line(&fetched.stdout, "fetched commit")?;
    // Recheck after network access, before the first index/worktree mutation.
    git.ensure_idle()?;
    git.ensure_store_changes(relative)?;
    git.checked_os(&[
        OsStr::new("add"),
        OsStr::new("--all"),
        OsStr::new("--"),
        store_path.as_os_str(),
    ])?;
    let staged = git.output(&["diff", "--cached", "--quiet", "--exit-code"])?;
    let committed = match staged.status.code() {
        Some(0) => false,
        Some(1) => {
            git.ensure_store_changes(relative)?;
            git.checked(&[
                "commit",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                "Sync todo store",
            ])?;
            true
        }
        _ => return Err(git_error("inspect staged changes", &staged)),
    };
    git.ensure_worktree_clean(false)?;
    let merged = git.output(&[
        "merge",
        "--no-edit",
        "--no-commit",
        "--no-stat",
        "--no-autostash",
        "--no-overwrite-ignore",
        "--ff",
        "--no-verify-signatures",
        "--strategy=ort",
        "-Xno-renames",
        &fetched,
    ])?;
    let owns_merge = git.operation_exists("MERGE_HEAD")?;
    let finish = (|| {
        let conflicts = git.conflicts()?;
        if !merged.status.success() && (!owns_merge || conflicts.is_empty()) {
            return Err(git_error("merge upstream", &merged));
        }
        let mut conflicts_resolved = 0;
        for (path, stages) in conflicts {
            conflicts_resolved += git.resolve_file(&path, &stages, choice, &mut resolve)?;
        }
        if !git.conflicts()?.is_empty() {
            return Err(failed("Git still reports unresolved index entries"));
        }
        // Validation takes and releases its own lock; no Git runs while it is held.
        validate_store(&root).into_result()?;
        git.ensure_worktree_clean(true)?;
        if owns_merge {
            git.checked(&[
                "commit",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                "Merge upstream todo changes",
            ])?;
        }
        Ok(conflicts_resolved)
    })();
    let conflicts_resolved = match finish {
        Ok(count) => count,
        Err(error) => {
            if owns_merge {
                if let Err(abort) = git.checked(&["merge", "--abort"]) {
                    return Err(failed(format!(
                        "{error}; also could not abort this sync's merge: {abort}. Resolve the merge manually; local commits remain intact"
                    )));
                }
            }
            return Err(error);
        }
    };
    git.ensure_worktree_clean(false)?;
    let destination = format!("HEAD:{remote_ref}");
    git.checked(&[
        "push",
        "--no-verify",
        "--signed=false",
        "--no-follow-tags",
        "--recurse-submodules=no",
        "--",
        &remote,
        &destination,
    ])?;
    Ok(SyncResult {
        branch: branch_ref
            .strip_prefix("refs/heads/")
            .unwrap_or(&branch_ref)
            .to_owned(),
        upstream,
        committed,
        conflicts_resolved,
    })
}

struct Git {
    worktree: PathBuf,
}

impl Git {
    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command.current_dir(&self.worktree);
        // The explicit store chooses the repository, never the invoking shell's Git context.
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_NAMESPACE",
            "GIT_CEILING_DIRECTORIES",
            "GIT_DISCOVERY_ACROSS_FILESYSTEM",
            "GIT_PREFIX",
            "GIT_SHALLOW_FILE",
            "GIT_REPLACE_REF_BASE",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
        ] {
            command.env_remove(name);
        }
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "false")
            .env("SSH_ASKPASS", "false")
            .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes")
            .env("GIT_LITERAL_PATHSPECS", "1")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.editor=false",
                "-c",
                "sequence.editor=false",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "merge.autoStash=false",
                "-c",
                "merge.directoryRenames=false",
                "-c",
                "credential.interactive=false",
                "-c",
                "core.fsmonitor=false",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn output(&self, args: &[&str]) -> Result<Output> {
        self.command().args(args).output().map_err(spawn_error)
    }

    fn checked(&self, args: &[&str]) -> Result<Output> {
        let output = self.output(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(git_error(args[0], &output))
        }
    }

    fn checked_os(&self, args: &[&OsStr]) -> Result<Output> {
        let output = self.command().args(args).output().map_err(spawn_error)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(git_error(&args[0].to_string_lossy(), &output))
        }
    }

    fn input(&self, args: &[&str], bytes: &[u8]) -> Result<Output> {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(spawn_error)?;
        let written = child
            .stdin
            .take()
            .ok_or_else(|| failed("Git stdin is unavailable"))?
            .write_all(bytes);
        let output = child.wait_with_output().map_err(spawn_error)?;
        if !output.status.success() {
            return Err(git_error(args[0], &output));
        }
        written.map_err(|error| failed(format!("Cannot write Git input: {error}")))?;
        Ok(output)
    }

    fn operation_exists(&self, name: &str) -> Result<bool> {
        let output = self.checked(&["rev-parse", "--path-format=absolute", "--git-path", name])?;
        let path = PathBuf::from(os_bytes(trim_lf(&output.stdout))?);
        match fs::symlink_metadata(&path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(failed(format!(
                "Cannot inspect Git operation state: {error}"
            ))),
        }
    }

    fn ensure_idle(&self) -> Result<()> {
        for name in [
            "MERGE_HEAD",
            "rebase-merge",
            "rebase-apply",
            "REBASE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "sequencer",
            "index.lock",
            "MERGE_AUTOSTASH",
        ] {
            if self.operation_exists(name)? {
                return Err(precondition(format!(
                    "Finish or abort the existing Git operation ({name}) before syncing"
                )));
            }
        }
        if !self.conflicts()?.is_empty() {
            return Err(precondition(
                "Resolve existing Git conflicts before syncing",
            ));
        }
        Ok(())
    }

    fn ensure_store_changes(&self, store: &Path) -> Result<()> {
        let output = self.checked(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])?;
        for path in status_paths(&output.stdout)? {
            if !store.as_os_str().is_empty() && !path.starts_with(store) {
                return Err(precondition(
                    "Changes outside the selected store must be committed or moved before syncing",
                )
                .with_path(path));
            }
        }
        Ok(())
    }

    fn ensure_worktree_clean(&self, allow_staged: bool) -> Result<()> {
        let output = self.checked(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])?;
        let mut records = nul_records(&output.stdout)?.into_iter();
        while let Some(record) = records.next() {
            if record.len() < 4 || record[2] != b' ' {
                return Err(failed("Malformed Git status output"));
            }
            if !allow_staged || record[1] != b' ' || record[0] == b'?' {
                return Err(failed("The worktree changed during sync or could not be fully staged; local work is retained. Quiesce other writers before retrying")
                    .with_path(PathBuf::from(os_bytes(&record[3..])?)));
            }
            if matches!(record[0], b'R' | b'C') && records.next().is_none() {
                return Err(failed("Missing Git rename source"));
            }
        }
        Ok(())
    }

    fn conflicts(&self) -> Result<BTreeMap<PathBuf, Stages>> {
        let output = self.checked(&["ls-files", "--unmerged", "-z"])?;
        index_conflicts(&output.stdout)
    }

    fn blob(&self, entry: &Option<IndexEntry>) -> Result<Option<Vec<u8>>> {
        entry
            .as_ref()
            .map(|entry| {
                self.checked(&["cat-file", "blob", &entry.object])
                    .map(|output| output.stdout)
            })
            .transpose()
    }

    fn resolve_file(
        &self,
        path: &Path,
        stages: &Stages,
        policy: Option<ConflictChoice>,
        resolve: &mut impl FnMut(&SyncConflict) -> Result<ConflictChoice>,
    ) -> Result<usize> {
        // Submodules contain commits, not blobs. Never recurse into or replace their worktrees.
        if stages.iter().flatten().any(|entry| entry.mode == "160000") {
            return Err(
                precondition("Resolve submodule conflicts manually before syncing").with_path(path),
            );
        }
        // Ort may relocate a file to a synthetic "path~HEAD" during a file/directory
        // collision. That temporary name is not a resolution of the original path.
        // Refuse rather than committing it or recursively deleting the other side's tree.
        let mut original_path = false;
        for tree in ["HEAD", "MERGE_HEAD"] {
            let output = self.checked_os(&[
                OsStr::new("ls-tree"),
                OsStr::new("--full-tree"),
                OsStr::new("-z"),
                OsStr::new(tree),
                OsStr::new("--"),
                path.as_os_str(),
            ])?;
            if !output.stdout.is_empty() {
                original_path = true;
                break;
            }
        }
        if !original_path {
            return Err(precondition("Git relocated a file/directory conflict; resolve that structural conflict manually before syncing")
                .with_path(path));
        }
        let base = self.blob(&stages[0])?;
        let ours = self.blob(&stages[1])?;
        let theirs = self.blob(&stages[2])?;
        let regular = stages
            .iter()
            .flatten()
            .all(|entry| matches!(entry.mode.as_str(), "100644" | "100755"));
        let textual = [&base, &ours, &theirs]
            .into_iter()
            .flatten()
            .all(|bytes| !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok());
        if regular && textual && ours.is_some() && theirs.is_some() {
            let ours = ours.as_deref().unwrap_or_default();
            let theirs = theirs.as_deref().unwrap_or_default();
            let (content, mut count) = self.merge_text(
                path,
                [base.as_deref().unwrap_or_default(), ours, theirs],
                policy,
                resolve,
            )?;
            let our_mode = stages[1].as_ref().map(|entry| entry.mode.as_str());
            let their_mode = stages[2].as_ref().map(|entry| entry.mode.as_str());
            let base_mode = stages[0].as_ref().map(|entry| entry.mode.as_str());
            let mode = if our_mode == their_mode || their_mode == base_mode {
                our_mode
            } else if our_mode == base_mode {
                their_mode
            } else {
                let selection = choose(
                    policy,
                    resolve,
                    &SyncConflict {
                        path: path.to_owned(),
                        description: format!(
                            "File mode conflict: ours {}, theirs {}",
                            our_mode.unwrap_or("absent"),
                            their_mode.unwrap_or("absent")
                        ),
                        ours: Some(ours.to_vec()),
                        theirs: Some(theirs.to_vec()),
                    },
                )?;
                count += 1;
                match selection {
                    ConflictChoice::Ours => our_mode,
                    ConflictChoice::Theirs => their_mode,
                }
            }
            .ok_or_else(|| failed("Missing regular-file conflict mode"))?;
            let object = self.input(&["hash-object", "-w", "--stdin"], &content)?;
            let object = text_line(&object.stdout, "resolved blob")?;
            self.install(
                path,
                Some(&IndexEntry {
                    mode: mode.to_owned(),
                    object,
                }),
                stages,
            )?;
            Ok(count)
        } else {
            let selected = choose(
                policy,
                resolve,
                &SyncConflict {
                    path: path.to_owned(),
                    description: if ours.is_none() || theirs.is_none() {
                        "Modify/delete or structural file conflict"
                    } else {
                        "Binary or file-type conflict"
                    }
                    .to_owned(),
                    ours,
                    theirs,
                },
            )?;
            let entry = match selected {
                ConflictChoice::Ours => &stages[1],
                ConflictChoice::Theirs => &stages[2],
            };
            self.install(path, entry.as_ref(), stages)?;
            Ok(1)
        }
    }

    fn install(&self, path: &Path, entry: Option<&IndexEntry>, stages: &Stages) -> Result<()> {
        safe_relative(path)?;
        // Refuse symlink ancestors even for paths outside the store: no filesystem escape.
        let mut ancestor = self.worktree.clone();
        if let Some(parent) = path.parent() {
            for component in parent.components() {
                ancestor.push(component);
                if let Ok(metadata) = fs::symlink_metadata(&ancestor) {
                    if !metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err(failed(
                            "Conflict path has a non-directory ancestor; resolve it manually",
                        )
                        .with_path(path));
                    }
                }
            }
        }
        let absolute = self.worktree.join(path);
        // Remove only a file/symlink, never recursively remove a directory or unrelated data.
        match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_dir() => {
                return Err(
                    failed("Conflict overlaps a directory; resolve it manually").with_path(path)
                )
            }
            Ok(_) => fs::remove_file(&absolute).map_err(|error| {
                failed(format!("Cannot replace conflict file: {error}")).with_path(path)
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failed(format!("Cannot inspect conflict file: {error}")).with_path(path))
            }
        }
        let object_length = stages
            .iter()
            .flatten()
            .next()
            .ok_or_else(|| failed("Missing conflict object"))?
            .object
            .len();
        let mut input = format!("0 {}\t", "0".repeat(object_length)).into_bytes();
        input.extend_from_slice(path.as_os_str().as_encoded_bytes());
        input.push(0);
        if let Some(entry) = entry {
            input.extend_from_slice(format!("{} {}\t", entry.mode, entry.object).as_bytes());
            input.extend_from_slice(path.as_os_str().as_encoded_bytes());
            input.push(0);
        }
        self.input(&["update-index", "-z", "--index-info"], &input)?;
        if entry.is_some() {
            self.checked_os(&[
                OsStr::new("checkout-index"),
                OsStr::new("--force"),
                OsStr::new("--"),
                path.as_os_str(),
            ])?;
        }
        Ok(())
    }

    fn merge_text(
        &self,
        path: &Path,
        sides: [&[u8]; 3],
        policy: Option<ConflictChoice>,
        resolve: &mut impl FnMut(&SyncConflict) -> Result<ConflictChoice>,
    ) -> Result<(Vec<u8>, usize)> {
        let [base, ours, theirs] = sides;
        let directory = tempfile::tempdir()
            .map_err(|error| failed(format!("Cannot create merge scratch space: {error}")))?;
        let paths = [
            directory.path().join("ours"),
            directory.path().join("base"),
            directory.path().join("theirs"),
        ];
        for (path, bytes) in paths.iter().zip([ours, base, theirs]) {
            fs::write(path, bytes)
                .map_err(|error| failed(format!("Cannot write merge scratch file: {error}")))?;
        }
        let marker = format!("otodo-{}", ulid::Ulid::new());
        let our_label = format!("{marker}-ours");
        let their_label = format!("{marker}-theirs");
        let marker_size = [base, ours, theirs]
            .into_iter()
            .flat_map(|bytes| bytes.split(|byte| *byte == b'\n'))
            .map(|line| {
                line.first().map_or(0, |first| {
                    if matches!(first, b'<' | b'>' | b'=' | b'|') {
                        line.iter().take_while(|byte| *byte == first).count()
                    } else {
                        0
                    }
                })
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(40);
        let size_option = format!("--marker-size={marker_size}");
        let output = self
            .command()
            .args([
                "merge-file",
                "--stdout",
                "--diff3",
                &size_option,
                "-L",
                &our_label,
                "-L",
                &marker,
                "-L",
                &their_label,
                "--",
            ])
            .args(&paths)
            .output()
            .map_err(spawn_error)?;
        // merge-file returns the conflict count (capped at 127), not just 0/1.
        // Errors have negative exit values, exposed as 128..=255 on Unix.
        let expected = match output.status.code() {
            Some(count @ 0..=127) => count as usize,
            _ => return Err(git_error("merge conflict text", &output)),
        };
        let (content, count) =
            resolve_hunks(path, &output.stdout, &marker, marker_size, policy, resolve)?;
        if count.min(127) != expected {
            return Err(failed(
                "Git conflict count does not match the resolved text; refusing to replace the file",
            ));
        }
        Ok((content, count))
    }
}

#[derive(Clone, Debug)]
struct IndexEntry {
    mode: String,
    object: String,
}
type Stages = [Option<IndexEntry>; 3];

fn index_conflicts(bytes: &[u8]) -> Result<BTreeMap<PathBuf, Stages>> {
    let mut entries = BTreeMap::new();
    for record in nul_records(bytes)? {
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| failed("Malformed Git index output"))?;
        let header = text(&record[..tab], "index entry")?;
        let fields: Vec<_> = header.split(' ').collect();
        if fields.len() != 3
            || !matches!(fields[0], "100644" | "100755" | "120000" | "160000")
            || !matches!(fields[1].len(), 40 | 64)
            || !fields[1].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(failed("Malformed Git conflict index entry"));
        }
        let stage = match fields[2] {
            "1" => 0,
            "2" => 1,
            "3" => 2,
            _ => return Err(failed("Invalid Git conflict stage")),
        };
        let path = PathBuf::from(os_bytes(&record[tab + 1..])?);
        safe_relative(&path)?;
        let stages: &mut Stages = entries.entry(path).or_insert_with(|| [None, None, None]);
        if stages[stage].is_some() {
            return Err(failed("Duplicate Git conflict stage"));
        }
        stages[stage] = Some(IndexEntry {
            mode: fields[0].to_owned(),
            object: fields[1].to_owned(),
        });
    }
    Ok(entries)
}

fn status_paths(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    let mut records = nul_records(bytes)?.into_iter();
    let mut paths = Vec::new();
    while let Some(record) = records.next() {
        if record.len() < 4 || record[2] != b' ' {
            return Err(failed("Malformed Git status output"));
        }
        let path = PathBuf::from(os_bytes(&record[3..])?);
        safe_relative(&path)?;
        paths.push(path);
        if record[..2].iter().any(|byte| matches!(byte, b'R' | b'C')) {
            let source = records
                .next()
                .ok_or_else(|| failed("Missing Git rename source"))?;
            let path = PathBuf::from(os_bytes(source)?);
            safe_relative(&path)?;
            paths.push(path);
        }
    }
    Ok(paths)
}

fn nul_records(bytes: &[u8]) -> Result<Vec<&[u8]>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&0) {
        return Err(failed("Unterminated Git NUL output"));
    }
    let records: Vec<_> = bytes[..bytes.len() - 1].split(|byte| *byte == 0).collect();
    if records.iter().any(|record| record.is_empty()) {
        return Err(failed("Empty Git NUL record"));
    }
    Ok(records)
}

fn safe_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(failed("Git returned an unsafe relative path").with_path(path));
    }
    Ok(())
}

fn resolve_hunks(
    path: &Path,
    bytes: &[u8],
    marker: &str,
    marker_size: usize,
    policy: Option<ConflictChoice>,
    resolve: &mut impl FnMut(&SyncConflict) -> Result<ConflictChoice>,
) -> Result<(Vec<u8>, usize)> {
    let start = format!("{} {marker}-ours", "<".repeat(marker_size));
    let base = format!("{} {marker}", "|".repeat(marker_size));
    let middle = "=".repeat(marker_size);
    let end = format!("{} {marker}-theirs", ">".repeat(marker_size));
    let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
    let mut result = Vec::with_capacity(bytes.len());
    let mut count = 0;
    while let Some(line) = lines.next() {
        if trim_line_ending(line) != start.as_bytes() {
            result.extend_from_slice(line);
            continue;
        }
        let mut ours = Vec::new();
        let mut theirs = Vec::new();
        let mut state = 0;
        let mut closed = false;
        for line in lines.by_ref() {
            let marker_line = trim_line_ending(line);
            if state == 0 && marker_line == base.as_bytes() {
                state = 1;
            } else if state == 1 && marker_line == middle.as_bytes() {
                state = 2;
            } else if state == 2 && marker_line == end.as_bytes() {
                closed = true;
                break;
            } else if state == 0 {
                ours.extend_from_slice(line);
            } else if state == 2 {
                theirs.extend_from_slice(line);
            }
        }
        if !closed {
            return Err(failed("Malformed uniquely delimited text conflict"));
        }
        count += 1;
        let conflict = SyncConflict {
            path: path.to_owned(),
            description: format!("Text conflict hunk {count}"),
            ours: Some(ours),
            theirs: Some(theirs),
        };
        let selected = choose(policy, resolve, &conflict)?;
        result.extend_from_slice(match selected {
            ConflictChoice::Ours => conflict.ours.as_deref().unwrap_or_default(),
            ConflictChoice::Theirs => conflict.theirs.as_deref().unwrap_or_default(),
        });
    }
    Ok((result, count))
}

fn choose(
    policy: Option<ConflictChoice>,
    resolve: &mut impl FnMut(&SyncConflict) -> Result<ConflictChoice>,
    conflict: &SyncConflict,
) -> Result<ConflictChoice> {
    match policy {
        Some(choice) => Ok(choice),
        None => resolve(conflict),
    }
}

fn trim_lf(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}

fn trim_line_ending(bytes: &[u8]) -> &[u8] {
    let bytes = trim_lf(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn text(bytes: &[u8], description: &str) -> Result<String> {
    String::from_utf8(bytes.to_vec())
        .map_err(|_| precondition(format!("The {description} must be UTF-8")))
}
fn text_line(bytes: &[u8], description: &str) -> Result<String> {
    text(trim_lf(bytes), description)
}

#[cfg(unix)]
fn os_bytes(bytes: &[u8]) -> Result<OsString> {
    use std::os::unix::ffi::OsStrExt;
    Ok(OsStr::from_bytes(bytes).to_os_string())
}
#[cfg(not(unix))]
fn os_bytes(bytes: &[u8]) -> Result<OsString> {
    text(bytes, "Git path").map(OsString::from)
}

fn precondition(message: impl Into<String>) -> Error {
    Error::validation("sync_precondition", message)
}
fn failed(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Io, "sync_failed", message)
}
fn spawn_error(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        Error::unsupported(
            "sync_unavailable",
            "Git is not available; install Git to use /sync",
        )
    } else {
        failed(format!("Cannot run Git: {error}"))
    }
}
fn git_error(action: &str, output: &Output) -> Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    failed(format!(
        "Git could not {action}: {}{}",
        stderr.trim(),
        if stderr.is_empty() { stdout.trim() } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_status_checks_both_paths_without_prefix_or_newline_confusion() {
        let paths =
            status_paths(b"R  todos/task\nname.md\0todos-other/task.md\0?? todos/-new file\0")
                .unwrap();
        assert!(paths[0].starts_with("todos"));
        assert!(!paths[1].starts_with("todos"));
        assert_eq!(paths[2], Path::new("todos/-new file"));
        assert!(status_paths(b"R  todos/task\0").is_err());
    }

    #[test]
    fn hunk_choices_preserve_clean_regions_and_literal_normal_markers() {
        let marker = "unique";
        let bytes = format!("incoming edit\n<<<<<<< ordinary content\n{} {marker}-ours\nlocal\n{} {marker}\nbase\n{}\nremote\n{} {marker}-theirs\nlocal clean edit\n", "<".repeat(40), "|".repeat(40), "=".repeat(40), ">".repeat(40));
        let mut called = false;
        let (merged, count) = resolve_hunks(
            Path::new("todos/a.md"),
            bytes.as_bytes(),
            marker,
            40,
            Some(ConflictChoice::Theirs),
            &mut |_| {
                called = true;
                Ok(ConflictChoice::Ours)
            },
        )
        .unwrap();
        assert_eq!(
            merged,
            b"incoming edit\n<<<<<<< ordinary content\nremote\nlocal clean edit\n"
        );
        assert_eq!(count, 1);
        assert!(!called);
    }

    #[test]
    fn crlf_hunks_prompt_separately_without_normalizing_content() {
        let hunk = |ours, theirs| {
            format!(
                "{} unique-ours\r\n{ours}\r\n{} unique\r\nbase\r\n{}\r\n{theirs}\r\n{} unique-theirs\r\n",
                "<".repeat(40), "|".repeat(40), "=".repeat(40), ">".repeat(40)
            )
        };
        let source = format!(
            "before\r\n{}between\r\n{}after\r\n",
            hunk("local first", "remote first"),
            hunk("local second", "remote second"),
        );
        let mut prompts = 0;
        let (merged, count) = resolve_hunks(
            Path::new("task.md"),
            source.as_bytes(),
            "unique",
            40,
            None,
            &mut |conflict| {
                prompts += 1;
                assert!(conflict.ours.as_ref().unwrap().ends_with(b"\r\n"));
                Ok(if prompts == 1 {
                    ConflictChoice::Ours
                } else {
                    ConflictChoice::Theirs
                })
            },
        )
        .unwrap();
        assert_eq!(prompts, 2);
        assert_eq!(count, 2);
        assert_eq!(
            merged,
            b"before\r\nlocal first\r\nbetween\r\nremote second\r\nafter\r\n"
        );
    }

    #[test]
    fn malformed_hunk_never_silently_drops_the_remaining_file() {
        let bytes = format!("{} unique-ours\nunclosed\n", "<".repeat(40));
        assert!(resolve_hunks(
            Path::new("a"),
            bytes.as_bytes(),
            "unique",
            40,
            Some(ConflictChoice::Ours),
            &mut |_| unreachable!()
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn index_paths_preserve_non_utf8_and_tabs() {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = format!("100644 {} 2\t", "a".repeat(40)).into_bytes();
        bytes.extend_from_slice(b"todos/\xff\tname\0");
        let entries = index_conflicts(&bytes).unwrap();
        assert_eq!(
            entries.keys().next().unwrap().as_os_str().as_bytes(),
            b"todos/\xff\tname"
        );
    }
}
