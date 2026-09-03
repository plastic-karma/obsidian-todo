# obsidian-todo

`obsidian-todo` is a local-first Rust library and `otodo` CLI for managing structured tasks inside an existing Obsidian vault. Tasks and projects remain human-editable Markdown files with YAML front matter—no database, server, network access, or Git automation.

## Features

- Workflow states, projects, tags, due dates, and recurrence
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

The store contains `.todo/config.toml`, `.todo/schema.json`, `Tasks/**/*.md`, `Projects/*.md`, and `todos.base`. Other vault content is left untouched.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo bench --bench core
```

## License

MIT
