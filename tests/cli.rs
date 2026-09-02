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
    let value: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(value["version"], 1);
    assert_eq!(value["error"]["code"], code);
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
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "validate"], None);
}

#[test]
fn errors_have_stable_exit_codes_json_shape_and_leave_no_temporary_files() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let first_id = "01K4B0ZSBZZV25T1K0D3TA8JHR";
    let second_id = "01K4B0ZSBZZV25T1K0D3TA8JHS";
    let record =
        |name: &str| format!("---\nname: \"{name}\"\nstate: open\nprojects: []\ntags: []\n---\n");
    fs::write(root.join(format!("Tasks/{first_id}.md")), record("First")).expect("first");
    fs::write(root.join(format!("Tasks/{second_id}.md")), record("Second")).expect("second");

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

    fs::write(
        root.join(format!("Tasks/{first_id}.md")),
        b"---\nname: First\nstate: open\nprojects: []\ntags: []\n---\n<<<<<<< ours\nbody\n=======\nother\n>>>>>>> theirs\n",
    )
    .expect("conflict");
    let validation = json_failure(
        vault.path(),
        &["--root", "Todo", "validate"],
        5,
        "validation_failed",
    );
    assert!(validation["error"]["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue["code"] == "unresolved_conflict"));
    json_failure(
        vault.path(),
        &["--root", "Todo", "edit", first_id, "--state", "active"],
        6,
        "unresolved_conflict",
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
