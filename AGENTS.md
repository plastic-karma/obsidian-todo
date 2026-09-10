# Repository Guidelines

## Project Overview

`obsidian-todo` is a local-first Rust library plus the `otodo` CLI. It stores tasks and projects as human-editable Markdown notes with YAML front matter inside an existing Obsidian vault. Durable state is filesystem-only: `.todo/config.toml`, `.todo/schema.json`, `Tasks/**/*.md`, `Projects/*.md`, and ordinary `Attachments/**` files with task body links; initialization also creates `todos.base` for Obsidian.

`REQUIREMENTS.md` is the normative v2 specification with explicit legacy-v1 support. Treat its RFC 2119 requirements and non-goals as hard constraints. V2 enables optional `parent` ULIDs; v1 remains flat and preserves any unknown `parent` metadata. Never auto-upgrade a store or create a hidden database. Ordinary operations, discovery, initialization, and validation remain offline, Git-free, and store-contained (apart from explicit body-file/attachment source reads). Only user-invoked `/sync [ours|theirs]` in `input` may access the containing Git worktree, metadata, and configured upstream under section 18.1.

## Architecture & Data Flow

1. `src/main.rs` calls `src/cli.rs::run`; `cli::execute` is the composition root and maps Clap arguments into command request structs.
2. `init` resolves a vault and creates a contained v2 store. Store commands, including explicit upgrade, discover a store in this order: `--root`, `OBSIDIAN_TODO_ROOT`, nearest ancestor store, then one unambiguous direct child. `capabilities`, help, and executable-version queries bypass store discovery.
3. `Store::open` in `src/store.rs` canonicalizes paths, loads strict TOML config, verifies the exact embedded schema for explicitly supported v1/v2, and establishes managed-directory boundaries. Operations must recheck config/schema generation and config path identity after locking and before publication; upgrades require quiesced old writers and fail-closed explicit resume.
4. `src/commands/{task,project}.rs` owns use-case transactions and cross-record invariants. Commands acquire shared/exclusive store locks, load records, validate references and state transitions, then serialize changes. V2 parent edits validate the effective graph, preserve repairability of well-typed faults, and never cascade; deleting a task with any direct children refuses.
5. `src/frontmatter.rs` parses bounded YAML front matter into `Task`/`Project` models while preserving Markdown bodies and unknown Obsidian properties. `src/model.rs` and `src/recurrence.rs` enforce domain rules.
6. Mutations carry a full-file snapshot. Before replace/delete, the store re-reads and hashes the source; mismatches fail as concurrent edits. Relation-sensitive operations also recheck identity/edge dependencies, including newly added children. Writes use same-directory temporary files, sync, atomic persist/rename, and parent-directory sync. These are single-file guarantees, not multi-file atomicity against external editors.
7. `src/output.rs` renders either human output or the stable versioned JSON envelope. Success goes to stdout; errors and diagnostics go to stderr with stable error codes and exit classes.
8. `src/sync.rs` owns explicit Git synchronization and typed conflict resolution; input adapters provide either the interactive resolver or a noninteractive policy. Automatically stage/commit only selected-store changes, but fetch/merge/push the whole containing branch, including already committed unrelated paths. Require an existing attached branch/upstream, configured author/authentication, clean unrelated paths, no pre-existing Git operation/conflicts, and quiesced writers/sync. Preserve nonconflicting text in both policies; prompt per hunk, falling back to file choice for binary/delete/structural conflicts. Never hold store locks across Git changes or prompts. Abort only this invocation's active merge on resolver cancellation/error, retain automatic local commits and failed-push results, then reload store/catalog before further input.

Whole-store validation is a separate tolerant path: `src/validate.rs::validate_store` accumulates and deterministically sorts independent issues. Do not replace it with fail-fast `Store::open` behavior.

## Key Directories

- `src/attachments.rs`: shared explicit-link parsing, safe source staging, attachment metadata, and diagnostics.
- `src/commands/`: initialization and task/project application operations.
- `src/`: CLI boundary, persistence, domain models, codecs, discovery, validation, synchronization, and errors.
- `tests/`: black-box CLI coverage in `tests/cli.rs` and isolated Git synchronization coverage in `tests/sync.rs`.
- `benches/`: Criterion benchmarks for recurrence, front matter, and store scanning.
- `assets/`: authoritative generated-store asset `schema.json`.

There are no checked-in scripts, task runners, CI workflows, or separate `docs/` tree.

## Development Commands

Use Cargo directly from the repository root:

```bash
cargo check --all-targets
cargo build
cargo run --bin otodo -- --help
cargo run --bin otodo -- init Todo --vault-root .
```

Repository QA gate from `REQUIREMENTS.md`:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Focused iteration examples:

```bash
cargo test --lib frontmatter::tests
cargo test --bin otodo cli::tests
cargo test --test cli full_task_project_recurrence_and_unknown_property_workflow -- --exact
cargo bench --bench core
```

Use `cargo build --release` for the optimized binary. Automation may add `--locked` because `Cargo.lock` is committed.

## Code Conventions & Common Patterns

- Follow standard Rust formatting and naming. Types/request DTOs use `PascalCase` (`AddTask`, `EditProject`, `ValidationReport`); functions and stable error codes use `snake_case`.
- Reuse semantic helper prefixes: `parse_*`, `serialize_*`, `validate_*`, `ensure_*`, and `reject_*`. Helpers ending `_unlocked` must only run inside an existing `Store::with_shared_lock` or `Store::with_exclusive_lock` closure; avoid nested lock-taking APIs.
- Return `crate::error::Result<T>` and propagate with `?`/`map_err`. Build typed errors through `Error::{usage, not_found, validation, unsupported, io}` and attach `path`, `field`, or source location. Malformed user/repository data must not panic or produce a backtrace.
- Treat config, paths, filenames, links, and YAML as hostile input. Preserve path containment and symlink defenses. Never construct shell commands from record data or invoke editors/hooks. Git subprocesses belong only to explicit `/sync`; use argument arrays, capture output, disable hooks and credential/editor prompts, and never init, force-push, stash, reset-hard, rebase, or change Git configuration.
- Never regex-parse front matter. Preserve unknown YAML values semantically and preserve an untouched Markdown body byte-for-byte. Writers normalize user-supplied bodies to LF with one trailing newline, emit canonical core-property order, and sort projects/tags deterministically.
- Task identity is the 26-character ULID filename, never front matter. Tasks may be nested recursively; projects are flat and use stable lowercase slugs. V2 `parent` is an optional quoted full ULID, case-insensitive on read/canonical-uppercase on write, emitted after tags; omission means root. Derive children by identity, never persist child arrays or inherit metadata/lifecycle. State is a string validated against ordered `Config.states`, not a Rust enum.
- Production code is synchronous. There is no async runtime, global mutable state, service container, or storage abstraction. Dependency injection is explicit: command functions receive `&Store` plus request structs, and time-sensitive code receives `Clock` (`SystemClock` or `FixedClock`).
- Prefer command-layer APIs when cross-record invariants matter. Low-level store reads do not uniformly enforce project-reference consistency.

## Important Files

- `REQUIREMENTS.md`: normative behavior, storage, safety, CLI, and definition-of-done contract.
- `Cargo.toml`: crate targets, Rust 1.82 MSRV, dependencies, lint policy, and release profile.
- `src/lib.rs`: public library facade and module graph.
- `src/main.rs`, `src/cli.rs`, `src/output.rs`: executable entry, dispatch, and output contracts.
- `src/input.rs`, `src/input_ui.rs`: line parsing, plain input, focused TUI, and conflict-choice presentation.
- `src/sync.rs`: explicit store-scoped automatic commits, whole-branch upstream synchronization, and conflict resolution.
- `src/store.rs`: locking, scanning, snapshots, atomic writes, permissions, and symlink containment.
- `src/config.rs`: strict generated config, managed paths, workflow states, schema/base constants, and Obsidian links.
- `src/model.rs`, `src/recurrence.rs`: domain invariants, clock injection, recurrence parsing, and date advancement.
- `src/frontmatter.rs`: safe YAML/Markdown parsing and canonical serialization.
- `src/validate.rs`, `src/error.rs`: aggregate diagnostics, stable codes, and exit-status mapping.
- `assets/`: exact supported structural schemas copied to initialized/upgraded stores; retain the historical v1 asset and keep v1/v2 schemas aligned with runtime validation and tests.

## Runtime/Tooling Preferences

- Rust 2021, MSRV 1.82; stable Rust only. No `rust-toolchain` pin exists.
- Cargo is the only package manager. Do not hand-edit `Cargo.lock`.
- Use `serde_yaml_ng`, not the similarly named legacy crate. Serialization order matters; `serde_json` enables `preserve_order`.
- `unsafe` code is forbidden. Clippy `all` is denied, except the explicit allowances in `Cargo.toml`.
- Runtime is blocking and file-backed: no async runtime, database, or HTTP client. Ordinary commands need neither Git nor network; explicit `/sync` alone depends on the installed Git executable and existing upstream transport/authentication, with no new Rust dependency or persistent metadata.
- Linux and macOS are the supported atomic-write targets. Unix-specific permissions/path tests are conditionally compiled.
- For scripts/agents, prefer `--format json`, pass `--root` or `OBSIDIAN_TODO_ROOT`, use full task IDs for destructive operations, and pass `--today YYYY-MM-DD` for deterministic date-sensitive behavior.

## Testing & QA

Tests use standard Rust `#[test]`, `assert_cmd`, `TempDir`, and Criterion; no snapshot, mocking, async-test, or property-testing framework is present.

Choose checks for the changed behavior during iteration, then run the required repository QA gate for implementation changes. Once checks pass, repeat or broaden them only for new changes, failures, or unresolved concerns. For instruction/documentation-only changes, validate the affected content and references without running unrelated Rust builds, benchmarks, or tests.

Complete authorized work using the current specification and repository context for routine choices; ask only when an unresolved choice materially changes the outcome. Delegate substantial independent slices when useful, with disjoint ownership and one integration owner who runs shared verification after integration. Explicit user scope takes precedence over skill defaults; reading a workflow is not a request to execute it.

- Colocated `#[cfg(test)]` modules cover private parser, model, recurrence, storage, validation, output, and command invariants.
- `tests/cli.rs` runs the compiled binary against real temporary vaults and verifies streams, exit codes, JSON shapes, exact files, preservation, containment, concurrency refusal, discovery, and ordinary-command no-Git behavior. `tests/sync.rs` uses isolated local Git repositories/remotes to defend store-only commits, whole-branch synchronization, conflict policies, refusal boundaries, cancellation, and retained work after failures.
- Keep tests deterministic with fixed ULIDs where identity is incidental, `FixedClock`/`--today` for dates, local fixture builders, and per-test `TempDir`s. Do not mutate process-global environment when command-local injection works.
- Assert observable contracts: serialized bytes, resulting filesystem state, stable error code/exit class, stdout versus stderr, JSON fields, and unchanged unrelated data. Avoid tests that only mirror source structure.
- Changes to record fields/order, line endings, JSON, schema, recurrence, filesystem safety, or CLI output require the nearest behavioral unit test plus relevant black-box CLI coverage. Keep `REQUIREMENTS.md`, supported schema assets, Rust validation, generated output, and the shared subtask conformance corpus synchronized.
- No numeric coverage threshold or coverage tool is configured. Behavioral completeness and the full format/lint/test gate are the acceptance standard.
