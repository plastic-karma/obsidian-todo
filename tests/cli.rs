use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::cargo::cargo_bin_cmd;
use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn command() -> Command {
    cargo_bin_cmd!("otodo")
}

fn fake_vault() -> TempDir {
    let temp = TempDir::new().expect("temporary vault");
    fs::create_dir(temp.path().join(".obsidian")).expect("Obsidian marker");
    fs::create_dir(temp.path().join(".git")).expect("Git marker");
    fs::write(temp.path().join("unrelated.md"), b"unrelated\r\ncontent\n").expect("unrelated note");
    fs::write(
        temp.path().join(".obsidian/community-plugins.json"),
        b"[\"obsidian-git\"]",
    )
    .expect("Obsidian configuration");
    temp
}

fn initialize(vault: &Path) -> PathBuf {
    command()
        .current_dir(vault)
        .args(["init", "Todo", "--vault-root", "."])
        .assert()
        .success()
        .stderr("");
    vault.join("Todo")
}

fn json_success(cwd: &Path, arguments: &[&str]) -> Value {
    let output = command()
        .current_dir(cwd)
        .args(arguments)
        .arg("--format")
        .arg("json")
        .output()
        .expect("run command");
    assert!(
        output.status.success(),
        "status={:?}\nstdout={}\nstderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(!output.stdout.contains(&0x1b));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(value["version"], 1);
    value
}

fn json_failure(cwd: &Path, arguments: &[&str], exit: i32, code: &str) -> Value {
    let output = command()
        .current_dir(cwd)
        .args(arguments)
        .arg("--format=json")
        .output()
        .expect("run command");
    assert_eq!(output.status.code(), Some(exit));
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.contains(&0x1b));
    let value: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(value["version"], 1);
    assert_eq!(value["error"]["code"], code);
    let error = value["error"].as_object().expect("JSON error object");
    for field in [
        "code", "message", "path", "field", "line", "column", "issues",
    ] {
        assert!(
            error.contains_key(field),
            "missing JSON error field {field}"
        );
    }
    value
}

fn task_id(value: &Value) -> &str {
    value["task"]["id"].as_str().expect("task ID")
}

#[test]
fn initialization_is_contained_and_minimal_task_is_exact_markdown() {
    let vault = fake_vault();
    let unrelated_before = fs::read(vault.path().join("unrelated.md")).expect("unrelated note");
    let obsidian_before =
        fs::read(vault.path().join(".obsidian/community-plugins.json")).expect("Obsidian config");

    let dry_run = json_success(
        vault.path(),
        &["init", "Preview", "--vault-root", ".", "--dry-run"],
    );
    assert_eq!(dry_run["dry_run"], true);
    assert!(!vault.path().join("Preview").exists());

    let root = initialize(vault.path());
    assert!(root.join(".todo/config.toml").is_file());
    assert!(root.join(".todo/schema.json").is_file());
    assert!(root.join("Tasks").is_dir());
    assert!(root.join("Projects").is_dir());
    assert!(!root.join(".git").exists());
    let config = fs::read_to_string(root.join(".todo/config.toml")).expect("configuration");
    assert!(config.contains("schema_version = 1"));
    assert!(config.contains("obsidian_link_prefix = \"Todo\""));

    let added = json_success(vault.path(), &["--root", "Todo", "add", "Minimal"]);
    assert_eq!(added["task"]["due_date"], Value::Null);
    assert_eq!(added["task"]["recurrence"], Value::Null);
    assert_eq!(added["task"]["recurrence_from"], Value::Null);
    assert_eq!(added["task"]["last_completed_date"], Value::Null);
    let id = task_id(&added);
    assert_eq!(id.len(), 26);
    let markdown = fs::read_to_string(root.join(format!("Tasks/{id}.md"))).expect("task file");
    assert_eq!(
        markdown,
        "---\nname: \"Minimal\"\nstate: open\nprojects: []\ntags: []\n---\n"
    );
    json_success(vault.path(), &["--root", "Todo", "validate"]);

    assert_eq!(
        fs::read(vault.path().join("unrelated.md")).expect("unrelated note"),
        unrelated_before
    );
    assert_eq!(
        fs::read(vault.path().join(".obsidian/community-plugins.json")).expect("Obsidian config"),
        obsidian_before
    );
}

#[test]
fn full_task_project_recurrence_and_unknown_property_workflow() {
    let vault = fake_vault();
    let unrelated_before = fs::read(vault.path().join("unrelated.md")).expect("unrelated note");
    let obsidian_before =
        fs::read(vault.path().join(".obsidian/community-plugins.json")).expect("Obsidian config");
    let root = initialize(vault.path());
    json_success(
        vault.path(),
        &[
            "--root",
            "Todo",
            "project",
            "create",
            "work",
            "--name",
            "Work",
            "--body",
            "Project notes",
        ],
    );
    let added = json_success(
        vault.path(),
        &[
            "--root",
            "Todo",
            "add",
            "Review plan",
            "--project",
            "work",
            "--tag",
            "review",
            "--tag",
            "Planning",
            "--due-date",
            "2026-09-07",
            "--recurrence",
            "freq=weekly;byday=mo",
            "--recurrence-from",
            "schedule",
            "--body",
            "Line one\r\nLine two",
        ],
    );
    let id = task_id(&added).to_owned();
    assert_eq!(added["task"]["projects"], serde_json::json!(["work"]));
    assert_eq!(
        added["task"]["recurrence"],
        "FREQ=WEEKLY;INTERVAL=1;BYDAY=MO"
    );
    assert_eq!(added["task"]["body"], "Line one\nLine two\n");

    let path = root.join(format!("Tasks/{id}.md"));
    let source = fs::read_to_string(&path).expect("task source");
    let closing = source.rfind("\n---\n").expect("closing delimiter") + 1;
    let mut externally_edited = source;
    externally_edited.insert_str(
        closing,
        "plugin-map:\n  color: blue\n  values: [1, two, null]\n",
    );
    fs::write(&path, externally_edited).expect("Obsidian property edit");

    let edited = json_success(
        vault.path(),
        &["--root", "Todo", "edit", &id[..6], "--state", "active"],
    );
    assert_eq!(edited["task"]["state"], "active");
    assert_eq!(
        edited["task"]["extra_properties"]["plugin-map"]["color"],
        "blue"
    );
    assert_eq!(edited["task"]["body"], "Line one\nLine two\n");

    let listed = json_success(
        vault.path(),
        &[
            "--root",
            "Todo",
            "list",
            "--state",
            "active",
            "--project",
            "work",
            "--tag",
            "review",
            "--due-before",
            "2026-09-08",
            "--recurring",
        ],
    );
    assert_eq!(listed["tasks"].as_array().expect("tasks").len(), 1);
    let human_list = command()
        .current_dir(vault.path())
        .args(["--root", "Todo", "list", "--state", "active"])
        .output()
        .expect("human list");
    assert!(human_list.status.success());
    assert!(human_list.stderr.is_empty());
    let human_list = String::from_utf8(human_list.stdout).expect("UTF-8 human list");
    for value in [&id, "active", "2026-09-07", "↻", "[work]", "Review plan"] {
        assert!(
            human_list.contains(value),
            "missing {value:?}: {human_list}"
        );
    }

    let completed = json_success(
        vault.path(),
        &["--root", "Todo", "--today", "2026-09-09", "complete", &id],
    );
    assert_eq!(completed["task"]["due_date"], "2026-09-14");
    assert_eq!(completed["task"]["last_completed_date"], "2026-09-09");
    assert_eq!(completed["task"]["state"], "open");

    json_success(vault.path(), &["--root", "Todo", "finish-series", &id]);
    let default_list = json_success(vault.path(), &["--root", "Todo", "list"]);
    assert!(default_list["tasks"].as_array().expect("tasks").is_empty());
    json_success(vault.path(), &["--root", "Todo", "reopen", &id]);
    json_success(vault.path(), &["--root", "Todo", "cancel", &id]);
    let relative = json_success(
        vault.path(),
        &[
            "--root",
            "Todo",
            "add",
            "Completion relative",
            "--due-date",
            "2026-09-01",
            "--recurrence",
            "FREQ=DAILY;INTERVAL=3",
            "--recurrence-from",
            "completion",
        ],
    );
    let relative_id = task_id(&relative).to_owned();
    let relative = json_success(
        vault.path(),
        &[
            "--root",
            "Todo",
            "--today",
            "2026-09-09",
            "complete",
            &relative_id,
        ],
    );
    assert_eq!(relative["task"]["due_date"], "2026-09-12");
    assert_eq!(relative["task"]["last_completed_date"], "2026-09-09");
    json_success(
        vault.path(),
        &["--root", "Todo", "finish-series", &relative_id],
    );

    let failure = json_failure(
        vault.path(),
        &["--root", "Todo", "project", "delete", "work", "--yes"],
        5,
        "project_in_use",
    );
    assert!(failure["error"]["message"]
        .as_str()
        .expect("message")
        .contains(&id));
    json_success(
        vault.path(),
        &["--root", "Todo", "edit", &id, "--remove-project", "work"],
    );
    json_success(
        vault.path(),
        &["--root", "Todo", "project", "delete", "work", "--yes"],
    );
    json_success(vault.path(), &["--root", "Todo", "validate"]);
    assert_eq!(
        fs::read(vault.path().join("unrelated.md")).expect("unrelated note"),
        unrelated_before
    );
    assert_eq!(
        fs::read(vault.path().join(".obsidian/community-plugins.json")).expect("Obsidian config"),
        obsidian_before
    );
}

#[test]
fn sparse_store_supports_body_sources_and_all_normal_commands_without_git() {
    let vault = fake_vault();
    let original = initialize(vault.path());
    let sparse = TempDir::new().expect("sparse parent");
    let root = sparse.path().join("OnlyTodo");
    copy_directory(&original, &root);
    assert!(!sparse.path().join(".git").exists());
    assert!(!sparse.path().join(".obsidian").exists());

    let body_file = sparse.path().join("body.md");
    fs::write(&body_file, b"from file\r\n").expect("body file");
    let body_path = body_file.to_str().expect("UTF-8 body path");
    let project = json_success_without_path(
        sparse.path(),
        &[
            "--root",
            "OnlyTodo",
            "project",
            "create",
            "work",
            "--name",
            "Work",
            "--body-file",
            body_path,
        ],
        None,
    );
    assert_eq!(project["project"]["body"], "from file\n");
    let added = json_success_without_path(
        sparse.path(),
        &[
            "--root",
            "OnlyTodo",
            "add",
            "Sparse task",
            "--project",
            "work",
            "--body-file",
            "-",
        ],
        Some("from stdin\r\n"),
    );
    let id = task_id(&added).to_owned();
    assert_eq!(added["task"]["body"], "from stdin\n");

    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "show", &id], None);
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "edit", &id, "--body", "argument body"],
        None,
    );
    let edited_from_file = json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "edit", &id, "--body-file", body_path],
        None,
    );
    assert_eq!(edited_from_file["task"]["body"], "from file\n");
    let edited_from_stdin = json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "edit", &id, "--body-file", "-"],
        Some("edited from stdin\r\n"),
    );
    assert_eq!(edited_from_stdin["task"]["body"], "edited from stdin\n");
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "list"], None);
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "complete", &id, "--on", "2026-09-02"],
        None,
    );
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "project", "list"],
        None,
    );
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "project", "show", "work"],
        None,
    );
    json_success_without_path(
        sparse.path(),
        &[
            "--root", "OnlyTodo", "project", "edit", "work", "--name", "Office",
        ],
        None,
    );
    json_success_without_path(
        sparse.path(),
        &[
            "--root",
            "OnlyTodo",
            "edit",
            &id,
            "--remove-project",
            "work",
        ],
        None,
    );
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "project", "delete", "work", "--yes"],
        None,
    );
    let survivor = json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "add", "Survivor"],
        None,
    );
    let survivor_id = task_id(&survivor).to_owned();
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "delete", &id, "--yes"],
        None,
    );
    assert!(!root.join(format!("Tasks/{id}.md")).exists());
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "show", &survivor_id],
        None,
    );
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "validate"], None);
}

#[test]
fn errors_have_stable_exit_codes_json_shape_and_leave_no_temporary_files() {
    let vault = fake_vault();
    let unrelated_before = fs::read(vault.path().join("unrelated.md")).expect("unrelated note");
    let obsidian_before =
        fs::read(vault.path().join(".obsidian/community-plugins.json")).expect("Obsidian config");
    let root = initialize(vault.path());
    json_failure(
        vault.path(),
        &[
            "--root",
            "Todo",
            "add",
            "Unsupported recurrence",
            "--due-date",
            "2026-09-07",
            "--recurrence",
            "FREQ=DAILY;COUNT=2",
            "--recurrence-from",
            "schedule",
        ],
        7,
        "unsupported_recurrence",
    );
    json_failure(
        vault.path(),
        &[
            "--root",
            "Todo",
            "add",
            "Missing project",
            "--project",
            "does-not-exist",
        ],
        5,
        "project_not_found",
    );
    let first_id = "01K4B0ZSBZZV25T1K0D3TA8JHR";
    let second_id = "01K4B0ZSBZZV25T1K0D3TA8JHS";
    let dangling_id = "01J3B0ZSBZZV25T1K0D3TA8JHR";
    let record =
        |name: &str| format!("---\nname: \"{name}\"\nstate: open\nprojects: []\ntags: []\n---\n");
    fs::write(root.join(format!("Tasks/{first_id}.md")), record("First")).expect("first");
    fs::write(root.join(format!("Tasks/{second_id}.md")), record("Second")).expect("second");
    fs::write(
        root.join(format!("Tasks/{dangling_id}.md")),
        "---\nname: Dangling\nstate: open\nprojects:\n  - \"[[Todo/Projects/missing]]\"\ntags: []\n---\n",
    )
    .expect("dangling task");

    let ambiguous = json_failure(
        vault.path(),
        &["--root", "Todo", "show", "01K4B0"],
        4,
        "ambiguous_task_id",
    );
    assert!(ambiguous["error"]["message"]
        .as_str()
        .expect("message")
        .contains(first_id));
    json_failure(
        vault.path(),
        &["--root", "Todo", "show", "71K4B0"],
        3,
        "task_not_found",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "delete", &first_id[..6], "--yes"],
        2,
        "full_id_required",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "delete", first_id],
        2,
        "confirmation_required",
    );
    json_success(
        vault.path(),
        &["--root", "Todo", "delete", second_id, "--yes"],
    );
    assert!(root.join(format!("Tasks/{first_id}.md")).is_file());
    assert!(!root.join(format!("Tasks/{second_id}.md")).exists());
    assert!(root.join(format!("Tasks/{dangling_id}.md")).is_file());

    let conflict = b"---\nname: First\nstate: open\nprojects: []\ntags: []\n---\n<<<<<<< ours\nbody\n=======\nother\n>>>>>>> theirs\n";
    fs::write(root.join(format!("Tasks/{first_id}.md")), conflict).expect("conflict");
    let validation = json_failure(
        vault.path(),
        &["--root", "Todo", "validate"],
        6,
        "validation_failed",
    );
    assert!(validation["error"]["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue["code"] == "unresolved_conflict"));
    assert!(validation["error"]["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue["code"] == "missing_project_reference"));
    json_failure(
        vault.path(),
        &["--root", "Todo", "edit", first_id, "--state", "active"],
        6,
        "unresolved_conflict",
    );
    assert_eq!(
        fs::read(root.join(format!("Tasks/{first_id}.md"))).expect("conflicted task"),
        conflict
    );
    assert!(fs::read_dir(root.join("Tasks"))
        .expect("tasks")
        .all(|entry| !entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")));

    let output = command()
        .current_dir(vault.path())
        .args(["--root", "Todo", "list"])
        .output()
        .expect("human command");
    assert!(!output.stdout.contains(&0x1b));
    assert!(!output.stderr.contains(&0x1b));
    let config_path = root.join(".todo/config.toml");
    let config = fs::read_to_string(&config_path)
        .expect("config")
        .replace("schema_version = 1", "schema_version = 0");
    fs::write(config_path, config).expect("older schema");
    let unsupported = json_failure(
        vault.path(),
        &["--root", "Todo", "validate"],
        7,
        "validation_failed",
    );
    assert!(unsupported["error"]["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue["code"] == "unsupported_schema"));
    assert_eq!(
        fs::read(vault.path().join("unrelated.md")).expect("unrelated note"),
        unrelated_before
    );
    assert_eq!(
        fs::read(vault.path().join(".obsidian/community-plugins.json")).expect("Obsidian config"),
        obsidian_before
    );
}

#[test]
fn every_cli_command_family_has_a_representative_failure_exit() {
    let vault = fake_vault();
    let unrelated_before = fs::read(vault.path().join("unrelated.md")).expect("unrelated note");
    let root = initialize(vault.path());

    json_failure(
        vault.path(),
        &["init", "Todo", "--vault-root", "."],
        5,
        "store_already_initialized",
    );
    json_failure(
        vault.path(),
        &["--root", "missing", "root"],
        3,
        "store_not_found",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "add", "Missing", "--project", "missing"],
        5,
        "project_not_found",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "list", "--state", "missing"],
        5,
        "unknown_state",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "show", "01A000"],
        3,
        "task_not_found",
    );

    json_success(
        vault.path(),
        &[
            "--root", "Todo", "project", "create", "work", "--name", "Work",
        ],
    );
    json_failure(
        vault.path(),
        &[
            "--root",
            "Todo",
            "project",
            "create",
            "work",
            "--name",
            "Duplicate",
        ],
        5,
        "project_already_exists",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "project", "show", "missing"],
        3,
        "project_not_found",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "project", "edit", "work"],
        2,
        "no_changes",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "project", "delete", "work"],
        2,
        "confirmation_required",
    );

    let open = json_success(vault.path(), &["--root", "Todo", "add", "Open"]);
    let open_id = task_id(&open).to_owned();
    let done = json_success(vault.path(), &["--root", "Todo", "add", "Done"]);
    let done_id = task_id(&done).to_owned();
    json_success(
        vault.path(),
        &["--root", "Todo", "complete", &done_id, "--on", "2026-09-02"],
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "edit", &open_id],
        2,
        "no_changes",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "finish-series", &open_id],
        5,
        "task_not_recurring",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "reopen", &open_id],
        5,
        "task_not_terminal",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "complete", &done_id],
        5,
        "task_already_terminal",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "cancel", &done_id],
        5,
        "task_already_terminal",
    );
    json_failure(
        vault.path(),
        &[
            "--root",
            "Todo",
            "delete",
            "01A00000000000000000000000",
            "--yes",
        ],
        3,
        "task_not_found",
    );

    fs::write(
        root.join("Tasks/01J3B0ZSBZZV25T1K0D3TA8JHR.md"),
        b"not front matter\n",
    )
    .expect("malformed task");
    json_failure(
        vault.path(),
        &["--root", "Todo", "project", "list"],
        5,
        "missing_frontmatter",
    );
    json_failure(
        vault.path(),
        &["--root", "Todo", "validate"],
        5,
        "validation_failed",
    );
    assert_eq!(
        fs::read(vault.path().join("unrelated.md")).expect("unrelated note"),
        unrelated_before
    );
}

#[test]
fn cli_detects_an_external_edit_between_read_and_atomic_replace() {
    const ID: &str = "01K4B0ZSBZZV25T1K0D3TA8JHR";
    const EXTERNAL: &[u8] =
        b"---\nname: External\nstate: blocked\nprojects: []\ntags: []\n---\nexternal\n";

    let vault = fake_vault();
    let root = initialize(vault.path());
    let target = root.join(format!("Tasks/{ID}.md"));
    let source = format!(
        "---\nname: Original\nstate: open\nprojects: []\ntags: []\n---\n{}",
        "large body\n".repeat(650_000)
    );
    assert!(source.len() < 8 * 1024 * 1024);
    fs::write(&target, source).expect("large task");

    let tasks_directory = root.join("Tasks");
    let watched_target = target.clone();
    let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
    let watcher = std::thread::spawn(move || {
        ready_sender.send(()).expect("watcher ready");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let temporary_exists = fs::read_dir(&tasks_directory)
                .expect("scan task directory")
                .any(|entry| {
                    entry
                        .expect("directory entry")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".tmp")
                });
            if temporary_exists {
                fs::write(&watched_target, EXTERNAL).expect("external edit");
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::yield_now();
        }
    });
    ready_receiver.recv().expect("watcher started");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_otodo"))
        .current_dir(vault.path())
        .args([
            "--root",
            "Todo",
            "edit",
            ID,
            "--state",
            "active",
            "--format=json",
        ])
        .output()
        .expect("run concurrent edit");
    assert!(watcher.join().expect("watcher completed"));
    assert_eq!(output.status.code(), Some(6));
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "concurrent_modification");
    assert_eq!(fs::read(&target).expect("external result"), EXTERNAL);
    assert!(fs::read_dir(root.join("Tasks"))
        .expect("task directory")
        .all(|entry| !entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")));
}

#[test]
fn discovery_precedence_and_direct_child_ambiguity_are_deterministic() {
    let parent = TempDir::new().expect("parent");
    for store in ["first", "second"] {
        command()
            .current_dir(parent.path())
            .args(["init", store, "--vault-root", "."])
            .assert()
            .success();
    }
    let first = parent.path().join("first");
    let second = parent.path().join("second");

    json_failure(parent.path(), &["root"], 4, "ambiguous_store");
    let explicit = json_success(
        parent.path(),
        &["--root", first.to_str().expect("path"), "root"],
    );
    assert_eq!(
        Path::new(explicit["root"].as_str().expect("root")),
        first.canonicalize().expect("canonical first")
    );

    let output = command()
        .current_dir(parent.path())
        .env("OBSIDIAN_TODO_ROOT", &second)
        .args(["root", "--format=json"])
        .output()
        .expect("environment root");
    assert!(output.status.success());
    let environment: Value = serde_json::from_slice(&output.stdout).expect("JSON");
    assert_eq!(
        Path::new(environment["root"].as_str().expect("root")),
        second.canonicalize().expect("canonical second")
    );
}

fn json_success_without_path(cwd: &Path, arguments: &[&str], stdin: Option<&str>) -> Value {
    let mut command = command();
    command
        .current_dir(cwd)
        .env("PATH", "/definitely/no/executables")
        .args(arguments)
        .arg("--format=json");
    if let Some(stdin) = stdin {
        command.write_stdin(stdin);
    }
    let output = command.output().expect("run sparse command");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

fn copy_directory(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("destination");
    for entry in fs::read_dir(source).expect("source directory") {
        let entry = entry.expect("entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}
