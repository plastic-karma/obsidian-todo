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
fn human_success(cwd: &Path, arguments: &[&str]) -> String {
    let output = command()
        .current_dir(cwd)
        .args(arguments)
        .output()
        .expect("run human command");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(!output.stdout.is_empty());
    assert!(!output.stdout.contains(&0x1b));
    String::from_utf8(output.stdout).expect("UTF-8 human output")
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
    let todos_base = fs::read_to_string(root.join("todos.base")).expect("Obsidian base");
    assert_eq!(
        todos_base,
        r#"filters: "base == link(\"Todo/todos.base\")"
formulas:
  todo: "file.asLink(name)"
properties:
  formula.todo:
    displayName: Todo
  parent:
    displayName: Parent
  due_date:
    displayName: Due
  recurrence_from:
    displayName: Recurrence mode
  last_completed_date:
    displayName: Last completed
views:
  - type: table
    name: Todos
    order:
      - formula.todo
      - state
      - due_date
      - projects
      - tags
      - parent
      - recurrence
      - recurrence_from
      - last_completed_date
"#
    );
    assert!(!root.join(".git").exists());
    let config = fs::read_to_string(root.join(".todo/config.toml")).expect("configuration");
    assert!(config.contains("schema_version = 2"));
    assert_eq!(
        fs::read_to_string(root.join(".todo/schema.json")).expect("initialized schema"),
        obsidian_todo::config::embedded_schema(2).expect("v2 schema"),
    );
    assert!(config.contains("obsidian_link_prefix = \"Todo\""));

    let added = json_success(vault.path(), &["--root", "Todo", "add", "Minimal"]);
    assert_eq!(added["task"]["parent"], Value::Null);
    assert_eq!(added["task"]["url"], Value::Null);
    assert_eq!(added["task"]["due_date"], Value::Null);
    assert_eq!(added["task"]["recurrence"], Value::Null);
    assert_eq!(added["task"]["recurrence_from"], Value::Null);
    assert_eq!(added["task"]["last_completed_date"], Value::Null);
    assert_eq!(
        added["task"]["extra_properties"]["base"],
        "[[Todo/todos.base]]"
    );
    let id = task_id(&added);
    assert_eq!(id.len(), 26);
    let markdown = fs::read_to_string(root.join(format!("Tasks/{id}.md"))).expect("task file");
    assert_eq!(
        markdown,
        "---\nname: \"Minimal\"\nstate: open\nprojects: []\ntags: []\nbase: '[[Todo/todos.base]]'\n---\n"
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
fn url_cli_round_trip_preserves_lifecycle_and_refuses_invalid_mutations() {
    for legacy in [false, true] {
        let vault = fake_vault();
        let root = initialize(vault.path());
        if legacy {
            legacy_store(&root);
        }
        let config_before = fs::read(root.join(".todo/config.toml")).expect("config");
        let schema_before = fs::read(root.join(".todo/schema.json")).expect("schema");
        let url = "HTTPS://Example.COM/a%20b?q=One#Section";
        let added = json_success(
            &root,
            &[
                "add",
                "Linked recurrence",
                "--url",
                &format!("  {url}  "),
                "--due-date",
                "2026-09-08",
                "--recurrence",
                "FREQ=DAILY",
                "--recurrence-from",
                "schedule",
            ],
        );
        let id = task_id(&added);
        let path = root.join(added["task"]["path"].as_str().expect("path"));
        assert_eq!(added["task"]["url"], url);
        let source = fs::read_to_string(&path).expect("source");
        let source = source.replacen(
            "\n---\n",
            "\nplugin: {nested: [true, null, note]}\n---\nBody\r\n",
            1,
        );
        fs::write(&path, source).expect("external metadata");
        assert_eq!(json_success(&root, &["show", id])["task"]["url"], url);
        assert!(human_success(&root, &["show", id]).contains(&format!("url: {url}\n")));
        assert_eq!(json_success(&root, &["list"])["tasks"][0]["url"], url);
        let completed = json_success(&root, &["complete", id, "--on", "2026-09-08"]);
        assert_eq!(completed["task"]["due_date"], "2026-09-09");
        assert_eq!(completed["task"]["url"], url);
        for operation in ["finish-series", "reopen", "cancel", "reopen"] {
            assert_eq!(json_success(&root, &[operation, id])["task"]["url"], url);
        }
        let edited = json_success(&root, &["edit", id, "--state", "active"]);
        assert_eq!(edited["task"]["url"], url);
        assert_eq!(edited["task"]["body"], "Body\r\n");
        assert_eq!(
            edited["task"]["extra_properties"]["plugin"],
            serde_json::json!({"nested": [true, null, "note"]})
        );

        let before = store_bytes(&root);
        for invalid in [
            "javascript:alert(1)",
            "https:///path",
            "https://example.com/a b",
            "https://example.com/%GG",
        ] {
            for args in [
                vec!["add", "Invalid", "--url", invalid],
                vec!["edit", id, "--name", "Must not change", "--url", invalid],
            ] {
                let error = json_failure(&root, &args, 5, "invalid_url");
                assert_eq!(error["error"]["field"], "url");
                assert_eq!(store_bytes(&root), before);
            }
        }
        json_failure(
            &root,
            &["edit", id, "--url", url, "--clear-url"],
            2,
            "usage_error",
        );
        assert_eq!(store_bytes(&root), before);
        let replacement = "http://[::1]:8080/reference";
        assert_eq!(
            json_success(&root, &["edit", id, "--url", replacement])["task"]["url"],
            replacement
        );
        let cleared = json_success(&root, &["edit", id, "--clear-url"]);
        assert_eq!(cleared["task"]["url"], Value::Null);
        assert_eq!(cleared["task"]["body"], "Body\r\n");
        assert!(!fs::read_to_string(&path)
            .expect("cleared file")
            .contains("\nurl:"));
        json_success(&root, &["validate"]);
        assert_eq!(
            fs::read(root.join(".todo/config.toml")).expect("config"),
            config_before
        );
        assert_eq!(
            fs::read(root.join(".todo/schema.json")).expect("schema"),
            schema_before
        );

        let malformed = fs::read_to_string(&path).expect("source").replacen(
            "\n---\n",
            "\nurl: 'mailto:person@example.com'\n---\n",
            1,
        );
        fs::write(&path, malformed).expect("external malformed link");
        let before = store_bytes(&root);
        let report = json_failure(&root, &["validate"], 5, "validation_failed");
        assert!(report["error"]["issues"]
            .as_array()
            .expect("issues")
            .iter()
            .any(|issue| { issue["code"] == "invalid_url" && issue["field"] == "url" }));
        json_failure(&root, &["edit", id, "--state", "open"], 5, "invalid_url");
        assert_eq!(store_bytes(&root), before);
    }
}

#[test]
fn every_command_emits_human_success_only_on_stdout() {
    let vault = fake_vault();
    human_success(
        vault.path(),
        &["init", "Todo", "--vault-root", ".", "--color", "auto"],
    );
    human_success(vault.path(), &["--root", "Todo", "root"]);
    human_success(
        vault.path(),
        &[
            "--root", "Todo", "project", "create", "work", "--name", "Work",
        ],
    );
    human_success(vault.path(), &["--root", "Todo", "project", "list"]);
    human_success(vault.path(), &["--root", "Todo", "project", "show", "work"]);
    human_success(
        vault.path(),
        &[
            "--root", "Todo", "project", "edit", "work", "--name", "Office",
        ],
    );

    let added = human_success(vault.path(), &["--root", "Todo", "add", "Human task"]);
    let id = added
        .split_whitespace()
        .next()
        .expect("human add task ID")
        .to_owned();
    assert_eq!(id.len(), 26);
    human_success(vault.path(), &["--root", "Todo", "list"]);
    human_success(vault.path(), &["--root", "Todo", "show", &id]);
    human_success(
        vault.path(),
        &["--root", "Todo", "edit", &id, "--state", "active"],
    );
    human_success(
        vault.path(),
        &["--root", "Todo", "complete", &id, "--on", "2026-09-02"],
    );
    human_success(vault.path(), &["--root", "Todo", "reopen", &id]);
    human_success(vault.path(), &["--root", "Todo", "cancel", &id]);
    human_success(vault.path(), &["--root", "Todo", "reopen", &id]);
    human_success(vault.path(), &["--root", "Todo", "delete", &id, "--yes"]);

    let recurring = human_success(
        vault.path(),
        &[
            "--root",
            "Todo",
            "add",
            "Recurring",
            "--due-date",
            "2026-09-02",
            "--recurrence",
            "FREQ=DAILY",
            "--recurrence-from",
            "schedule",
        ],
    );
    let recurring_id = recurring
        .split_whitespace()
        .next()
        .expect("human recurring task ID")
        .to_owned();
    human_success(
        vault.path(),
        &["--root", "Todo", "finish-series", &recurring_id],
    );
    human_success(
        vault.path(),
        &["--root", "Todo", "project", "delete", "work", "--yes"],
    );
    human_success(vault.path(), &["--root", "Todo", "validate"]);
    let failure = command()
        .current_dir(vault.path())
        .args(["--root", "Todo", "show", "01A000"])
        .output()
        .expect("run human failure");
    assert_eq!(failure.status.code(), Some(3));
    assert!(failure.stdout.is_empty());
    assert!(!failure.stderr.is_empty());
    assert!(!failure.stderr.contains(&0x1b));
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
    let unrelated = sparse.path().join("unrelated.md");
    fs::write(&unrelated, b"outside the copied store\r\n").expect("unrelated sparse note");
    let unrelated_before = fs::read(&unrelated).expect("unrelated sparse note");
    let root = sparse.path().join("OnlyTodo");
    copy_directory(&original, &root);
    assert!(!sparse.path().join(".git").exists());
    assert!(!sparse.path().join(".obsidian").exists());
    let dry_run = json_success_without_path(
        sparse.path(),
        &["init", "DryTodo", "--vault-root", ".", "--dry-run"],
        None,
    );
    assert_eq!(dry_run["dry_run"], true);
    assert!(!sparse.path().join("DryTodo").exists());
    let resolved = json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "root"], None);
    assert!(resolved["root"]
        .as_str()
        .is_some_and(|path| path.ends_with("/OnlyTodo")));

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
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "reopen", &id], None);
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "cancel", &id], None);
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "reopen", &id], None);
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
    let recurring = json_success_without_path(
        sparse.path(),
        &[
            "--root",
            "OnlyTodo",
            "add",
            "Recurring",
            "--due-date",
            "2026-09-01",
            "--recurrence",
            "FREQ=DAILY",
            "--recurrence-from",
            "schedule",
        ],
        None,
    );
    let recurring_id = task_id(&recurring).to_owned();
    json_success_without_path(
        sparse.path(),
        &["--root", "OnlyTodo", "finish-series", &recurring_id],
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
    let hidden_record = root.join("Tasks/.editor.md");
    fs::write(
        &hidden_record,
        b"---\nname: Hidden\nstate: open\nprojects: []\ntags: []\n---\n",
    )
    .expect("hidden editor record");
    let warning_output = command()
        .current_dir(sparse.path())
        .env("PATH", "/definitely/no/executables")
        .args(["--root", "OnlyTodo", "validate"])
        .output()
        .expect("human validation");
    assert!(warning_output.status.success());
    assert!(warning_output.stderr.is_empty());
    let warning_stdout = String::from_utf8(warning_output.stdout).expect("UTF-8 warning output");
    assert!(warning_stdout.contains("warning[unexpected_file]"));
    assert!(warning_stdout.contains("Tasks/.editor.md"));
    fs::remove_file(hidden_record).expect("remove hidden editor record");
    json_success_without_path(sparse.path(), &["--root", "OnlyTodo", "validate"], None);
    assert_eq!(
        fs::read(unrelated).expect("unrelated sparse note"),
        unrelated_before
    );
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
        3,
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
    assert_eq!(validation["error"]["validation"]["valid"], false);
    assert!(validation["error"]["validation"]["errors"]
        .as_u64()
        .is_some_and(|count| count >= 2));
    assert!(validation["error"]["validation"]["warnings"].is_number());
    assert!(validation["error"]["validation"]["tasks"].is_number());
    assert!(validation["error"]["validation"]["projects"].is_number());
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
        .replace("schema_version = 2", "schema_version = 0");
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
    fs::create_dir(vault.path().join("Broken")).expect("broken store candidate");
    let broken = json_failure(
        vault.path(),
        &["--root", "Broken", "validate"],
        5,
        "validation_failed",
    );
    assert!(broken["error"]["issues"]
        .as_array()
        .expect("validation issues")
        .iter()
        .any(|issue| issue["code"] == "metadata_directory_missing"));
    json_failure(
        vault.path(),
        &["--root", "Todo", "add", "Missing", "--project", "missing"],
        3,
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
    assert!(!output.stdout.contains(&0x1b));
    let value: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(value["version"], 1);
    value
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

fn legacy_store(root: &Path) {
    let config_path = root.join(".todo/config.toml");
    let config = fs::read_to_string(&config_path).expect("configuration");
    fs::write(
        config_path,
        config.replace("schema_version = 2", "schema_version = 1"),
    )
    .expect("legacy configuration");
    fs::write(
        root.join(".todo/schema.json"),
        obsidian_todo::config::embedded_schema(1).expect("historical schema"),
    )
    .expect("legacy schema");
}

fn store_bytes(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn collect(
        root: &Path,
        directory: &Path,
        files: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) {
        for entry in fs::read_dir(directory).expect("store directory") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root)
                        .expect("contained path")
                        .to_path_buf(),
                    fs::read(path).expect("store file"),
                );
            }
        }
    }
    let mut files = std::collections::BTreeMap::new();
    collect(root, root, &mut files);
    files
}

#[test]
fn capabilities_bypasses_invalid_explicit_and_environment_roots() {
    let directory = TempDir::new().expect("rootless directory");
    fs::create_dir(directory.path().join(".todo")).expect("invalid store marker");
    fs::write(directory.path().join(".todo/config.toml"), "not valid TOML")
        .expect("invalid configuration");
    for arguments in [
        vec!["--format=json", "capabilities"],
        vec!["--root", "missing", "capabilities", "--format=json"],
        vec!["capabilities", "--root", ".", "--format=json"],
    ] {
        let output = command()
            .current_dir(directory.path())
            .env("OBSIDIAN_TODO_ROOT", directory.path().join("also-missing"))
            .args(arguments)
            .output()
            .expect("rootless capabilities");
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).expect("capabilities JSON"),
            serde_json::json!({
                "version": 1,
                "store_schema_versions": [1, 2],
                "features": ["subtasks", "task_candidates", "store_upgrade", "attachments", "task_urls"],
            })
        );
    }
}

#[test]
fn parent_lifecycle_is_independent_and_deletion_requires_explicit_detach() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let parent = json_success(
        &root,
        &["add", "Parent", "--state", "done", "--tag", "private"],
    );
    let parent_id = task_id(&parent);
    let parent_path = root.join(parent["task"]["path"].as_str().expect("parent path"));
    let parent_bytes = fs::read(&parent_path).expect("parent bytes");
    let child = json_success(
        &root,
        &["add", "Child", "--parent", &parent_id.to_ascii_lowercase()],
    );
    let child_id = task_id(&child);
    assert_eq!(child["task"]["parent"], parent_id);
    assert_eq!(child["task"]["state"], "open");
    assert_eq!(child["task"]["tags"], serde_json::json!([]));
    assert_eq!(child["task"]["due_date"], Value::Null);
    assert_eq!(
        json_success(&root, &["list"])["tasks"],
        serde_json::json!([child["task"]])
    );
    let grandchild = json_success(&root, &["add", "Grandchild", "--parent", child_id]);
    let grandchild_id = task_id(&grandchild);
    assert_eq!(
        json_success(&root, &["list", "--parent", parent_id])["tasks"],
        serde_json::json!([child["task"]])
    );
    let grandchild_path = root.join(grandchild["task"]["path"].as_str().expect("path"));
    let grandchild_bytes = fs::read(&grandchild_path).expect("grandchild");
    for operation in ["complete", "reopen", "cancel", "reopen"] {
        let changed = json_success(&root, &[operation, child_id]);
        assert_eq!(changed["task"]["parent"], parent_id);
        assert_eq!(changed["task"]["path"], child["task"]["path"]);
        assert_eq!(fs::read(&parent_path).expect("parent"), parent_bytes);
        assert_eq!(
            fs::read(&grandchild_path).expect("grandchild"),
            grandchild_bytes
        );
    }
    let before_cycle = store_bytes(&root);
    json_failure(
        &root,
        &["edit", parent_id, "--parent", grandchild_id],
        5,
        "parent_cycle",
    );
    assert_eq!(store_bytes(&root), before_cycle);
    json_success(&root, &["complete", child_id]);
    let before_delete = store_bytes(&root);
    let refusal = json_failure(&root, &["delete", parent_id, "--yes"], 5, "task_in_use");
    assert!(refusal["error"]["message"]
        .as_str()
        .expect("message")
        .contains(child_id));
    assert_eq!(store_bytes(&root), before_delete);
    let detached = json_success(&root, &["edit", child_id, "--clear-parent"]);
    assert_eq!(detached["task"]["parent"], Value::Null);
    assert_eq!(detached["task"]["path"], child["task"]["path"]);
    json_success(&root, &["delete", parent_id, "--yes"]);
    assert!(!parent_path.exists());
    let roots = json_success(&root, &["list", "--all", "--roots"]);
    assert_eq!(roots["tasks"], serde_json::json!([detached["task"]]));
    assert_eq!(
        fs::read(&grandchild_path).expect("grandchild"),
        grandchild_bytes
    );
}

#[test]
fn recurring_parent_and_child_advance_only_the_selected_series() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let parent = json_success(
        &root,
        &[
            "add",
            "Schedule parent",
            "--due-date",
            "2026-09-01",
            "--recurrence",
            "FREQ=DAILY",
            "--recurrence-from",
            "schedule",
        ],
    );
    let child = json_success(
        &root,
        &[
            "add",
            "Completion child",
            "--parent",
            task_id(&parent),
            "--due-date",
            "2026-09-01",
            "--recurrence",
            "FREQ=DAILY",
            "--recurrence-from",
            "completion",
        ],
    );
    let parent_path = root.join(parent["task"]["path"].as_str().expect("path"));
    let child_path = root.join(child["task"]["path"].as_str().expect("path"));
    let parent_bytes = fs::read(&parent_path).expect("parent");
    let completed = json_success(&root, &["complete", task_id(&child), "--on", "2026-09-06"]);
    assert_eq!(completed["task"]["due_date"], "2026-09-07");
    assert_eq!(completed["task"]["parent"], task_id(&parent));
    assert_eq!(fs::read(&parent_path).expect("parent"), parent_bytes);
    let child_bytes = fs::read(&child_path).expect("child");
    let completed = json_success(&root, &["complete", task_id(&parent), "--on", "2026-09-06"]);
    assert_eq!(completed["task"]["due_date"], "2026-09-07");
    assert_eq!(fs::read(&child_path).expect("child"), child_bytes);
    let finished = json_success(&root, &["finish-series", task_id(&child)]);
    assert_eq!(finished["task"]["parent"], task_id(&parent));
    assert_eq!(finished["task"]["state"], "done");
}

#[test]
fn compact_candidates_preserve_sort_literal_query_and_exact_truncation() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let first = json_success(&root, &["add", "Needle [.*] B", "--due-date", "2026-09-01"]);
    let second = json_success(
        &root,
        &[
            "add",
            "Needle [.*] A",
            "--parent",
            task_id(&first),
            "--body",
            "not compact",
        ],
    );
    json_success(&root, &["add", "Unmatched", "--state", "done"]);
    let compact_first = serde_json::json!({
        "id": task_id(&first), "path": first["task"]["path"], "name": "Needle [.*] B",
        "state": "open", "terminal": false, "parent": null,
    });
    let compact_second = serde_json::json!({
        "id": task_id(&second), "path": second["task"]["path"], "name": "Needle [.*] A",
        "state": "open", "terminal": false, "parent": task_id(&first),
    });
    assert_eq!(
        json_success(
            &root,
            &["list", "--summary", "--query", "[.*]", "--limit", "1"]
        ),
        serde_json::json!({"version": 1, "tasks": [compact_first], "has_more": true})
    );
    for extra in [vec![], vec!["--limit", "2"], vec!["--limit", "1000"]] {
        let mut args = vec!["list", "--summary", "--query", "nEeDlE [.*]"];
        args.extend(extra);
        assert_eq!(
            json_success(&root, &args),
            serde_json::json!({
                "version": 1, "tasks": [compact_first, compact_second], "has_more": false,
            })
        );
    }
    assert_eq!(
        json_success(&root, &["list", "--query", "NEEDLE"]),
        serde_json::json!({"version": 1, "tasks": [first["task"], second["task"]]})
    );
    assert_eq!(
        json_success(
            &root,
            &["list", "--query", &task_id(&second).to_ascii_lowercase()]
        )["tasks"],
        serde_json::json!([second["task"]])
    );
    assert_eq!(
        json_success(&root, &["list", "--summary", "--query", "", "--limit", "2"])["has_more"],
        false
    );
    assert_eq!(
        json_success(
            &root,
            &["list", "--summary", "--query", "no match", "--limit", "1"]
        ),
        serde_json::json!({"version": 1, "tasks": [], "has_more": false})
    );
}

#[test]
fn parent_arguments_and_candidate_constraints_have_stable_errors_without_writes() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let task = json_success(&root, &["add", "Selected"]);
    let id = task_id(&task);
    let before = store_bytes(&root);
    for arguments in [
        vec!["add", "Child", "--parent", &id[..8]],
        vec!["edit", id, "--parent", "Tasks/not-an-id.md"],
        vec!["list", "--parent", ""],
    ] {
        let error = json_failure(&root, &arguments, 5, "invalid_parent_id");
        assert_eq!(error["error"]["field"], "parent");
    }
    for arguments in [
        vec!["list", "--parent", id, "--roots"],
        vec!["edit", id, "--parent", id, "--clear-parent"],
        vec!["list", "--limit", "1"],
        vec!["list", "--summary", "--limit", "0"],
        vec!["list", "--summary", "--limit", "1001"],
        vec!["list", "--summary", "--limit", "1.5"],
    ] {
        json_failure(&root, &arguments, 2, "usage_error");
    }
    const MISSING: &str = "00000000000000000000000001";
    for arguments in [
        vec!["add", "Child", "--parent", MISSING],
        vec!["edit", id, "--parent", MISSING],
        vec!["list", "--parent", MISSING],
    ] {
        json_failure(&root, &arguments, 3, "task_not_found");
    }
    json_failure(
        &root,
        &["edit", id, "--parent", id],
        5,
        "self_parent_reference",
    );
    assert_eq!(store_bytes(&root), before);
}

#[test]
fn graph_faults_cannot_be_hidden_by_query_terminal_roots_or_limit() {
    const MISSING: &str = "00000000000000000000000001";
    let vault = fake_vault();
    let root = initialize(vault.path());
    let visible = json_success(&root, &["add", "Visible", "--due-date", "2026-09-01"]);
    let broken = json_success(&root, &["add", "Hidden", "--state", "done"]);
    let path = root.join(broken["task"]["path"].as_str().expect("path"));
    let source = fs::read_to_string(&path).expect("record");
    fs::write(
        &path,
        source.replace("tags: []", &format!("tags: []\nparent: \"{MISSING}\"")),
    )
    .expect("missing stored edge");
    assert_eq!(
        json_success(&root, &["show", task_id(&broken)])["task"]["parent"],
        MISSING
    );
    for arguments in [
        vec!["list"],
        vec![
            "list",
            "--roots",
            "--query",
            "Visible",
            "--summary",
            "--limit",
            "1",
        ],
        vec!["list", "--query", "no matches", "--summary", "--limit", "1"],
        vec![
            "list",
            "--parent",
            task_id(&visible),
            "--summary",
            "--limit",
            "1",
        ],
    ] {
        json_failure(&root, &arguments, 5, "missing_parent_reference");
    }
    let repaired = json_success(&root, &["edit", task_id(&broken), "--clear-parent"]);
    assert_eq!(repaired["task"]["parent"], Value::Null);
    assert_eq!(
        json_success(&root, &["list", "--summary", "--limit", "1"])["has_more"],
        false
    );
}

#[test]
fn project_deletion_isolated_from_orphans_and_cycles_preserves_reference_safety() {
    const FIRST: &str = "01J3B0ZSBZZV25T1K0D3TA8JHR";
    const SECOND: &str = "01J3B0ZSBZZV25T1K0D3TA8JHS";
    const MISSING: &str = "00000000000000000000000001";

    for code in ["missing_parent_reference", "parent_cycle"] {
        let vault = fake_vault();
        let root = initialize(vault.path());
        for slug in ["unused", "work"] {
            json_success(&root, &["project", "create", slug, "--name", slug]);
        }
        let parent = if code == "parent_cycle" {
            SECOND
        } else {
            MISSING
        };
        fs::write(
            root.join(format!("Tasks/{FIRST}.md")),
            format!(
                "---\nname: Unassigned\nstate: open\nprojects: []\ntags: []\nparent: \"{parent}\"\n---\n"
            ),
        )
        .expect("typed faulty parent");
        let second_parent = if code == "parent_cycle" {
            format!("parent: \"{FIRST}\"\n")
        } else {
            String::new()
        };
        fs::write(
            root.join(format!("Tasks/{SECOND}.md")),
            format!(
                "---\nname: Referencing\nstate: done\nprojects: [\"[[Todo/Projects/work]]\"]\ntags: []\n{second_parent}---\n"
            ),
        )
        .expect("referencing task");

        let mut expected = store_bytes(&root);
        json_success(&root, &["project", "delete", "unused", "--yes"]);
        expected.remove(Path::new("Projects/unused.md"));
        assert_eq!(store_bytes(&root), expected);

        let failure = json_failure(
            &root,
            &["project", "delete", "work", "--yes"],
            5,
            "project_in_use",
        );
        assert!(failure["error"]["message"]
            .as_str()
            .expect("message")
            .contains(SECOND));
        for arguments in [
            vec!["list", "--all"],
            vec!["list", "--summary", "--query", "no matches", "--limit", "1"],
        ] {
            json_failure(&root, &arguments, 5, code);
        }
        assert_eq!(store_bytes(&root), expected);
    }
}

#[test]
fn project_deletion_still_rejects_malformed_records_and_duplicate_identities() {
    const ID: &str = "01J3B0ZSBZZV25T1K0D3TA8JHR";
    const VALID: &str = "---\nname: Unassigned\nstate: open\nprojects: []\ntags: []\n---\n";

    let vault = fake_vault();
    let root = initialize(vault.path());
    json_success(&root, &["project", "create", "unused", "--name", "Unused"]);
    let path = root.join(format!("Tasks/{ID}.md"));
    for (source, code) in [
        ("not front matter\n", "missing_frontmatter"),
        (
            "---\nname: Unassigned\nstate: open\nprojects: []\ntags: []\nparent: null\n---\n",
            "invalid_parent_id",
        ),
    ] {
        fs::write(&path, source).expect("malformed task");
        let before = store_bytes(&root);
        json_failure(&root, &["project", "delete", "unused", "--yes"], 5, code);
        assert_eq!(store_bytes(&root), before);
    }

    fs::write(&path, VALID).expect("valid task");
    let nested = root.join("Tasks/nested");
    fs::create_dir(&nested).expect("nested tasks");
    fs::write(
        nested.join(format!("{}.md", ID.to_ascii_lowercase())),
        VALID,
    )
    .expect("duplicate identity");
    let before = store_bytes(&root);
    json_failure(
        &root,
        &["project", "delete", "unused", "--yes"],
        5,
        "duplicate_task_id",
    );
    assert_eq!(store_bytes(&root), before);
}

#[test]
fn legacy_parent_extras_remain_flat_and_relation_flags_are_gated() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    legacy_store(&root);
    let task = json_success(&root, &["add", "Legacy"]);
    let path = root.join(task["task"]["path"].as_str().expect("path"));
    let source = fs::read_to_string(&path).expect("task");
    fs::write(
        &path,
        source.replace("tags: []", "tags: []\nparent: {plugin: [null, 17, old]}"),
    )
    .expect("legacy metadata");
    let edited = json_success(&root, &["edit", task_id(&task), "--name", "Preserved"]);
    assert_eq!(edited["task"]["parent"], Value::Null);
    assert_eq!(
        edited["task"]["extra_properties"]["parent"],
        serde_json::json!({"plugin": [null, 17, "old"]})
    );
    assert_eq!(
        json_success(
            &root,
            &["list", "--summary", "--query", "preserved", "--limit", "1"]
        ),
        serde_json::json!({
            "version": 1, "tasks": [{
                "id": task_id(&task), "path": task["task"]["path"], "name": "Preserved",
                "state": "open", "terminal": false, "parent": null,
            }], "has_more": false,
        })
    );
    let before = store_bytes(&root);
    for arguments in [
        vec!["add", "Child", "--parent", task_id(&task)],
        vec!["edit", task_id(&task), "--parent", task_id(&task)],
        vec!["edit", task_id(&task), "--clear-parent"],
        vec!["list", "--parent", task_id(&task)],
        vec!["list", "--roots"],
    ] {
        json_failure(&root, &arguments, 7, "unsupported_schema");
    }
    assert_eq!(store_bytes(&root), before);
    assert_eq!(
        fs::read_to_string(root.join(".todo/schema.json")).expect("schema"),
        obsidian_todo::config::embedded_schema(1).expect("legacy schema")
    );
}

#[test]
fn upgrade_dry_run_cutover_and_noop_preserve_every_record_and_custom_base() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    legacy_store(&root);
    json_success(&root, &["project", "create", "work", "--name", "Work"]);
    json_success(
        &root,
        &[
            "add",
            "Legacy",
            "--project",
            "work",
            "--body",
            "Preserve me\n[Receipt](../Attachments/manual.pdf)",
        ],
    );
    fs::create_dir(root.join("Attachments")).expect("attachments");
    fs::write(root.join("Attachments/manual.pdf"), [0, 255, 1, 13, 10]).expect("attachment bytes");
    fs::write(root.join("todos.base"), b"# custom base\r\nviews: []\n").expect("custom Base");
    let config_path = root.join(".todo/config.toml");
    let config = fs::read_to_string(&config_path).expect("config");
    fs::write(&config_path, format!("# keep this comment\n{config}")).expect("comment");
    let before = store_bytes(&root);
    assert_eq!(
        json_success(&root, &["upgrade", "--to", "2", "--dry-run"]),
        serde_json::json!({
            "version": 1,
            "upgrade": {"from": 1, "to": 2, "dry_run": true, "status": "planned"},
        })
    );
    assert_eq!(store_bytes(&root), before);
    assert_eq!(
        json_success(&root, &["upgrade", "--to", "2"]),
        serde_json::json!({
            "version": 1,
            "upgrade": {"from": 1, "to": 2, "dry_run": false, "status": "upgraded"},
        })
    );
    let mut expected = before;
    let config =
        String::from_utf8(expected[Path::new(".todo/config.toml")].clone()).expect("UTF-8");
    expected.insert(
        PathBuf::from(".todo/config.toml"),
        config
            .replace("schema_version = 1", "schema_version = 2")
            .into_bytes(),
    );
    expected.insert(
        PathBuf::from(".todo/schema.json"),
        obsidian_todo::config::embedded_schema(2)
            .expect("v2 schema")
            .as_bytes()
            .to_vec(),
    );
    assert_eq!(store_bytes(&root), expected);
    for dry_run in [false, true] {
        let mut arguments = vec!["upgrade", "--to", "2"];
        if dry_run {
            arguments.push("--dry-run");
        }
        assert_eq!(
            json_success(&root, &arguments),
            serde_json::json!({
                "version": 1,
                "upgrade": {"from": 2, "to": 2, "dry_run": dry_run, "status": "already_current"},
            })
        );
        assert_eq!(store_bytes(&root), expected);
    }
    json_failure(&root, &["upgrade", "--to", "1"], 7, "unsupported_schema");
    json_failure(&root, &["upgrade", "--to", "3"], 7, "unsupported_schema");
    assert_eq!(store_bytes(&root), expected);
}

#[test]
fn upgrade_intermediate_pair_fails_closed_and_requires_explicit_resume() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    legacy_store(&root);
    let task = json_success(
        &root,
        &[
            "add",
            "Unchanged",
            "--body",
            "[Receipt](../Attachments/manual.pdf)",
        ],
    );
    fs::create_dir(root.join("Attachments")).expect("attachments");
    fs::write(root.join("Attachments/manual.pdf"), [0, 255, 13, 10]).expect("binary");
    fs::write(
        root.join(".todo/schema.json"),
        obsidian_todo::config::embedded_schema(2).expect("v2 schema"),
    )
    .expect("interrupted schema-first publication");
    let before = store_bytes(&root);
    for arguments in [
        vec!["list"],
        vec!["edit", task_id(&task), "--name", "Not written"],
    ] {
        json_failure(&root, &arguments, 7, "schema_version_mismatch");
    }
    json_failure(&root, &["validate"], 7, "validation_failed");
    assert_eq!(store_bytes(&root), before);
    assert_eq!(
        json_success(&root, &["upgrade", "--to", "2", "--dry-run"])["upgrade"]["status"],
        "planned"
    );
    assert_eq!(store_bytes(&root), before);
    assert_eq!(
        json_success(&root, &["upgrade", "--to", "2"]),
        serde_json::json!({
            "version": 1,
            "upgrade": {"from": 1, "to": 2, "dry_run": false, "status": "resumed"},
        })
    );
    let after = store_bytes(&root);
    for (path, bytes) in before {
        if path != Path::new(".todo/config.toml") {
            assert_eq!(after[&path], bytes, "changed {}", path.display());
        }
    }
    assert_eq!(
        json_success(&root, &["show", task_id(&task)])["task"],
        task["task"]
    );
}

#[test]
fn parent_identity_survives_custom_paths_sparse_layout_and_manual_moves() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let config_path = root.join(".todo/config.toml");
    let config = fs::read_to_string(&config_path)
        .expect("config")
        .replace(
            "tasks_directory = \"Tasks\"",
            "tasks_directory = \"Notes/Tasks\"",
        )
        .replace(
            "projects_directory = \"Projects\"",
            "projects_directory = \"Notes/Projects\"",
        );
    fs::write(config_path, config).expect("custom paths");
    fs::create_dir(root.join("Notes")).expect("notes");
    fs::rename(root.join("Tasks"), root.join("Notes/Tasks")).expect("move tasks");
    fs::rename(root.join("Projects"), root.join("Notes/Projects")).expect("move projects");
    fs::remove_dir_all(vault.path().join(".obsidian")).expect("sparse vault");
    legacy_store(&root);
    let parent = json_success(&root, &["add", "Moved parent"]);
    let original_path = root.join(parent["task"]["path"].as_str().expect("path"));
    let nested = root.join("Notes/Tasks/Archive/deeper");
    fs::create_dir_all(&nested).expect("nested tree");
    let moved_path = nested.join(format!("{}.md", task_id(&parent).to_ascii_lowercase()));
    fs::rename(original_path, &moved_path).expect("manual move");
    let bytes = fs::read(&moved_path).expect("moved parent");
    let before_upgrade = store_bytes(&root);
    assert_eq!(
        json_success(&root, &["upgrade", "--to", "2"])["upgrade"]["status"],
        "upgraded"
    );
    for (path, bytes) in before_upgrade {
        if !path.starts_with(".todo") {
            assert_eq!(
                fs::read(root.join(path)).expect("preserved sparse file"),
                bytes
            );
        }
    }
    let child = json_success(&root, &["add", "Child", "--parent", task_id(&parent)]);
    assert_eq!(child["task"]["parent"], task_id(&parent));
    assert_eq!(
        child["task"]["path"],
        format!("Notes/Tasks/{}.md", task_id(&child))
    );
    let summary = json_success(&root, &["list", "--summary", "--query", task_id(&parent)]);
    assert_eq!(
        summary["tasks"][0]["path"],
        format!(
            "Notes/Tasks/Archive/deeper/{}.md",
            task_id(&parent).to_ascii_lowercase()
        )
    );
    let detached = json_success(&root, &["edit", task_id(&child), "--clear-parent"]);
    assert_eq!(detached["task"]["path"], child["task"]["path"]);
    assert_eq!(fs::read(moved_path).expect("parent"), bytes);
}

#[test]
fn upgrade_rejects_every_legacy_parent_key_on_start_and_resume_without_writes() {
    for parent in [
        "null",
        "\"00000000000000000000000001\"",
        "[old, metadata]",
        "{plugin: value}",
    ] {
        for resume in [false, true] {
            let vault = fake_vault();
            let root = initialize(vault.path());
            legacy_store(&root);
            let task = json_success(&root, &["add", "Collision"]);
            let relative = Path::new(task["task"]["path"].as_str().expect("path"));
            let path = root.join(relative);
            let source = fs::read_to_string(&path).expect("task");
            fs::write(
                &path,
                source.replace("tags: []", &format!("tags: []\nparent: {parent}")),
            )
            .expect("legacy parent key");
            if resume {
                fs::write(
                    root.join(".todo/schema.json"),
                    obsidian_todo::config::embedded_schema(2).expect("schema"),
                )
                .expect("intermediate pair");
            }
            let before = store_bytes(&root);
            for dry_run in [false, true] {
                let mut arguments = vec!["upgrade", "--to", "2"];
                if dry_run {
                    arguments.push("--dry-run");
                }
                let error = json_failure(&root, &arguments, 5, "validation_failed");
                assert!(error["error"]["issues"]
                    .as_array()
                    .expect("issues")
                    .iter()
                    .any(|issue| {
                        issue["field"] == "parent"
                            && issue["path"] == relative.to_str().expect("UTF-8 path")
                    }));
                assert_eq!(store_bytes(&root), before);
            }
        }
    }
}

#[test]
fn upgrade_rejects_reversed_and_customized_pairs_without_overwriting() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    fs::write(
        root.join(".todo/schema.json"),
        obsidian_todo::config::embedded_schema(1).expect("v1 schema"),
    )
    .expect("reversed pair");
    let before = store_bytes(&root);
    let output = command()
        .current_dir(&root)
        .args(["upgrade", "--to", "2", "--format=json"])
        .output()
        .expect("reversed upgrade");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured error");
    assert_eq!(error["version"], 1);
    assert!(error["error"]["code"].is_string());
    assert_eq!(store_bytes(&root), before);
    legacy_store(&root);
    let mut schema: Value =
        serde_json::from_str(obsidian_todo::config::embedded_schema(1).expect("schema"))
            .expect("schema JSON");
    schema["properties"]["injected"] = serde_json::json!({"type": "string"});
    fs::write(
        root.join(".todo/schema.json"),
        serde_json::to_vec(&schema).expect("JSON"),
    )
    .expect("customized schema");
    let before = store_bytes(&root);
    let output = command()
        .current_dir(&root)
        .args(["upgrade", "--to", "2", "--dry-run", "--format=json"])
        .output()
        .expect("customized upgrade");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("structured error");
    assert_eq!(error["version"], 1);
    assert!(error["error"]["code"].is_string());
    assert_eq!(store_bytes(&root), before);
}

#[test]
fn unsupported_versions_precede_shape_errors_and_never_mutate() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    fs::write(
        root.join(".todo/config.toml"),
        "schema_version = 3\nunknown = true\n",
    )
    .expect("future config");
    let before = store_bytes(&root);
    json_failure(&root, &["add", "Never written"], 7, "unsupported_schema");
    json_failure(&root, &["upgrade", "--to", "2"], 7, "unsupported_schema");
    assert_eq!(store_bytes(&root), before);
}

#[test]
fn reparent_by_selected_prefix_preserves_body_extras_and_actual_child_path() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    const CHILD: &str = "00000000000000000000000001";
    let first = json_success(&root, &["add", "First"]);
    let second = json_success(&root, &["add", "Second", "--parent", task_id(&first)]);
    let directory = root.join("Tasks/Manual");
    fs::create_dir(&directory).expect("manual directory");
    let path = directory.join(format!("{CHILD}.md"));
    fs::write(
        &path,
        format!(
            "---\nname: Child\nstate: done\nprojects: []\ntags: [own]\nparent: \"{}\"\nplugin: {{items: [1, null]}}\n---\nraw\r\nbody  ",
            task_id(&first)
        ),
    )
    .expect("manual child");
    let first_path = root.join(first["task"]["path"].as_str().expect("path"));
    let second_path = root.join(second["task"]["path"].as_str().expect("path"));
    let first_bytes = fs::read(&first_path).expect("first");
    let second_bytes = fs::read(&second_path).expect("second");
    let moved = json_success(
        &root,
        &[
            "edit",
            &CHILD[..8],
            "--parent",
            &task_id(&second).to_ascii_lowercase(),
        ],
    );
    assert_eq!(moved["task"]["id"], CHILD);
    assert_eq!(moved["task"]["parent"], task_id(&second));
    assert_eq!(moved["task"]["path"], format!("Tasks/Manual/{CHILD}.md"));
    assert_eq!(moved["task"]["state"], "done");
    assert_eq!(moved["task"]["tags"], serde_json::json!(["own"]));
    assert_eq!(moved["task"]["body"], "raw\r\nbody  ");
    assert_eq!(
        moved["task"]["extra_properties"]["plugin"],
        serde_json::json!({"items": [1, null]})
    );
    assert!(fs::read_to_string(&path)
        .expect("child")
        .contains(&format!("parent: \"{}\"\n", task_id(&second))));
    assert_eq!(fs::read(first_path).expect("first"), first_bytes);
    assert_eq!(fs::read(second_path).expect("second"), second_bytes);
}

#[test]
fn attachments_import_binary_and_link_manually_placed_files_in_both_store_versions() {
    for version in [1, 2] {
        let vault = fake_vault();
        let root = initialize(vault.path());
        if version == 1 {
            legacy_store(&root);
        }
        let config = fs::read(root.join(".todo/config.toml")).expect("config");
        let schema = fs::read(root.join(".todo/schema.json")).expect("schema");
        let bytes = [0, 255, b'\n', 0, 1, 2, 3];
        let source = vault.path().join("café [photo].png");
        let receipt = vault.path().join("receipt.pdf");
        fs::write(&source, bytes).expect("source");
        fs::write(&receipt, b"receipt\r\n").expect("receipt");
        let task = json_success(
            &root,
            &[
                "add",
                "Expenses",
                "--body",
                "Keep this body.",
                "--attach",
                source.to_str().expect("path"),
                "--attach",
                receipt.to_str().expect("path"),
            ],
        );
        assert_eq!(task["version"], 1);
        let id = task_id(&task);
        let listed = json_success(&root, &["attachment", "list", id]);
        let attachments = listed["attachments"].as_array().expect("attachments");
        assert_eq!(attachments.len(), 2);
        let image_path = attachments[0]["path"].as_str().expect("image path");
        assert_eq!(attachments[0]["display_name"], "café [photo].png");
        assert_eq!(attachments[0]["byte_size"], bytes.len());
        assert_eq!(attachments[0]["availability"], "available");
        assert_eq!(fs::read(root.join(image_path)).expect("import"), bytes);
        assert!(task["task"]["body"]
            .as_str()
            .expect("body")
            .contains("![café \\[photo\\].png](../Attachments/"));
        assert!(task["task"]["body"]
            .as_str()
            .expect("body")
            .contains("caf%C3%A9%20%5Bphoto%5D.png"));
        let resolved = json_success(&root, &["attachment", "path", id, image_path]);
        assert_eq!(
            resolved["path"],
            root.join(image_path).to_str().expect("absolute")
        );
        fs::write(root.join("Attachments/manual.pdf"), b"manual").expect("manual");
        json_success(&root, &["attachment", "link", id, "Attachments/manual.pdf"]);
        json_success(&root, &["attachment", "link", id, "Attachments/manual.pdf"]);
        assert_eq!(
            json_success(&root, &["attachment", "list", id])["attachments"]
                .as_array()
                .expect("list")
                .len(),
            3
        );
        json_success(&root, &["attachment", "unlink", id, image_path]);
        assert_eq!(fs::read(root.join(image_path)).expect("retained"), bytes);
        json_success(&root, &["complete", id]);
        json_success(&root, &["delete", id, "--yes"]);
        assert_eq!(
            fs::read(root.join(image_path)).expect("retained after deletion"),
            bytes
        );
        assert_eq!(
            fs::read(root.join(".todo/config.toml")).expect("config"),
            config
        );
        assert_eq!(
            fs::read(root.join(".todo/schema.json")).expect("schema"),
            schema
        );
    }
}

#[test]
fn attachment_nested_links_unlink_preservation_and_missing_diagnostics() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let task = json_success(&root, &["add", "Nested"]);
    let id = task_id(&task);
    fs::create_dir_all(root.join("Tasks/nested/deep")).expect("nested");
    let old_path = root.join(task["task"]["path"].as_str().expect("path"));
    let new_path = root.join(format!("Tasks/nested/deep/{id}.md"));
    fs::rename(&old_path, &new_path).expect("move");
    let original = fs::read_to_string(&new_path).expect("read");
    fs::write(
        &new_path,
        original.replacen("---\n", "---\nattachments: {custom: [keep, this]}\n", 1),
    )
    .expect("custom property");
    let body = "Unrelated body.\n`[[Attachments/manual.pdf]]`\n[[Attachments/manual.pdf|Receipt]]\n[x](../../../Attachments/missing.pdf)\n";
    json_success(&root, &["edit", id, "--body", body]);
    fs::create_dir(root.join("Attachments")).expect("attachments");
    fs::write(root.join("Attachments/manual.pdf"), b"manual").expect("manual");
    let listed = json_success(&root, &["attachment", "list", id]);
    assert_eq!(listed["attachments"][0]["availability"], "available");
    assert_eq!(listed["attachments"][1]["availability"], "missing");
    assert!(listed["attachments"][1]["byte_size"].is_null());
    let report = json_success(&root, &["validate"]);
    assert_eq!(report["valid"], true);
    assert!(report["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue["code"] == "attachment_missing"));
    json_success(&root, &["edit", id, "--name", "Still editable"]);
    json_success(
        &root,
        &["attachment", "unlink", id, "Attachments/manual.pdf"],
    );
    let shown = json_success(&root, &["show", id]);
    assert_eq!(
        shown["task"]["body"],
        body.replace("[[Attachments/manual.pdf|Receipt]]", "")
    );
    assert_eq!(
        shown["task"]["extra_properties"]["attachments"],
        serde_json::json!({"custom": ["keep", "this"]})
    );
    json_success(&root, &["attachment", "link", id, "Attachments/manual.pdf"]);
    assert!(json_success(&root, &["show", id])["task"]["body"]
        .as_str()
        .expect("body")
        .contains("../../../Attachments/manual.pdf"));
    json_success(
        &root,
        &["attachment", "unlink", id, "Attachments/missing.pdf"],
    );
    assert!(fs::read(root.join("Attachments/manual.pdf")).is_ok());
}

#[test]
fn attachment_staging_rejects_missing_and_oversized_sources_before_task_publication() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    let good = vault.path().join("good.pdf");
    let large = vault.path().join("large.pdf");
    fs::write(&good, b"good").expect("good");
    let file = fs::File::create(&large).expect("large");
    file.set_len(obsidian_todo::attachments::MAX_ATTACHMENT_BYTES + 1)
        .expect("size");
    for (source, code) in [
        (
            vault.path().join("missing.pdf"),
            "attachment_source_invalid",
        ),
        (large.clone(), "attachment_too_large"),
    ] {
        json_failure(
            &root,
            &[
                "add",
                "Never published",
                "--attach",
                good.to_str().expect("path"),
                "--attach",
                source.to_str().expect("path"),
            ],
            5,
            code,
        );
        assert!(fs::read_dir(root.join("Tasks"))
            .expect("tasks")
            .next()
            .is_none());
        assert!(!root.join("Attachments").exists());
    }
    file.set_len(obsidian_todo::attachments::MAX_ATTACHMENT_BYTES)
        .expect("boundary");
    let task = json_success(
        &root,
        &[
            "add",
            "Exactly 20 MiB",
            "--attach",
            large.to_str().expect("path"),
        ],
    );
    let result = json_success(&root, &["attachment", "list", task_id(&task)]);
    assert_eq!(
        result["attachments"][0]["byte_size"],
        obsidian_todo::attachments::MAX_ATTACHMENT_BYTES
    );
    let before = store_bytes(&root);
    json_failure(
        &root,
        &[
            "attachment",
            "add",
            task_id(&task),
            good.to_str().expect("path"),
            "/nonexistent-attachment.pdf",
        ],
        5,
        "attachment_source_invalid",
    );
    assert_eq!(store_bytes(&root), before);
}

#[test]
fn attachments_overlap_disables_only_attachment_operations() {
    let vault = fake_vault();
    let root = initialize(vault.path());
    fs::rename(root.join("Tasks"), root.join("Attachments")).expect("move tasks");
    let config = root.join(".todo/config.toml");
    fs::write(
        &config,
        fs::read_to_string(&config).expect("config").replace(
            "tasks_directory = \"Tasks\"",
            "tasks_directory = \"Attachments\"",
        ),
    )
    .expect("change config");
    let task = json_success(&root, &["add", "Ordinary still works"]);
    json_success(&root, &["edit", task_id(&task), "--name", "Editable"]);
    json_failure(
        &root,
        &["attachment", "list", task_id(&task)],
        5,
        "attachments_disabled",
    );
    let report = json_success(&root, &["validate"]);
    assert!(report["issues"]
        .as_array()
        .expect("issues")
        .iter()
        .any(|issue| issue["code"] == "attachments_disabled"));
}

#[cfg(unix)]
#[test]
fn attachment_symlink_sources_targets_and_traversal_are_refused() {
    use std::os::unix::fs::symlink;
    let vault = fake_vault();
    let root = initialize(vault.path());
    let source = vault.path().join("source.pdf");
    fs::write(&source, b"private").expect("source");
    let alias = vault.path().join("alias.pdf");
    symlink(&source, &alias).expect("source symlink");
    json_failure(
        &root,
        &[
            "add",
            "Never published",
            "--attach",
            alias.to_str().expect("path"),
        ],
        5,
        "attachment_source_invalid",
    );
    let task = json_success(&root, &["add", "Existing"]);
    fs::create_dir(root.join("Attachments")).expect("directory");
    symlink(&source, root.join("Attachments/alias.pdf")).expect("target symlink");
    json_failure(
        &root,
        &[
            "attachment",
            "link",
            task_id(&task),
            "Attachments/alias.pdf",
        ],
        5,
        "unsafe_attachment_path",
    );
    for path in [
        "../source.pdf",
        "Attachments/../Tasks/x.md",
        "Attachments//x.pdf",
        "/Attachments/x.pdf",
        "Attachments/./x.pdf",
    ] {
        json_failure(
            &root,
            &["attachment", "link", task_id(&task), path],
            5,
            "unsafe_attachment_path",
        );
    }
    assert_eq!(fs::read(source).expect("source preserved"), b"private");
}
