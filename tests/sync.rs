use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Output};

use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;

struct Repository {
    _temp: TempDir,
    local: PathBuf,
    peer: PathBuf,
    remote: PathBuf,
    environment: Vec<(OsString, OsString)>,
    task_path: String,
    task_source: String,
}

impl Repository {
    fn new() -> Self {
        let temp = TempDir::new().expect("temporary repositories");
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let environment = vec![
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("HOME".into(), home.clone().into_os_string()),
            ("XDG_CONFIG_HOME".into(), home.into_os_string()),
            ("LC_ALL".into(), "C".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_SYSTEM".into(), "/dev/null".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_EDITOR".into(), "false".into()),
            ("GIT_AUTHOR_DATE".into(), "2026-01-01T00:00:00Z".into()),
            ("GIT_COMMITTER_DATE".into(), "2026-01-01T00:00:00Z".into()),
            ("GIT_CONFIG_COUNT".into(), "7".into()),
            ("GIT_CONFIG_KEY_0".into(), "user.name".into()),
            ("GIT_CONFIG_VALUE_0".into(), "Sync Test".into()),
            ("GIT_CONFIG_KEY_1".into(), "user.email".into()),
            ("GIT_CONFIG_VALUE_1".into(), "sync@example.invalid".into()),
            ("GIT_CONFIG_KEY_2".into(), "commit.gpgSign".into()),
            ("GIT_CONFIG_VALUE_2".into(), "false".into()),
            ("GIT_CONFIG_KEY_3".into(), "tag.gpgSign".into()),
            ("GIT_CONFIG_VALUE_3".into(), "false".into()),
            ("GIT_CONFIG_KEY_4".into(), "core.hooksPath".into()),
            ("GIT_CONFIG_VALUE_4".into(), "/dev/null".into()),
            ("GIT_CONFIG_KEY_5".into(), "protocol.file.allow".into()),
            ("GIT_CONFIG_VALUE_5".into(), "always".into()),
            ("GIT_CONFIG_KEY_6".into(), "init.defaultBranch".into()),
            ("GIT_CONFIG_VALUE_6".into(), "main".into()),
        ];
        let mut repository = Self {
            local: temp.path().join("local"),
            peer: temp.path().join("peer"),
            remote: temp.path().join("remote.git"),
            environment,
            _temp: temp,
            task_path: String::new(),
            task_source: String::new(),
        };
        let parent = repository._temp.path();
        repository.git(parent, &["init", "--bare", "remote.git"]);
        repository.git(parent, &["clone", "remote.git", "local"]);
        repository.cli_success(&repository.local, &["init", "Todo", "--vault-root", "."]);
        // Git cannot preserve an empty managed Projects directory in a clone.
        repository.cli_success(
            &repository.local,
            &[
                "--root", "Todo", "project", "create", "work", "--name", "Work",
            ],
        );
        let task = repository.cli_success(&repository.local, &["--root", "Todo", "add", "Shared"]);
        repository.task_path = format!("Todo/{}", task["task"]["path"].as_str().unwrap());
        let path = repository.local.join(&repository.task_path);
        let mut source = fs::read_to_string(&path).unwrap();
        source.push_str("\nLocal independent: base\n");
        for line in 0..12 {
            source.push_str(&format!("Unchanged leading context {line}\n"));
        }
        source.push_str("Contested: base\n");
        for line in 0..12 {
            source.push_str(&format!("Unchanged trailing context {line}\n"));
        }
        source.push_str("Remote independent: base\n");
        fs::write(path, &source).unwrap();
        repository.task_source = source;
        fs::create_dir(repository.local.join("Notes")).unwrap();
        fs::write(
            repository.local.join("Notes/unrelated.md"),
            b"unrelated baseline\n",
        )
        .unwrap();
        repository.git(&repository.local, &["add", "--", "Todo", "Notes"]);
        repository.git(&repository.local, &["commit", "-m", "Initial store"]);
        repository.git(&repository.local, &["push", "-u", "origin", "main"]);
        repository.git(parent, &["clone", "remote.git", "peer"]);
        repository
    }

    fn command(&self, cwd: &Path) -> Command {
        let mut command = cargo_bin_cmd!("otodo");
        command
            .env_clear()
            .envs(self.environment.clone())
            .current_dir(cwd);
        command
    }

    fn git_output(&self, cwd: &Path, args: &[&str]) -> Output {
        ProcessCommand::new("git")
            .env_clear()
            .envs(self.environment.clone())
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run local Git")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git_output(cwd, args);
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn cli_success(&self, cwd: &Path, args: &[&str]) -> Value {
        let output = self
            .command(cwd)
            .args(args)
            .arg("--format=json")
            .output()
            .unwrap();
        assert_success(&output);
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["version"], 1);
        envelope
    }

    fn input(&self, lines: &str) -> Output {
        self.command(&self.local)
            .args(["--root", "Todo", "input", "--format", "json"])
            .write_stdin(lines)
            .output()
            .unwrap()
    }

    fn sync_success(&self, lines: &str, committed: bool, conflicts: usize) -> Vec<Value> {
        let output = self.input(lines);
        assert_success(&output);
        let envelopes = envelopes(&output.stdout);
        assert!(
            envelopes.iter().any(|value| {
                *value
                    == json!({"version": 1, "sync": {
                        "branch": "main", "upstream": "origin/main",
                        "committed": committed, "conflicts_resolved": conflicts,
                    }})
            }),
            "missing sync envelope: {envelopes:?}"
        );
        envelopes
    }

    fn commit_peer(&self) {
        self.git(&self.peer, &["add", "--", "Todo"]);
        self.git(&self.peer, &["commit", "-m", "Remote store changes"]);
        self.git(&self.peer, &["push", "origin", "main"]);
    }

    fn diverge(&self) -> (String, String) {
        let ours = self
            .task_source
            .replace("Local independent: base", "Local independent: retained")
            .replace("Contested: base", "Contested: ours");
        let theirs = self
            .task_source
            .replace("Remote independent: base", "Remote independent: retained")
            .replace("Contested: base", "Contested: theirs");
        fs::write(self.local.join(&self.task_path), &ours).unwrap();
        fs::write(self.peer.join(&self.task_path), &theirs).unwrap();
        self.commit_peer();
        (ours, theirs)
    }

    fn remote_head(&self) -> String {
        self.git(&self.remote, &["rev-parse", "refs/heads/main"])
    }

    fn head(&self) -> String {
        self.git(&self.local, &["rev-parse", "HEAD"])
    }

    fn remote_file(&self, path: &str) -> String {
        self.git(&self.remote, &["show", &format!("refs/heads/main:{path}")])
    }

    fn assert_clean(&self) {
        assert_eq!(
            self.git(
                &self.local,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            ""
        );
        assert!(!self.local.join(".git/MERGE_HEAD").exists());
    }
}

fn assert_success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.stdout.contains(&0x1b));
}

fn envelopes(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON envelope per line"))
        .collect()
}

fn assert_failure(output: &Output, exit: i32, code: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("only JSON on stderr");
    assert_eq!(error["version"], 1);
    assert_eq!(error["error"]["code"], code);
    error
}

#[test]
fn sync_commits_only_pending_store_changes_and_pushes_the_branch() {
    let repository = Repository::new();
    fs::write(
        repository.local.join("Notes/unrelated.md"),
        b"already committed unrelated change\n",
    )
    .unwrap();
    repository.git(&repository.local, &["add", "--", "Notes/unrelated.md"]);
    repository.git(
        &repository.local,
        &["commit", "-m", "Unrelated committed work"],
    );
    let parent = repository.head();
    let added = repository.cli_success(
        &repository.local,
        &["--root", "Todo", "add", "Locally captured"],
    );
    let path = format!("Todo/{}", added["task"]["path"].as_str().unwrap());
    let source = fs::read_to_string(repository.local.join(&path)).unwrap();
    let result = repository.sync_success("/sync\n", true, 0);
    assert_eq!(result.len(), 1);
    assert_eq!(repository.head(), repository.remote_head());
    assert_eq!(
        repository.git(&repository.local, &["rev-parse", "HEAD^"]),
        parent
    );
    assert_eq!(
        repository.git(
            &repository.local,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        ),
        format!("{path}\n")
    );
    assert_eq!(repository.remote_file(&path), source);
    assert_eq!(
        repository.remote_file("Notes/unrelated.md"),
        "already committed unrelated change\n"
    );
    repository.assert_clean();
}

#[test]
fn sync_fast_forwards_remote_task_changes_without_an_extra_commit() {
    let repository = Repository::new();
    let updated = repository
        .task_source
        .replace("Contested: base", "Contested: remote update");
    fs::write(repository.peer.join(&repository.task_path), &updated).unwrap();
    repository.commit_peer();
    let remote = repository.remote_head();
    assert_eq!(repository.sync_success("/sync\n", false, 0).len(), 1);
    assert_eq!(repository.head(), remote);
    assert_eq!(
        fs::read_to_string(repository.local.join(&repository.task_path)).unwrap(),
        updated
    );
    repository.assert_clean();
}

#[test]
fn mixed_input_reloads_config_and_catalog_after_sync_before_further_captures() {
    let repository = Repository::new();
    repository.cli_success(
        &repository.peer,
        &[
            "--root", "Todo", "project", "create", "remote", "--name", "Remote",
        ],
    );
    let remote_task = repository.cli_success(
        &repository.peer,
        &["--root", "Todo", "add", "Imported", "--project", "remote"],
    );
    let config_path = repository.peer.join("Todo/.todo/config.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("default_state = \"open\"", "default_state = \"active\"");
    fs::write(config_path, &config).unwrap();
    repository.commit_peer();
    let old_config = fs::File::open(repository.local.join("Todo/.todo/config.toml")).unwrap();
    let values = repository.sync_success(
        "Before sync\n/sync\n/list #remote\nAfter sync #remote\n",
        true,
        0,
    );
    assert_eq!(values.len(), 4);
    assert_eq!(values[0]["task"]["name"], "Before sync");
    assert_eq!(values[0]["task"]["state"], "open");
    assert_eq!(
        values[2],
        json!({"version": 1, "tasks": [remote_task["task"]]})
    );
    assert_eq!(values[3]["task"]["name"], "After sync");
    assert_eq!(values[3]["task"]["state"], "active");
    assert_eq!(values[3]["task"]["projects"], json!(["remote"]));
    assert_eq!(
        fs::read_to_string(repository.local.join("Todo/.todo/config.toml")).unwrap(),
        config
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(
            old_config.metadata().unwrap().ino(),
            fs::metadata(repository.local.join("Todo/.todo/config.toml"))
                .unwrap()
                .ino()
        );
    }
    drop(old_config);
    let before_path = format!("Todo/{}", values[0]["task"]["path"].as_str().unwrap());
    assert_eq!(
        repository.remote_file(&before_path),
        fs::read_to_string(repository.local.join(&before_path)).unwrap()
    );
    let after_path = format!("Todo/{}", values[3]["task"]["path"].as_str().unwrap());
    assert!(repository.local.join(&after_path).is_file());
    assert!(!repository
        .git_output(
            &repository.remote,
            &["cat-file", "-e", &format!("main:{after_path}")]
        )
        .status
        .success());
    assert_eq!(
        repository.git(
            &repository.local,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        format!("?? {after_path}\n")
    );
    assert_eq!(repository.head(), repository.remote_head());
}

#[test]
fn sync_policies_resolve_only_conflicting_text_and_keep_both_independent_edits() {
    for policy in ["ours", "theirs"] {
        let repository = Repository::new();
        repository.diverge();
        let remote_parent = repository.remote_head();
        let values = repository.sync_success(&format!("/sync {policy}\n"), true, 1);
        assert_eq!(values.len(), 1);
        let expected = repository
            .task_source
            .replace("Local independent: base", "Local independent: retained")
            .replace("Remote independent: base", "Remote independent: retained")
            .replace("Contested: base", &format!("Contested: {policy}"));
        assert_eq!(
            fs::read_to_string(repository.local.join(&repository.task_path)).unwrap(),
            expected,
            "policy {policy}"
        );
        assert_eq!(repository.remote_file(&repository.task_path), expected);
        assert_eq!(
            repository.git(&repository.local, &["rev-parse", "HEAD^2"]),
            remote_parent
        );
        assert_eq!(repository.head(), repository.remote_head());
        repository.assert_clean();
    }
}

#[test]
fn sync_crlf_conflicts_preserve_selected_and_nonconflicting_body_bytes() {
    let mut repository = Repository::new();
    repository.task_source = repository.task_source.replace('\n', "\r\n");
    fs::write(
        repository.local.join(&repository.task_path),
        &repository.task_source,
    )
    .unwrap();
    repository.git(&repository.local, &["add", "--", "Todo"]);
    repository.git(&repository.local, &["commit", "-m", "Use CRLF notes"]);
    repository.git(&repository.local, &["push"]);
    repository.git(&repository.peer, &["pull", "--ff-only"]);
    repository.diverge();
    repository.sync_success("/sync ours\n", true, 1);
    let expected = repository
        .task_source
        .replace("Local independent: base", "Local independent: retained")
        .replace("Remote independent: base", "Remote independent: retained")
        .replace("Contested: base", "Contested: ours");
    assert_eq!(
        fs::read(repository.local.join(&repository.task_path)).unwrap(),
        expected.as_bytes()
    );
    assert_eq!(
        repository.remote_file(&repository.task_path).as_bytes(),
        expected.as_bytes()
    );
    repository.assert_clean();
}

#[test]
fn failed_sync_preserves_merged_store_diagnostics_when_reopening_would_fail() {
    let repository = Repository::new();
    let config_path = repository.peer.join("Todo/.todo/config.toml");
    let config = fs::read_to_string(&config_path)
        .unwrap()
        .replace("schema_version = 2", "schema_version = 99");
    fs::write(config_path, &config).unwrap();
    repository.commit_peer();
    let remote = repository.remote_head();
    let error = assert_failure(
        &repository.input("/sync\nMust not be captured\n"),
        7,
        "validation_failed",
    );
    assert!(error["error"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| { issue["code"] == "unsupported_schema" }));
    let validation = repository
        .command(&repository.local)
        .args(["--root", "Todo", "--format=json", "validate"])
        .output()
        .unwrap();
    let validation: Value = serde_json::from_slice(&validation.stderr).unwrap();
    assert_eq!(error["error"]["issues"], validation["error"]["issues"]);
    assert_eq!(repository.head(), remote);
    assert_eq!(repository.remote_head(), remote);
    assert_eq!(
        fs::read_to_string(repository.local.join("Todo/.todo/config.toml")).unwrap(),
        config,
    );
    repository.assert_clean();
}

#[test]
fn sync_without_policy_stops_input_and_aborts_only_its_merge_preserving_local_commit() {
    let repository = Repository::new();
    let initial_head = repository.head();
    let (ours, _) = repository.diverge();
    let remote_head = repository.remote_head();
    let output = repository.input("/sync\nNever captured\n");
    assert_failure(&output, 2, "sync_conflict");
    assert_eq!(
        repository.git(&repository.local, &["rev-parse", "HEAD^"]),
        initial_head
    );
    assert_eq!(
        repository.git(
            &repository.local,
            &["show", &format!("HEAD:{}", repository.task_path)]
        ),
        ours
    );
    assert_eq!(
        fs::read_to_string(repository.local.join(&repository.task_path)).unwrap(),
        ours
    );
    assert_eq!(repository.remote_head(), remote_head);
    repository.assert_clean();
    let tasks = repository.cli_success(&repository.local, &["--root", "Todo", "list", "--all"]);
    assert_eq!(
        tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["Shared"]
    );
}

#[test]
fn sync_refuses_outside_store_changes_without_committing_or_staging_them() {
    for kind in ["staged", "unstaged", "untracked"] {
        let repository = Repository::new();
        let task = repository.cli_success(
            &repository.local,
            &["--root", "Todo", "add", "Pending local task"],
        );
        let task_path = repository
            .local
            .join("Todo")
            .join(task["task"]["path"].as_str().unwrap());
        let task_bytes = fs::read(&task_path).unwrap();
        let path = if kind == "untracked" {
            "Notes/new.md"
        } else {
            "Notes/unrelated.md"
        };
        fs::write(repository.local.join(path), b"outside pending work\n").unwrap();
        if kind == "staged" {
            repository.git(&repository.local, &["add", "--", path]);
        }
        let head = repository.head();
        let remote = repository.remote_head();
        let status = repository.git(
            &repository.local,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );
        let staged = repository.git(&repository.local, &["diff", "--cached", "--binary"]);
        let unstaged = repository.git(&repository.local, &["diff", "--binary"]);
        assert_failure(
            &repository.input("/sync ours\nNever captured\n"),
            5,
            "sync_precondition",
        );
        assert_eq!(repository.head(), head, "{kind}");
        assert_eq!(repository.remote_head(), remote, "{kind}");
        assert_eq!(
            repository.git(
                &repository.local,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            status,
            "{kind}"
        );
        assert_eq!(
            repository.git(&repository.local, &["diff", "--cached", "--binary"]),
            staged,
            "{kind}"
        );
        assert_eq!(
            repository.git(&repository.local, &["diff", "--binary"]),
            unstaged,
            "{kind}"
        );
        assert_eq!(
            fs::read(repository.local.join(path)).unwrap(),
            b"outside pending work\n"
        );
        assert_eq!(fs::read(task_path).unwrap(), task_bytes);
        assert!(!repository.local.join(".git/MERGE_HEAD").exists());
    }
}

#[test]
fn sync_requires_an_upstream_before_committing_local_changes() {
    let repository = Repository::new();
    repository.git(&repository.local, &["branch", "--unset-upstream"]);
    let source = repository
        .task_source
        .replace("Contested: base", "Contested: pending");
    fs::write(repository.local.join(&repository.task_path), &source).unwrap();
    let head = repository.head();
    assert_failure(&repository.input("/sync\n"), 5, "sync_precondition");
    assert_eq!(repository.head(), head);
    assert_eq!(repository.remote_head(), head);
    assert_eq!(
        fs::read_to_string(repository.local.join(&repository.task_path)).unwrap(),
        source
    );
    assert_eq!(
        repository.git(&repository.local, &["diff", "--cached", "--name-only"]),
        ""
    );
    assert!(!repository.local.join(".git/MERGE_HEAD").exists());
}

#[test]
fn sync_preserves_a_preexisting_conflicted_merge_instead_of_aborting_it() {
    let repository = Repository::new();
    repository.diverge();
    repository.git(&repository.local, &["add", "--", "Todo"]);
    repository.git(&repository.local, &["commit", "-m", "Local changes"]);
    repository.git(&repository.local, &["fetch", "origin"]);
    let merge = repository.git_output(&repository.local, &["merge", "--no-edit", "origin/main"]);
    assert_eq!(merge.status.code(), Some(1));
    let merge_head = fs::read(repository.local.join(".git/MERGE_HEAD")).unwrap();
    let source = fs::read(repository.local.join(&repository.task_path)).unwrap();
    let unmerged = repository.git(&repository.local, &["ls-files", "--unmerged"]);
    let head = repository.head();
    let remote = repository.remote_head();
    assert_failure(&repository.input("/sync theirs\n"), 5, "sync_precondition");
    assert_eq!(
        fs::read(repository.local.join(".git/MERGE_HEAD")).unwrap(),
        merge_head
    );
    assert_eq!(
        fs::read(repository.local.join(&repository.task_path)).unwrap(),
        source
    );
    assert_eq!(
        repository.git(&repository.local, &["ls-files", "--unmerged"]),
        unmerged
    );
    assert_eq!(repository.head(), head);
    assert_eq!(repository.remote_head(), remote);
}

#[test]
fn malformed_sync_options_stop_before_fetch_staging_or_committing() {
    let repository = Repository::new();
    let source = repository
        .task_source
        .replace("Contested: base", "Contested: pending");
    fs::write(repository.local.join(&repository.task_path), &source).unwrap();
    let head = repository.head();
    for line in [
        "/sync wrong",
        "/sync OURS",
        "/sync --ours",
        "/sync ours theirs",
        "/sync theirs extra",
    ] {
        let error = assert_failure(
            &repository.input(&format!("{line}\nNever captured\n")),
            2,
            "invalid_sync_option",
        );
        assert_eq!(error["error"]["field"], "input");
        assert_eq!(repository.head(), head);
        assert_eq!(repository.remote_head(), head);
        assert!(!repository.local.join(".git/FETCH_HEAD").exists());
        assert!(!repository.local.join(".git/MERGE_HEAD").exists());
        assert_eq!(
            repository.git(&repository.local, &["diff", "--cached", "--name-only"]),
            ""
        );
        assert_eq!(
            repository.git(
                &repository.local,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            format!(" M {}\n", repository.task_path)
        );
        assert_eq!(
            fs::read_to_string(repository.local.join(&repository.task_path)).unwrap(),
            source
        );
    }
}

#[test]
fn sparse_store_checkout_syncs_without_materializing_unrelated_directories() {
    let repository = Repository::new();
    repository.git(&repository.local, &["sparse-checkout", "init", "--cone"]);
    repository.git(&repository.local, &["sparse-checkout", "set", "Todo"]);
    assert!(!repository.local.join("Notes").exists());
    fs::write(
        repository.peer.join("Notes/unrelated.md"),
        b"remote unrelated update\n",
    )
    .unwrap();
    repository.git(&repository.peer, &["add", "--", "Notes"]);
    let remote_task = repository.cli_success(
        &repository.peer,
        &["--root", "Todo", "add", "Remote sparse task"],
    );
    repository.commit_peer();
    let local_task = repository.cli_success(
        &repository.local,
        &["--root", "Todo", "add", "Local sparse task"],
    );
    assert_eq!(repository.sync_success("/sync\n", true, 0).len(), 1);
    for task in [local_task, remote_task] {
        let path = format!("Todo/{}", task["task"]["path"].as_str().unwrap());
        assert_eq!(
            repository.remote_file(&path),
            fs::read_to_string(repository.local.join(path)).unwrap()
        );
    }
    assert_eq!(
        repository.git(&repository.local, &["show", "HEAD:Notes/unrelated.md"]),
        "remote unrelated update\n"
    );
    assert!(!repository.local.join("Notes").exists());
    assert_eq!(repository.head(), repository.remote_head());
    repository.assert_clean();
}
