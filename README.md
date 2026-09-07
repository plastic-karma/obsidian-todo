# obsidian-todo

`obsidian-todo` is a local-first Rust library and `otodo` CLI for managing structured tasks inside an existing Obsidian vault. Tasks and projects remain human-editable Markdown files with YAML front matter—no database, server, network access, or Git automation.

## Features

- Workflow states, projects, tags, due dates, and recurrence
- Ordinary file attachments in v1 and v2 stores, with Markdown links and image embeds
- Arbitrarily nested subtasks with stable full-ULID parent identity
- Recursive task discovery and deterministic filtering and sorting
- Atomic writes with concurrent-edit detection
- Human-readable output and a versioned JSON interface
- Whole-store validation with actionable diagnostics
- A generated `todos.base` table for Obsidian

## Install

Rust 1.82 or newer is required.

```sh
cargo install --path .
```

## Quick start

From the root of an existing Obsidian vault:

```sh
otodo init Todo --vault-root .
otodo --root Todo project create work --name Work
otodo --root Todo add "Review plan" --project work --tag review --due-date 2026-09-07
otodo --root Todo list
otodo --root Todo complete <task-id>
otodo --root Todo validate
```

Use `--format json` for scripts and `--today YYYY-MM-DD` for deterministic date-sensitive commands. Run `otodo --help` or `otodo <command> --help` for all options.

The store contains `.todo/config.toml`, `.todo/schema.json`, `Tasks/**/*.md`, `Projects/*.md`, `todos.base`, and imported files below `Attachments/`. Other vault content is left untouched.

## Subtasks

New stores use schema version 2. A child is an ordinary task with one optional
`parent: "FULL_PARENT_ULID"` property; a root omits the property. Parent IDs are
full task filename ULIDs, not names, paths, prefixes, or Obsidian links.

```sh
otodo --root Todo add "Prepare release"
otodo --root Todo add "Check signing" --parent <full-parent-id>
otodo --root Todo list --all --parent <full-parent-id>
otodo --root Todo list --all --roots
otodo --root Todo edit <child-id> --parent <another-full-parent-id>
otodo --root Todo edit <child-id> --clear-parent
```

Parents may be terminal. Each task keeps its own state, projects, tags, dates,
recurrence, and body: creating, completing, rescheduling, or reparenting a task
does not change its relatives. Deleting a task with any direct child is refused,
including when every child is terminal; explicitly detach, reparent, or delete
the children first. Moving a task within the configured Tasks tree preserves
relationships because identity comes from its filename.

Lists validate the whole graph before filtering. Missing parents, self-links,
cycles, and duplicate IDs are diagnosed rather than hidden by a query. Use
`show`, `validate`, and explicit parent edits to inspect and repair typed broken
relationships; unrelated safe edits remain possible.

Scripts can discover support without opening a store and request bounded,
literal case-insensitive name/ID candidates:

```sh
otodo --format json capabilities
otodo --root Todo --format json list --all --summary --query signing --limit 25
```

JSON envelopes remain version 1. Task JSON includes nullable `parent`; compact
results contain `id`, `path`, `name`, `state`, `terminal`, and `parent`, plus an
envelope-level `has_more`. `--limit` requires `--summary` and accepts 1–1000.
The Omarchy `br.otodo` Parent picker uses these bounded queries; it requires a
subtask-capable CLI and a v2 store for child creation. Ordinary root quick-add
continues to work with legacy stores and CLIs.

## Explicit schema upgrade

Schema-1 stores remain usable and flat. A legacy YAML `parent` property remains
unknown metadata, not a relationship. Parent operations require schema 2; no
command or client silently upgrades a store.

Before upgrading, reconcile and safeguard pending/offline work, then stop
**all writers and sync**, including old CLIs, mobile clients, and Obsidian Git.
An advisory lock cannot protect against an already-running old writer.

```sh
otodo --root Todo upgrade --to 2 --dry-run
otodo --root Todo upgrade --to 2
```

Any existing top-level legacy `parent` key blocks the upgrade, regardless of
its value. Deliberately relocate/remove that metadata first; values are never
silently promoted or discarded. Upgrade preserves task/project files and every
existing `todos.base` byte, including customizations. New Bases display a plain
parent-ID column; enable that property yourself in an existing customized Base.

Upgrade installs the v2 schema before updating the configuration version.
If interrupted between those replacements, ordinary CLI operations fail closed;
rerun the same explicit upgrade command to preflight and resume. Repeating a
completed upgrade is a no-op. Commit/synchronize the metadata through your normal
workflow before restarting only clients that support schema 2. Old v1 clients
reject the upgraded store; there is no automatic downgrade.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo bench --bench core
```

## License

MIT


## Attachments

Attachments work with both supported store schemas and need no upgrade. Files stay ordinary vault files; task bodies link to them and images use Markdown embeds. The CLI leaves custom frontmatter properties named `attachments` unchanged.

```sh
otodo --root Todo add "Submit expenses" --attach ./receipt.pdf --attach ./photo.png
otodo --root Todo attachment add <task-id> ./invoice.pdf ./notes.txt
otodo --root Todo attachment list <task-id> --format json
otodo --root Todo attachment link <task-id> Attachments/manual.pdf
otodo --root Todo attachment path <task-id> Attachments/manual.pdf
otodo --root Todo attachment unlink <task-id> Attachments/manual.pdf
```

Imports copy the original bytes into `Attachments/<fresh-ULID>/<sanitized-filename>` and accept up to 20 MiB per file. Source files and stored targets must be regular files without symlink components. Imports stage all source files before publishing a task. Files publish first; interrupted operations can leave unreferenced files. I/O failures can be uncertain, so check the task before retrying a creation.

`attachment list` returns store-relative `path`, `display_name`, `byte_size` (null for missing files), and `availability` (`available` or `missing`). `attachment path` prints an absolute existing local path suitable for opening with your preferred application. Unlink removes that task's links and retains the file. Task completion, recurrence, and deletion retain files too; several tasks may reference one attachment.

Manually copied files below `Attachments/` work as well. Use an explicit relative Markdown link such as `[Receipt](../Attachments/manual.pdf)` or `[[Attachments/manual.pdf|Receipt]]` (also `[[Todo/Attachments/manual.pdf]]` when `Todo` is the configured Obsidian link prefix); adjust `../` for nested task locations. Shortened links such as `[[manual.pdf]]` cannot identify attachments reliably: use an explicit path. Code examples are ignored. `validate` reports missing or unsupported attachment references without preventing ordinary task edits. If a customized task/project directory overlaps `Attachments/`, attachment operations are disabled until the directories are separated.

To paste directly into this folder in Obsidian, open **Settings → Files and links → Default location for new attachments**, choose **In the folder specified below**, and select the vault-relative store folder, for example `Todo/Attachments`. This preference applies to the entire vault; OTodo never changes it. See [Obsidian's attachment documentation](https://help.obsidian.md/attachments). Ensure pasted links include an explicit path if Obsidian shortens them. Git/Obsidian Git synchronization and sparse checkouts must include `Attachments/` alongside task files. Git remains external to the CLI.
