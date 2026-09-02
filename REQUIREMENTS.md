# Obsidian Todo CLI — Implementation Requirements

Status: normative implementation specification for the first production-capable release

Repository name: `obsidian-todo`

Rust package name: `obsidian-todo`

Library crate name: `obsidian_todo`

CLI executable name: `otodo`

## 1. Purpose

Build a local-first Rust CLI for managing structured todo notes stored inside a folder of an existing Obsidian vault. The entire vault may be a Git repository containing unrelated notes, daily notes, attachments, and Obsidian configuration. The todo system owns only its configured folder.

The CLI must work when a client has a sparse checkout containing only the todo folder. It must not require the rest of the Obsidian vault, GitHub, an Obsidian installation, or network access.

Git synchronization is deliberately external. On the primary desktop, the Obsidian Git plugin may commit, pull, merge or rebase, and push the whole vault. Other machines and agents may use ordinary Git, sparse checkout, branches, or pull requests. The CLI manages files and domain semantics only.

The resulting files must remain useful without the CLI:

- Obsidian must recognize task metadata as Properties/YAML front matter.
- A person must be able to inspect and edit every task with a text editor.
- Another implementation must be able to implement this specification without using the Rust code.
- No required state may exist only in a database, Git commit message, Git branch, Git tag, GitHub Issue, or Obsidian-private metadata.

Normative terms `MUST`, `MUST NOT`, `SHOULD`, and `MAY` have their RFC 2119 meanings.

## 2. Product goals

The first release MUST:

1. Initialize a self-contained todo store inside an existing Obsidian vault.
2. Discover and operate on that store without assuming it is the Git repository root.
3. Create, read, list, edit, complete, cancel, reopen, and intentionally delete tasks.
4. Create, read, list, edit, and safely delete first-class projects.
5. Support one current workflow state per task, with repository-configured states.
6. Support zero or more projects and zero or more Obsidian tags per task.
7. Store a required task name and an optional Markdown body.
8. Support optional due dates.
9. Support fixed-schedule and completion-relative recurring tasks.
10. Preserve unknown Obsidian properties when changing known properties.
11. Validate the complete todo store and report actionable errors.
12. Provide stable JSON output for scripts and agents.
13. Detect concurrent changes instead of silently overwriting them.
14. Perform single-file mutations using atomic replacement.
15. Work when only the todo folder is materialized by Git sparse checkout.
16. Never invoke Git or modify files outside the todo store during normal operation.
17. Expose the domain and storage implementation as a Rust library reusable by a future desktop application.

## 3. Non-goals for the first release

The first release MUST NOT implement:

- A hosted backend, daemon, web service, or account system.
- GitHub Issues or GitHub Projects synchronization.
- Automatic `git add`, commit, pull, rebase, merge, or push.
- Branch management or pull-request creation.
- A TUI or graphical desktop application.
- Notifications, alarms, or a background scheduler.
- Timed due dates; v1 due values are calendar dates only.
- Assignments, priorities, dependencies, subtasks, comments, or attachments as domain fields.
- Full historical occurrence tracking for recurring tasks.
- Arbitrary RFC 5545 recurrence features beyond the subset defined here.
- Semantic three-way merging of conflicting task files.
- Mutable folder placement based on state, project, tags, or due date.
- A local SQLite index or any other required derived database.
- Project slug renaming. A project display name can change; its slug is a stable identifier.
- Silent repair of malformed files.

These exclusions are scope boundaries, not placeholders. Code must not add partially implemented versions of them.

## 4. System boundary and invariants

### 4.1 Todo store boundary

A todo store is a directory containing `.todo/config.toml`. The directory containing `.todo/` is the store root.

Given this vault:

```text
MyVault/
├── .git/
├── .obsidian/
├── Daily/
├── Notes/
└── Todo/
    ├── .todo/
    │   ├── config.toml
    │   └── schema.json
    ├── Tasks/
    └── Projects/
```

`MyVault/Todo` is the store root. `MyVault` is the vault and Git worktree root, but neither is required during normal CLI operation.

Except for explicitly reading a user-supplied `--body-file`, the CLI MUST NOT read, create, modify, rename, or delete any path outside the store root. It MUST NOT modify `.obsidian/`, `.git/`, vault notes, or repository-level configuration.

### 4.2 Canonical state

The canonical current state consists only of:

- `.todo/config.toml`
- `.todo/schema.json`
- Markdown project records under the configured projects directory
- Markdown task records under the configured tasks directory

Git history is an audit and synchronization mechanism, not required application state. The Obsidian Git plugin is free to group unrelated note and task changes in one commit. Therefore application behavior MUST NOT depend on commit boundaries, commit messages, author dates, branches, tags, or a reachable `.git` directory.

### 4.3 Stable placement

Task state, project membership, tags, due date, and recurrence MUST be represented only in front matter. Changing them MUST NOT move the task file.

Folders such as `Tasks/Open`, `Tasks/Done`, or `Tasks/ProjectName` MUST NOT have domain meaning.

Task identity is the ULID filename basename, independent of any future subdirectory. The scanner MUST recursively inspect the configured tasks directory. V1 initialization MUST create tasks directly in the top-level tasks directory; it MUST NOT pre-shard them.

### 4.4 Obsidian compatibility

Task and project records MUST be UTF-8 Markdown files with YAML front matter beginning at the first byte of the file. Front matter uses an opening line `---`, YAML content, and a closing line `---`. The remainder is the Markdown body.

Core properties MUST be flat because Obsidian Properties does not provide a normal UI for nested values. The task body MUST remain outside front matter because Obsidian does not render Markdown inside property values.

## 5. Terminology

- **Vault:** The surrounding Obsidian vault, which may also be a Git worktree.
- **Store:** The folder containing `.todo/config.toml` and all application-owned data.
- **Task:** One Markdown task record under the tasks directory.
- **Task ID:** The 26-character uppercase ULID filename basename.
- **Project:** One Markdown project record under the projects directory.
- **Project slug:** The stable project filename basename.
- **State:** Exactly one configured workflow state referenced by a task.
- **Terminal state:** A configured state with `terminal = true`.
- **Current occurrence:** The occurrence represented by a recurring task's current `due_date`.
- **External writer:** Obsidian, another CLI invocation, a Git checkout operation, an agent, or any process capable of changing store files.

## 6. Required directory layout

The initialized layout MUST be:

```text
<store-root>/
├── .todo/
│   ├── config.toml
│   └── schema.json
├── Tasks/
└── Projects/
```

Directory names are configurable after initialization, but all configured managed paths MUST:

- Be relative to the store root.
- Be normalized paths without `.` or `..` components.
- Not be absolute.
- Resolve inside the store root.
- Not be symlinks.
- Not traverse through a symlink.
- Be distinct from one another and from `.todo`.

The CLI MUST reject a store whose managed paths violate these constraints. This prevents a malicious or mistaken config from causing writes outside the todo folder.

The CLI MUST NOT create cache, lock, index, temporary, or log files that remain in the store after a successful command. Temporary files used for atomic writes MUST be removed on success and best-effort removed on failure.

## 7. Store initialization and discovery

### 7.1 Initialization

Command:

```text
otodo init <store-path> --vault-root <vault-path>
```

`--vault-root` MAY be omitted only when the CLI can find an ancestor containing `.obsidian/`. Initialization MUST fail with a clear error if it cannot determine the vault root.

Initialization requirements:

1. Resolve the vault root and requested store path without following a path outside the vault.
2. Require the store path to be inside the vault root.
3. Fail if `.todo/config.toml` already exists.
4. Fail if the target exists and contains files unless `--adopt-empty-layout` is explicitly supplied and all existing managed directories are empty.
5. Create `.todo`, `Tasks`, and `Projects`.
6. Calculate `obsidian_link_prefix` as the store path relative to the vault root, using `/` separators.
7. Write the default config from section 8.
8. Write `schema.json` for editor and external-tool consumption.
9. Validate the resulting store before returning success.
10. Never initialize or modify Git.

The command MUST support `--dry-run`, which reports planned paths and files but makes no changes.

### 7.2 Discovery precedence

All commands except `init` MUST locate the store using this precedence:

1. Global `--root <path>`.
2. `OBSIDIAN_TODO_ROOT` environment variable.
3. Starting at the current directory, walk ancestors and use the first directory containing `.todo/config.toml`.
4. If no ancestor is a store, inspect only the current directory's direct children. If exactly one direct child contains `.todo/config.toml`, use it.
5. Otherwise fail and explain how to pass `--root`.

If direct-child discovery finds multiple stores, the command MUST fail as ambiguous and list their paths. It MUST NOT choose by directory name or modification time.

Scripts SHOULD always pass `--root` or set `OBSIDIAN_TODO_ROOT`.

Discovery MUST NOT invoke Git, require `.git`, recursively scan the entire vault, or inspect sibling vault notes.

Command:

```text
otodo root
```

This prints the resolved store root. In JSON mode it also reports config, tasks, and projects paths.

## 8. Store configuration

Default `.todo/config.toml`:

```toml
schema_version = 1

tasks_directory = "Tasks"
projects_directory = "Projects"
obsidian_link_prefix = "Todo"
default_state = "open"

[[states]]
id = "open"
name = "Open"
terminal = false

[[states]]
id = "active"
name = "Active"
terminal = false

[[states]]
id = "blocked"
name = "Blocked"
terminal = false

[[states]]
id = "done"
name = "Done"
terminal = true

[[states]]
id = "cancelled"
name = "Cancelled"
terminal = true
```

The generated `obsidian_link_prefix` MUST reflect the actual vault-relative store path rather than always being `Todo`.

Configuration validation:

- `schema_version` MUST equal `1` for v1 clients.
- A newer version MUST produce an unsupported-schema error and no mutation.
- State IDs MUST be unique, nonempty lowercase ASCII slugs matching `[a-z0-9][a-z0-9_-]*`.
- State display names MUST be nonempty after trimming.
- At least one state MUST be nonterminal.
- `default_state` MUST reference a configured nonterminal state.
- The same state ID MUST NOT appear more than once.
- State array order defines presentation and board-column order.
- Unknown top-level config keys MUST cause a validation error in v1. Configuration typos must not be silently ignored.
- Managed directory and link-prefix rules from sections 6 and 10 apply.

`schema.json` is a machine-readable description of record shapes. The Rust validation code remains authoritative for cross-record checks, recurrence semantics, path containment, and duplicate-key rejection that JSON Schema cannot fully express. The checked-in schema and Rust model MUST be kept in agreement by tests.

## 9. Task storage schema

### 9.1 Path and identity

A task path is:

```text
<tasks-directory>/<optional-subdirectories>/<ULID>.md
```

V1 writers create:

```text
Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md
```

Task IDs MUST:

- Be valid 26-character Crockford Base32 ULIDs.
- Be uppercase in filenames emitted by the CLI.
- Be unique by case-insensitive basename across the recursive tasks tree.
- Be generated locally without coordination.
- Never be stored redundantly in front matter.

A task move within the tasks directory does not change its identity. The CLI does not perform such moves in v1.

Every `.md` file under the tasks directory MUST be a valid task record. Non-Markdown files, hidden editor temporary files, and atomic-write temporary files are ignored by normal scanning; `validate` SHOULD warn about persistent unexpected files.

### 9.2 Canonical example

```markdown
---
name: Review weekly finances
state: open
projects:
  - "[[Todo/Projects/personal-finance]]"
tags:
  - finance
  - review
due_date: 2026-09-06
recurrence: "FREQ=WEEKLY;INTERVAL=1;BYDAY=SU"
recurrence_from: schedule
last_completed_date: 2026-08-30
---

Review transactions, reconcile accounts, and update the monthly budget.
```

### 9.3 Core task properties

| Property | YAML type | Required | Semantics |
|---|---|---:|---|
| `name` | string | yes | Nonempty, single-line task name. |
| `state` | string | yes | Exactly one configured state ID. |
| `projects` | list of strings | yes | Zero or more project wikilinks. |
| `tags` | list of strings | yes | Zero or more Obsidian tags without `#`. |
| `due_date` | date scalar | no | Local calendar date in `YYYY-MM-DD`. |
| `recurrence` | string | conditional | Supported RRULE subset. |
| `recurrence_from` | string | conditional | `schedule` or `completion`. |
| `last_completed_date` | date scalar | no | Most recent completion date for a recurring series. |

The body after the closing delimiter is the task's `body` in API and JSON output.

### 9.4 Name

`name` MUST:

- Be a YAML string.
- Contain at least one non-whitespace Unicode character.
- Contain no `\r` or `\n`.
- Be trimmed by CLI writers at both ends.

The file MUST NOT duplicate the name as a generated Markdown heading. User-authored headings in the body are allowed and have no domain meaning.

### 9.5 State

A task MUST have exactly one `state`, and it MUST reference a configured state ID.

Multiple simultaneous state values are prohibited. Orthogonal classification belongs in tags or projects.

Directly setting a recurring task to a terminal state ends the series; no future occurrence is generated automatically. The dedicated `complete` command has different recurring behavior defined in section 13.

### 9.6 Projects

`projects` MUST always be a YAML list, including when empty:

```yaml
projects: []
```

Each item MUST be a quoted Obsidian wikilink with no alias, fragment, block reference, or `.md` extension:

```yaml
projects:
  - "[[Todo/Projects/personal-finance]]"
```

The link target MUST equal:

```text
<obsidian_link_prefix>/<projects_directory>/<project-slug>
```

using `/` separators regardless of operating system.

Every referenced project MUST exist. Duplicate project references are invalid. Project order has no domain meaning. CLI writers MUST emit project links sorted by project slug.

Aliases such as `[[Todo/Projects/personal-finance|Finance]]` are rejected for core project references because the display text can drift from the project record's `name`.

### 9.7 Tags

`tags` MUST always be a YAML list, including when empty.

Each tag MUST:

- Be a string.
- Be nonempty after trimming.
- Not begin with `#`.
- Contain no whitespace, comma, YAML control character, or Obsidian wikilink delimiter.
- Be unique within the task by exact value.

Tags may contain `/` for Obsidian nested tags. The CLI MUST preserve case and Unicode; it MUST NOT silently lowercase or otherwise rename user tags. CLI writers MUST sort tags by Unicode code-point order to provide stable output.

### 9.8 Body

The body is every byte after the newline terminating the closing `---` delimiter.

Requirements:

- Body content is UTF-8 Markdown.
- Empty body is valid.
- The parser MUST distinguish absent body from malformed front matter, but the API MAY normalize absent body to an empty string.
- A metadata-only edit MUST preserve body content exactly.
- A body supplied by the CLI MUST normalize line endings to LF and end with exactly one newline when nonempty.
- Markdown, wikilinks, code fences, headings, checklists, and `---` lines inside the body have no metadata meaning.

### 9.9 Unknown properties

Obsidian and community plugins allow arbitrary properties. A v1 task mutation MUST preserve unknown front-matter keys and their YAML values semantically.

Rules:

- Core key names are reserved and case-sensitive.
- Duplicate YAML keys are invalid, including duplicate unknown keys.
- Unknown scalar, list, and mapping values MUST survive a known-field mutation.
- YAML comments and original formatting inside front matter SHOULD be preserved when the selected YAML editing library supports it, but comment preservation is not a v1 correctness requirement.
- If the parser cannot safely represent and re-emit an unknown value, the CLI MUST refuse to mutate the file rather than delete or coerce it.
- YAML custom tags, merge keys, and cyclic aliases MUST be rejected.
- YAML parsing MUST use a safe data-only mode and MUST NOT instantiate application objects from YAML tags.

Writers SHOULD emit core properties first in this order, followed by unknown properties in their existing relative order:

```text
name
state
projects
tags
due_date
recurrence
recurrence_from
last_completed_date
```

Property order is presentational only. `validate` MUST accept any property order.

## 10. Project storage schema

### 10.1 Path and identity

A project path is:

```text
<projects-directory>/<slug>.md
```

Project directories are flat in v1. The slug is the stable project ID and MUST match:

```text
[a-z0-9][a-z0-9-]*
```

Slugs are case-sensitive and CLI writers emit lowercase. A display-name change does not rename the slug.

### 10.2 Canonical project

```markdown
---
name: Personal Finance
---

Financial planning, account maintenance, and recurring reviews.
```

Project requirements:

- `name` is required and follows task-name rules.
- The Markdown body is optional and follows task-body rules.
- Unknown Obsidian properties are allowed and preserved under the same rules as task properties.
- No v1 project field duplicates the slug.
- Every `.md` file directly under the projects directory is a project record.
- Nested project files are invalid in v1.

Deleting a project with task references MUST fail. The CLI MUST list referencing task IDs. The user must first remove those references. There is no force mode that creates dangling references.

V1 does not rename project slugs. To change presentation, edit `name`. A future atomic migration command may introduce slug renaming.

## 11. Date semantics

All v1 domain dates are proleptic Gregorian local calendar dates serialized as exactly `YYYY-MM-DD`.

They are not timestamps and have no timezone. The CLI MUST NOT convert them through UTC.

Obsidian may expose YAML dates to parsers as a timestamp-like scalar. The front-matter layer MUST normalize an accepted date token to the logical string form `YYYY-MM-DD` without changing the calendar day.

Invalid dates such as `2026-02-29` MUST fail validation.

The CLI obtains the default current date from the operating system's local timezone. Date-dependent application code MUST use an injected clock abstraction so tests do not depend on wall-clock time.

Global option:

```text
--today YYYY-MM-DD
```

This overrides the current date for deterministic automation, previews, tests, overdue filtering, and default completion dates. It does not rewrite stored dates by itself.

## 12. Recurrence schema and grammar

### 12.1 Invariants

If `recurrence` is present:

- `due_date` MUST be present.
- `recurrence_from` MUST be present.
- `recurrence_from` MUST be `schedule` or `completion`.
- The current `due_date` MUST be a valid occurrence under the rule.
- `last_completed_date` MAY be present.

If `recurrence` is absent:

- `recurrence_from` MUST be absent.
- `last_completed_date` MUST be absent.

`last_completed_date`, when present, MUST NOT move backwards through a normal `complete` operation.

### 12.2 RRULE format

`recurrence` stores an RFC 5545-style rule without an `RRULE:` prefix:

```yaml
recurrence: "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,TH"
```

Parsing is case-insensitive for property names and symbolic values. Writers normalize them to uppercase and emit clauses in this order:

```text
FREQ;INTERVAL;BYDAY;BYMONTHDAY;BYMONTH
```

Duplicate clauses are invalid. Unknown clauses are invalid. Empty clause values are invalid.

Supported subset:

- `FREQ` is required and is one of `DAILY`, `WEEKLY`, `MONTHLY`, or `YEARLY`.
- `INTERVAL` is optional, defaults to `1`, and is a positive integer.
- `BYDAY` is allowed only with `WEEKLY`. It is a comma-separated unique list drawn from `MO,TU,WE,TH,FR,SA,SU`. Ordinals such as `1MO` and `-1FR` are not supported.
- `BYMONTHDAY` is allowed only with `MONTHLY` or `YEARLY`. It is a comma-separated unique list of integers `1..31`. Negative month days are not supported.
- `BYMONTH` is allowed only with `YEARLY`. It is a comma-separated unique list of integers `1..12`.

Defaults derived from the current due date:

- `WEEKLY` without `BYDAY` repeats on the due date's weekday.
- `MONTHLY` without `BYMONTHDAY` repeats on the due date's day of month.
- `YEARLY` without `BYMONTH` repeats in the due date's month.
- `YEARLY` without `BYMONTHDAY` repeats on the due date's day of month.

A month that does not contain a selected day is skipped rather than clamped. For example, a monthly recurrence anchored on the 31st has no February occurrence.

Unsupported v1 clauses include, without limitation:

```text
DTSTART
COUNT
UNTIL
WKST
BYSETPOS
BYYEARDAY
BYWEEKNO
BYHOUR
BYMINUTE
BYSECOND
```

The implementation MUST reject unsupported input. It MUST NOT ignore an unsupported clause or approximate its behavior.

### 12.3 Schedule mode

For:

```yaml
recurrence_from: schedule
```

calendar alignment is preserved. On completion date `D`, compute the first rule occurrence strictly after:

```text
max(current_due_date, D)
```

using the current due date as a valid sequence occurrence and recurrence anchor.

Consequences:

- Completing early advances after the scheduled due date.
- Completing late skips missed past occurrences while retaining weekday/month alignment.
- Completing a Monday weekly task on Wednesday yields the following Monday.

### 12.4 Completion mode

For:

```yaml
recurrence_from: completion
```

completion date `D` becomes the new recurrence anchor. Compute the first rule occurrence strictly after `D` using `D` as the new start.

Examples:

- `FREQ=DAILY;INTERVAL=3` completed on September 9 is next due September 12.
- `FREQ=WEEKLY;INTERVAL=1` completed on Wednesday is next due the following Wednesday.
- A `BYDAY` value remains valid only under the weekly rules above and selects the next matching day in the recurrence sequence.

The implementation MUST include recurrence tests for interval anchoring, multiple weekdays, month boundaries, leap years, invalid month days, early completion, late completion, and both recurrence modes.

## 13. Task operation semantics

### 13.1 Add

```text
otodo add <name> [options]
```

Options:

```text
--state <state>
--project <slug>            repeatable
--tag <tag>                 repeatable
--due-date <YYYY-MM-DD>
--recurrence <rule>
--recurrence-from <schedule|completion>
--body <text>
--body-file <path|->
```

Requirements:

- Default state is configured `default_state`.
- `--body` and `--body-file` are mutually exclusive.
- `--body-file -` reads UTF-8 body content from standard input.
- Every referenced project must exist.
- All recurrence invariants must hold before writing.
- Generate a new ULID and create the target with create-new semantics.
- Retry ULID generation on the effectively impossible filename collision.
- Make no Git commit.
- Return the task and path in JSON mode; print the ID and name in human mode.

### 13.2 List

```text
otodo list [filters]
```

Filters:

```text
--all
--state <state>             repeatable, OR semantics
--project <slug>            repeatable, AND semantics
--tag <tag>                 repeatable, AND semantics
--due-on <date>
--due-before <date>
--due-after <date>
--overdue
--recurring
--non-recurring
```

Default behavior excludes tasks in terminal states.

Filters combine with AND except repeated `--state`, which is an OR set. `--overdue` means a nonterminal task with `due_date < today`.

Default sort:

1. Tasks with due dates before tasks without due dates.
2. `due_date` ascending.
3. Configured state order.
4. Unicode code-point order of `name`.
5. Full task ID.

Human output SHOULD show an unambiguous ID prefix, state, due date, recurrence indicator, projects, and name. Human output is not a stable scripting interface.

If any task is invalid, `list` MUST fail and identify it rather than silently omit it. Users run `validate` to see all errors.

### 13.3 Show

```text
otodo show <id-or-prefix>
```

Human output shows all known properties, unknown properties, source path, and body. JSON output returns the normalized logical model plus unknown properties in a separate object.

### 13.4 Edit

```text
otodo edit <id-or-prefix> [changes]
```

Changes:

```text
--name <name>
--state <state>
--add-project <slug>        repeatable
--remove-project <slug>     repeatable
--add-tag <tag>             repeatable
--remove-tag <tag>          repeatable
--due-date <date>
--clear-due-date
--recurrence <rule>
--recurrence-from <mode>
--clear-recurrence
--body <text>
--body-file <path|->
```

Rules:

- At least one change is required.
- Conflicting changes to the same field are usage errors.
- Removing a missing project or tag is a domain error rather than a silent no-op.
- Adding an existing project or tag is a domain error rather than producing a duplicate.
- `--clear-due-date` is invalid while recurrence remains configured.
- `--clear-recurrence` removes `recurrence`, `recurrence_from`, and `last_completed_date` together. It leaves `due_date` unchanged unless `--clear-due-date` is also passed.
- Adding recurrence to a non-recurring task requires an existing or simultaneously supplied due date and an explicit recurrence mode.
- Metadata-only edits preserve the body exactly.
- All changes to one task are validated and written as one atomic replacement.

V1 does not open an interactive editor. Obsidian or a text editor remains the interactive editing surface.

### 13.5 Complete

```text
otodo complete <id-or-prefix> [--on <date>]
```

`--on` defaults to the effective current date.

For a non-recurring task:

1. Require a nonterminal current state.
2. Set `state` to configured state ID `done`.
3. Leave `due_date` unchanged.

The config MUST contain a terminal state with ID `done` for the `complete` command to be available. If it does not, `complete` fails and instructs the user to use `edit --state`.

For a recurring task:

1. Require a nonterminal current state.
2. Require valid `due_date`, `recurrence`, and `recurrence_from`.
3. Reject a completion date earlier than an existing `last_completed_date`.
4. Compute the next due date according to section 12.
5. Set `last_completed_date` to the completion date.
6. Set `due_date` to the next occurrence.
7. Set `state` to configured `default_state`.
8. Write all changes as one atomic replacement.

Completing a recurring task does not place it in `done`; it completes the current occurrence while keeping the series active.

### 13.6 Finish a recurring series

```text
otodo finish-series <id-or-prefix>
```

Requirements:

- The task must be recurring and nonterminal.
- Set its state to `done`.
- Preserve due, recurrence, recurrence mode, and last-completed values for inspection.
- Generate no next occurrence.

### 13.7 Cancel and reopen

```text
otodo cancel <id-or-prefix>
otodo reopen <id-or-prefix>
```

`cancel` requires a configured terminal state with ID `cancelled` and sets it without changing other fields.

`reopen` requires a terminal current state and sets configured `default_state`. It does not recalculate a recurring due date; an old due date may therefore be overdue.

### 13.8 Delete

```text
otodo delete <full-id> --yes
```

Hard deletion is for mistakes, not normal completion. Requirements:

- Require the full 26-character ID; prefixes are forbidden.
- Require `--yes`; there is no interactive prompt in v1.
- Delete only the resolved task file.
- Refuse to follow a symlink.
- Human output warns that recovery depends on external Git history or backups.

Normal users should prefer `cancel`.

## 14. Project commands

### 14.1 Create

```text
otodo project create <slug> --name <name> [--body <text>|--body-file <path|->]
```

Create a project using create-new and atomic-write semantics. Slug and name rules apply.

### 14.2 List and show

```text
otodo project list
otodo project show <slug>
```

Project list sorts by display name and then slug. JSON output includes the number of currently referencing tasks.

### 14.3 Edit

```text
otodo project edit <slug> [--name <name>] [--body <text>|--body-file <path|->]
```

At least one change is required. The slug and file path do not change.

### 14.4 Delete

```text
otodo project delete <slug> --yes
```

Requirements:

- Require `--yes`.
- Scan all valid tasks immediately before deletion.
- Fail and list task IDs if any task references the project.
- Delete only the project file.
- Never leave known dangling references deliberately.

## 15. Identifier resolution

Task commands accepting `<id-or-prefix>` MUST:

1. Normalize an input ULID prefix to uppercase.
2. Require at least six valid ULID characters unless the input is a full ID.
3. Search task basenames case-insensitively.
4. Succeed only for exactly one match.
5. Return not-found for zero matches.
6. Return ambiguous-ID and list matching full IDs for multiple matches.

Scripts SHOULD use full IDs. Names are never identifiers.

Project commands require exact slugs and do not perform prefix matching.

## 16. Validation

Command:

```text
otodo validate
```

`validate` is read-only and MUST inspect the complete store. It MUST report all independently discoverable errors in one run, sorted by path and then field.

Validation includes:

### Store and config

- Store marker and config readability.
- Supported schema version.
- Unknown config fields.
- Managed path containment and symlink checks.
- Directory existence and distinctness.
- State validity, uniqueness, and default-state rules.
- `schema.json` presence and compatibility with schema version.

### Projects

- Valid project paths and unique slugs.
- UTF-8 and front-matter delimiters.
- Duplicate YAML keys.
- Required name and property types.
- Safe unknown property values.
- No nested project records.

### Tasks

- Valid `.md` path and ULID basename.
- Case-insensitive ID uniqueness.
- UTF-8 and front-matter delimiters.
- No unresolved Git conflict markers.
- Duplicate YAML keys.
- Core property presence and types.
- Name, state, tags, projects, dates, and recurrence rules.
- Project link syntax and referential integrity.
- Conditional recurrence fields.
- Safe unknown property values.

Unresolved conflict markers include the standard line-start forms:

```text
<<<<<<<
|||||||
=======
>>>>>>>
```

A conflict marker anywhere in front matter or body makes the record invalid. The CLI MUST not mutate it.

Validation output must identify, when available:

- Stable error code.
- Store-relative file path.
- Line and column.
- Property name.
- Human-readable explanation.
- Suggested corrective action when deterministic.

`validate` MUST NOT rewrite, normalize, repair, or delete any file.

## 17. Sparse-checkout requirements

A fully functioning sparse client may materialize only:

```text
Todo/**
```

Normal commands MUST therefore:

- Require no `.git` directory or file.
- Require no `.obsidian` directory.
- Require no vault notes outside the store.
- Require no Git executable.
- Use `obsidian_link_prefix` from config rather than calculating it on every run.
- Never search outside the store for project link targets.
- Never infer state from branches or folders.
- Never query Git history for creation, modification, or completion time.

An integration test MUST copy only the initialized store folder to a temporary directory with no `.git` and no `.obsidian`, then demonstrate that add, list, show, edit, complete, project commands, and validate still work with `--root`.

Sparse checkout is a convenience boundary, not security isolation. Repository credentials may still permit access to the rest of the vault. This must be documented in CLI help for initialization or integration guidance, but the CLI does not manage credentials.

## 18. External synchronization and concurrency

### 18.1 Sync ownership

The CLI MUST NOT run Git commands. Obsidian Git or another external tool may commit, pull, merge or rebase, and push at any time.

The CLI MUST tolerate commits that mix todo files with unrelated vault files. No functional behavior may require one operation per Git commit.

### 18.2 Advisory process lock

Mutating CLI commands SHOULD acquire an advisory exclusive lock on the open `.todo/config.toml` file for the duration of the operation. Read-only scans MAY acquire a shared lock. This coordinates multiple `otodo` processes without creating a lock file that Obsidian Git could commit.

Other editors and Git do not honor this lock, so it is not sufficient by itself.

### 18.3 Optimistic concurrency

Before mutating an existing file, the CLI MUST:

1. Read the complete source bytes and file metadata.
2. Compute a cryptographic content hash.
3. Parse and validate the source.
4. Prepare and validate the replacement.
5. Immediately before replacement, reopen and rehash the current source.
6. Abort with a concurrent-modification error if the hash differs.

It MUST never use last-writer-wins after detecting an external change.

### 18.4 Atomic file replacement

For a successful existing-file mutation:

1. Create a uniquely named temporary file in the target's directory with create-new semantics.
2. Apply restrictive initial permissions and preserve the target's relevant permissions.
3. Write complete UTF-8 output.
4. Flush and sync the temporary file.
5. Perform the final source-hash check.
6. Atomically rename the temporary file over the target on supported platforms.
7. Sync the containing directory on Unix where supported.
8. Remove any leftover temporary file on failure.

New records MUST use create-new semantics and MUST never overwrite an existing path.

V1 officially supports Linux and macOS. The implementation SHOULD remain portable to Windows, but it must not claim atomic replacement behavior on a platform until tested there.

### 18.5 Pulled conflicts

A Git pull may leave conflict markers. The CLI must report those records as invalid and refuse normal mutation. V1 does not implement a merge resolver.

Different tasks normally merge as different files. Two writers editing the same task remain an explicit Git/file conflict; no hidden last-writer policy is allowed.

## 19. Front-matter parsing and serialization

The implementation MUST NOT parse YAML front matter with regular expressions.

Parser requirements:

- Require opening delimiter at byte zero; a UTF-8 BOM is invalid.
- Accept LF and CRLF input, but expose logical text consistently.
- Identify only the first valid closing delimiter after the opening delimiter.
- Parse YAML with duplicate-key detection enabled.
- Preserve the body boundary exactly.
- Reject unsafe or unsupported YAML constructs as defined in section 9.9.
- Distinguish syntax errors from schema/type errors.

Serializer requirements:

- Emit UTF-8 and LF line endings.
- Emit `---` delimiters on their own lines.
- Emit list-valued core properties as block lists, except empty lists may be `[]`.
- Quote Obsidian wikilinks.
- Quote recurrence strings.
- Serialize dates as `YYYY-MM-DD`.
- Never serialize an `id` property.
- Preserve unknown values semantically.
- Preserve an untouched body byte-for-byte during metadata-only changes.

The YAML library must be actively maintained or narrowly wrapped and thoroughly tested. If no available library can detect duplicate keys and preserve generic unknown values safely, implement a constrained front-matter layer rather than accepting silent data loss.

## 20. CLI interface and output

Global syntax:

```text
otodo [--root <path>] [--format <human|json>] [--color <auto|always|never>] [--today <date>] <command>
```

Defaults:

- `--format human`
- `--color auto`
- Current local date for `--today`

### 20.1 Standard streams

- Successful primary output goes to stdout.
- Diagnostics and errors go to stderr.
- Human progress noise MUST NOT appear in JSON output.
- JSON mode MUST disable color.
- Non-TTY human output MUST not contain ANSI unless `--color always`.

### 20.2 JSON contract

Every successful JSON document contains:

```json
{
  "version": 1
}
```

A normalized task object contains at least:

```json
{
  "id": "01K4B0ZSBZZV25T1K0D3TA8JHR",
  "path": "Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md",
  "name": "Review weekly finances",
  "state": "open",
  "terminal": false,
  "projects": ["personal-finance"],
  "tags": ["finance", "review"],
  "due_date": "2026-09-06",
  "recurrence": "FREQ=WEEKLY;INTERVAL=1;BYDAY=SU",
  "recurrence_from": "schedule",
  "last_completed_date": "2026-08-30",
  "body": "Review transactions.\n",
  "extra_properties": {}
}
```

Absent optional values MUST be JSON `null`, not omitted, in normalized task output. Arrays are always present.

List output:

```json
{
  "version": 1,
  "tasks": []
}
```

Errors in JSON mode are a single JSON object on stderr:

```json
{
  "version": 1,
  "error": {
    "code": "task_not_found",
    "message": "No task matches ID prefix 01K4B0",
    "path": null,
    "field": null
  }
}
```

The stable scripting contract consists of documented JSON fields, error codes, and exit codes. Human wording and table formatting may evolve.

### 20.3 Exit codes

| Exit code | Meaning |
|---:|---|
| 0 | Success |
| 2 | Command-line usage error |
| 3 | Store, task, or project not found |
| 4 | Ambiguous identifier or ambiguous store discovery |
| 5 | Validation or domain-invariant failure |
| 6 | Concurrent modification or unresolved conflict |
| 7 | Unsupported schema or recurrence feature |
| 8 | Filesystem or I/O failure |

No expected user-data error should cause a panic or Rust backtrace by default.

## 21. Rust architecture

The repository MUST be a normal Cargo package with both a library and binary:

```text
Cargo.toml
src/
├── lib.rs
├── main.rs
├── cli.rs
├── config.rs
├── discovery.rs
├── error.rs
├── frontmatter.rs
├── model.rs
├── output.rs
├── recurrence.rs
├── store.rs
├── validate.rs
└── commands/
    ├── mod.rs
    ├── init.rs
    ├── task.rs
    └── project.rs
```

Exact module splits may change when cohesion demands it, but these boundaries are required conceptually:

- **Model:** Domain types with no filesystem or CLI dependencies.
- **Front matter:** Obsidian/YAML parsing and serialization.
- **Recurrence:** Pure date and rule parsing/calculation.
- **Store:** Contained path resolution, scanning, locks, hashes, and atomic writes.
- **Validation:** Record and cross-record invariants.
- **Commands:** Application operations over the library.
- **CLI/output:** Argument parsing and human/JSON presentation only.

`src/lib.rs` MUST expose enough typed API for a future desktop application to:

- Discover/open a store.
- List and retrieve tasks and projects.
- Execute the same validated mutations.
- Subscribe to no hidden global state.
- Inject a clock for date-dependent operations.

The library MUST NOT print to stdout/stderr or terminate the process. It returns typed results and errors.

The binary MUST be a thin adapter over the library.

Engineering requirements:

- Stable Rust only; no nightly-only features.
- `unsafe` is prohibited unless separately justified and reviewed.
- Structured error enums with stable application error codes.
- No panic on malformed config, YAML, UTF-8, dates, paths, or recurrence input.
- Avoid unnecessary copies of bodies and complete file buffers where practical, but correctness and exact body preservation take priority.
- No network dependency at runtime.
- No Git library or Git executable invocation.
- All dependencies must have a clear purpose and compatible license.
- Use an actively maintained argument parser with generated shell help; `clap` derive is acceptable.
- Use typed date values rather than string arithmetic.
- Use a real ULID implementation rather than custom randomness.
- Use temporary-file and file-lock primitives with tested platform behavior.

## 22. Security and robustness

The CLI processes repository-controlled files that may have been written by an agent or pulled from a remote. Treat all config and Markdown as untrusted input.

Requirements:

- Prevent path traversal through config, project links, IDs, filenames, and symlinks.
- Never execute content from YAML, Markdown, task names, tags, or recurrence strings.
- Bound parser recursion and reject YAML constructs capable of alias expansion abuse.
- Produce useful errors for oversized or pathological records rather than exhausting memory where the selected parser permits limits.
- Never construct shell commands from task data.
- Body-file reading is explicitly user-requested and may access outside the store; no other operation may do so.
- Do not invoke `$EDITOR`, hooks, plugins, or arbitrary commands in v1.
- Do not expose unrelated vault files in JSON output or diagnostics.
- Resolve project links only against the configured in-store project directory.
- Refuse managed symlink directories and symlink task/project files for mutation.
- Preserve existing file permissions during replacement.
- Do not print full task bodies in error messages.

Repository access remains repository-wide. Sparse checkout and path-scoped commands are not confidentiality boundaries. This fact belongs in user-facing integration documentation, but credential management is outside this CLI.

## 23. Test requirements

Tests MUST defend observable behavior and plausible failure modes, not source layout.

### 23.1 Unit tests

Cover at least:

- Valid and malformed front-matter delimiters.
- Duplicate YAML keys.
- Safe preservation of unknown properties.
- Body exact preservation on metadata-only edits.
- CRLF input and LF output behavior.
- Name, tag, state, project-link, slug, and ULID validation.
- Date parsing, leap years, and invalid dates.
- Every supported recurrence frequency and clause.
- Rejected recurrence clauses and duplicate clauses.
- Schedule versus completion recurrence modes.
- Early and late recurring completion.
- Monthly 29th, 30th, and 31st behavior.
- Yearly February 29 behavior.
- Multiple weekly weekdays and intervals.
- ID-prefix unique, absent, and ambiguous resolution.
- Config path containment and symlink rejection.
- Stable JSON serialization with null optional values.

### 23.2 Integration tests

Use isolated temporary directories. Cover at least:

1. Initialize a store inside a fake vault containing `.obsidian` and unrelated notes.
2. Verify initialization does not alter unrelated files or initialize Git.
3. Add a minimal task and validate its exact Obsidian-compatible file.
4. Add a task with projects, tags, due date, recurrence, and body.
5. List/filter/sort in human and JSON modes.
6. Edit metadata while preserving body and unknown properties.
7. Edit body from argument, file, and stdin.
8. Complete non-recurring task.
9. Complete recurring task in both modes and verify next due.
10. Finish, cancel, and reopen a recurring series.
11. Reject missing projects and dangling references.
12. Reject project deletion while referenced.
13. Detect a source change between read and replacement and leave external content untouched.
14. Detect conflict markers and refuse mutation.
15. Validate multiple independent errors in one invocation.
16. Delete only a full-ID-selected task with `--yes`.
17. Run normal operations with no Git executable available.
18. Copy only the store folder into a directory with no vault, `.obsidian`, or `.git`; run all normal commands successfully with `--root`.
19. Verify every command leaves unrelated vault files byte-identical.
20. Verify failed mutations leave no committed replacement and no persistent temporary file.

### 23.3 CLI contract tests

For every command:

- Success exit code.
- Representative domain failure exit code.
- Human stdout/stderr separation.
- JSON success shape.
- JSON error shape.
- No ANSI in JSON or non-TTY auto-color output.
- Help text includes required options and examples.

## 24. Acceptance scenarios

The implementation is not complete until all scenarios below pass end to end.

### Scenario A: Existing shared vault

Given a vault with unrelated Markdown files and an Obsidian Git configuration, `otodo init Todo --vault-root .` creates only `Todo/**`. Adding and editing tasks changes only `Todo/**`. The unrelated files remain byte-identical.

### Scenario B: Obsidian-native record

A task produced by `otodo add` opens as a normal Markdown note. Obsidian recognizes `name` and `state` as text properties, `projects` as a list, `tags` as tags, and `due_date` as a date. Project links are clickable and resolve to files under `Todo/Projects`.

### Scenario C: Sparse external writer

A machine with only `Todo/**` materialized can invoke:

```text
otodo --root Todo project create work --name Work
otodo --root Todo add "Review plan" --project work --tag review
otodo --root Todo list --format json
otodo --root Todo validate
```

without `.obsidian`, `.git`, Git, GitHub, or network access.

### Scenario D: Obsidian Git pull

An external writer changes a different task and Git synchronization replaces files in the worktree. The next CLI invocation reads current disk state and returns the pulled values; it does not serve a stale required cache.

### Scenario E: Concurrent same-task edit

An external writer changes a task after `otodo` reads it but before replacement. `otodo` exits with concurrent-modification status and does not overwrite the external version.

### Scenario F: Fixed weekly recurrence

A task due Monday has:

```yaml
recurrence: "FREQ=WEEKLY;BYDAY=MO"
recurrence_from: schedule
```

Completing it Wednesday sets `last_completed_date` to Wednesday, advances `due_date` to the following Monday, and returns state to the configured default.

### Scenario G: Completion-relative recurrence

A task has:

```yaml
recurrence: "FREQ=DAILY;INTERVAL=3"
recurrence_from: completion
```

Completing it on September 9 sets the next due date to September 12 regardless of its previous due date.

### Scenario H: Manual Obsidian property

A user adds an unknown valid property to a task in Obsidian. A later `otodo edit --state active` preserves that property and its value. The body remains byte-identical.

### Scenario I: Git conflict

A pulled task contains conflict markers. `otodo validate` identifies the path and markers. `show`, `edit`, `complete`, and other mutation commands do not treat it as a valid task or rewrite it.

### Scenario J: Machine consumer

An agent can use only documented `--format json` output and full task IDs to create, find, update, and complete tasks. It never needs to parse human tables, Markdown front matter, Git logs, or commit messages.

## 25. Recommended implementation order

An implementation agent should proceed in this order because each stage supplies prerequisites for the next:

1. Create Cargo library/binary package, typed errors, and CLI shell.
2. Implement config types, store discovery, contained paths, and initialization.
3. Implement front-matter splitting, safe YAML model, task/project parsing, and serialization.
4. Implement full read-only validation and store scanning.
5. Implement project create/list/show/edit/delete.
6. Implement task add/list/show/edit with JSON output.
7. Implement atomic writes, advisory locking, and optimistic concurrency checks before enabling mutation commands broadly.
8. Implement date and recurrence parser/calculator with exhaustive unit tests.
9. Implement complete, finish-series, cancel, reopen, and delete.
10. Add sparse-layout, external-change, full CLI contract, and acceptance tests.
11. Run formatter, Clippy with warnings denied for project code, and the complete test suite.

Each stage must be production behavior, not a stub. Do not expose a command until its invariants, error handling, JSON contract, and behavioral tests are complete.

## 26. Definition of done

The first release is done when:

- Every product goal and acceptance scenario is implemented.
- Every non-goal remains absent rather than partially scaffolded.
- The library and CLI compile on stable Rust on Linux.
- The specified unit, integration, CLI, sparse-checkout, concurrency, and recurrence tests pass.
- `cargo fmt --check` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- `cargo test --all-targets --all-features` passes.
- The CLI demonstrably operates on an initialized store with no Git executable, no `.git`, no `.obsidian`, and no network.
- No normal operation reads or changes files outside the selected store.
- Obsidian-created unknown properties survive known-field mutations.
- Malformed or conflicted input produces an actionable error and no data loss.
- JSON output and exit codes match this specification.

Any implementation that silently loses unknown properties, overwrites a concurrent edit, depends on Git history, mutates unrelated vault content, or accepts unsupported recurrence rules as if valid is incorrect.