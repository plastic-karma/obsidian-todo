# Obsidian Todo CLI — Implementation Requirements

Status: normative implementation specification for store schema v2 and explicit legacy-v1 compatibility

Except where this specification explicitly distinguishes v1 from v2, the established v1 constraints remain requirements for both supported store versions. Store schema version is distinct from CLI JSON and client-local persistence envelope versions.

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

The implementation MUST:

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
18. Support optional, identity-based parent relationships in v2 stores, with independent task lifecycle and explicit repair of broken relationships.
19. Support legacy-v1 flat stores without reinterpreting their unknown properties, and provide a deliberate, fail-closed upgrade to v2.
20. Provide rootless capability discovery and bounded compact task-candidate queries.
21. Import and manage file attachments through ordinary Markdown body links in both store versions, without a schema upgrade or attachment frontmatter field.

## 3. Non-goals

Neither supported store version introduces:

- A hosted backend, daemon, web service, or account system.
- GitHub Issues or GitHub Projects synchronization.
- Automatic `git add`, commit, pull, rebase, merge, or push.
- Branch management or pull-request creation.
- A general-purpose TUI task manager or graphical desktop application. The focused input TUI in section 28 is authorized.
- Notifications, alarms, or a background scheduler.
- Timestamp/timezone persistence or scheduling. Local calendar due dates with optional civil due times are authorized by sections 11 and 28.
- Assignments, priorities, dependencies, comments, or attachments as frontmatter domain fields. File attachments and body links are explicitly authorized by section 27 below. Subtasks are authorized only by the v2 parent contract below; v1 remains flat.
- Full historical occurrence tracking for recurring tasks.
- Arbitrary RFC 5545 recurrence features beyond the subset defined here.
- Semantic three-way merging of conflicting task files.
- Mutable folder placement based on state, project, tags, or due date.
- A local SQLite index or any other required derived database.
- Project slug renaming. A project display name can change; its slug is a stable identifier.
- Silent repair of malformed files.
- Inherited metadata, aggregate completion, cascading changes/deletion, child arrays, or recurring subtree templates.

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

Except for explicitly reading a user-supplied `--body-file` or attachment import source, the CLI MUST NOT read, create, modify, rename, or delete any path outside the store root. It MUST NOT modify `.obsidian/`, `.git/`, vault notes, or repository-level configuration.

### 4.2 Canonical state

The canonical current state consists only of:

- `.todo/config.toml`
- `.todo/schema.json`
- Markdown project records under the configured projects directory
- Markdown task records under the configured tasks directory
- Ordinary attachment files under fixed `Attachments/`, associated only by task body links

Git history is an audit and synchronization mechanism, not required application state. The Obsidian Git plugin is free to group unrelated note and task changes in one commit. Therefore application behavior MUST NOT depend on commit boundaries, commit messages, author dates, branches, tags, or a reachable `.git` directory.

### 4.3 Stable placement

Task state, project membership, tags, parent relationship (v2), due date, and recurrence MUST be represented only in front matter. Changing them MUST NOT move the task file.

Folders such as `Tasks/Open`, `Tasks/Done`, or `Tasks/ProjectName` MUST NOT have domain meaning.

Task identity is the ULID filename basename, independent of any subdirectory. The scanner MUST recursively inspect the configured tasks directory. Initialization and normal task creation in both supported versions MUST create tasks directly in the top-level tasks directory; they MUST NOT pre-shard them.

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
- **Parent (v2):** The single task identified by a task's optional `parent` ULID in the same store.
- **Root (v2):** A task with no `parent` key, not a task whose parent reference is broken.
- **Child (v2):** An ordinary task with a parent; children and descendants are derived, never separately stored.
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
├── Projects/
└── todos.base
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
9. Write `todos.base`, containing a table view filtered to tasks whose `base` property links to that file.
10. Validate the resulting store before returning success.
11. Never initialize or modify Git.

The command MUST support `--dry-run`, which reports planned paths and files but makes no changes.

### 7.2 Discovery precedence

All store commands, including `upgrade`, MUST locate the store using this precedence. `init` uses its explicit destination; `capabilities`, help, and executable-version queries MUST NOT discover or open a store:

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

### 7.3 Explicit v1-to-v2 upgrade

```text
otodo upgrade --to 2 [--dry-run]
```

Normal reads, writes, initialization, and capability discovery MUST NOT automatically upgrade a store. New stores initialize at v2. The upgrader MUST:

1. Require explicit target `2`; unsupported targets fail with `unsupported_schema`. Require users to quiesce all writers and external synchronization, including old CLIs and clients with pending writes, before applying or resuming. The command MUST document this prerequisite, not claim that an advisory lock can enforce it.
2. Discover the contained store, acquire its exclusive config-file lock, and recheck the config/schema generation after acquiring the lock. Preflight exact supported config/schema compatibility, normal path/symlink/duplicate/conflict/record validation, and the entire recursive task tree.
3. Refuse a v1-to-v2 upgrade if ANY task has an existing top-level `parent` key, regardless of type, emptiness, or whether it looks like a valid ULID. It is legacy user/plugin metadata, not consent to promote a field. Report affected paths and `field: parent`; the user must explicitly relocate/remove that metadata first. Do not rewrite or discard it.
4. Preserve all task/project bytes, unknown config values only where already permitted, state configuration, managed paths, permissions, and unrelated files. Preserve the original config except for its schema-version change. The exact historical v1 schema asset MUST remain available for compatibility checks.
5. For `--dry-run`, perform preflight and describe the source/target versions, affected config/schema paths, and any resume state without changing any file. Collision and validation failures remain failures in dry-run.
6. Prepare and sync replacements, then atomically replace `.todo/schema.json` with the exact v2 schema before atomically replacing `.todo/config.toml` with its v2 version, using source snapshots and single-file safety rules. Sync each containing directory as appropriate. This is TWO atomic file replacements, NOT a multi-file atomic transaction.
7. Recognize only these upgrade states: coherent v1 config/v1 schema (start), valid v1 config/exact v2 schema (explicit resume), and coherent v2 config/v2 schema (already complete, no-op). Re-running `upgrade --to 2` explicitly resumes the intermediate pair after the same quiescence, collision, record, containment, and source-snapshot checks. Unsupported, malformed, customized, reversed, or otherwise mismatched pairs MUST fail without guessing or overwriting them.
8. Make the intermediate v1-config/v2-schema state fail closed for all normal store operations, including writes by a newly opened client. `validate` may report the mismatch but MUST NOT repair it. No parent semantics become writable until the coherent v2 pair is installed. A process interrupted before the schema replacement leaves a normal v1 store; one interrupted after the config replacement leaves a normal v2 store.
9. Never automatically roll back or downgrade to v1. Upgrading a relationless store does not require rewriting records or creating a persistent journal, lock file, or transaction framework.
10. Preserve every existing `todos.base` byte, including customized Base files, during upgrade and resume. New-store Base generation may show a plain parent-ID property column, but upgrading MUST NOT replace a user's Base or fabricate parent links from a guessed flat task path.

A held/open config inode is not a reliable lock across replacement of that path. Old clients already holding or waiting on the original inode may use stale v1 semantics. Therefore stopping old writers is a REQUIRED rollout boundary, not an optional precaution; only already updated clients can perform the post-lock generation checks in section 18.2. Installing compatible clients and safeguarding their pending work precedes activating v2. The CLI never runs Git to coordinate rollout.

## 8. Store configuration

Default `.todo/config.toml`:

```toml
schema_version = 2

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

- New clients MUST explicitly support schema versions `1` and `2`, selecting the exact structural schema for that version. All other versions fail with `unsupported_schema` and no mutation, before shape errors in unsupported configurations.
- V1 remains legacy flat semantics: `parent` is an arbitrary unknown task property, not a relationship. V1 parent feature operations (`add/edit --parent`, `edit --clear-parent`, `list --parent`, and `list --roots`) MUST fail with `unsupported_schema`, even when clearing an absent field. Other v1 operations retain their existing semantics and preserve unknown `parent` values.
- State IDs MUST be unique, nonempty lowercase ASCII slugs matching `[a-z0-9][a-z0-9_-]*`.
- State display names MUST be nonempty after trimming.
- At least one state MUST be nonterminal.
- `default_state` MUST reference a configured nonterminal state.
- The same state ID MUST NOT appear more than once.
- State array order defines presentation and board-column order.
- Unknown top-level config keys MUST cause a validation error in both supported versions. Configuration typos must not be silently ignored.
- Managed directory and link-prefix rules from sections 6 and 10 apply.

`schema.json` is a machine-readable description of record shapes. Its parsed document MUST equal the exact supported asset selected by `schema_version`; merely matching a version marker is insufficient. The Rust validation code remains authoritative for cross-record checks, recurrence semantics, path containment, and duplicate-key rejection that JSON Schema cannot fully express. Checked-in schemas and Rust models MUST remain in agreement, including explicit v1/v2 support and rejection of versions such as 0 and 3; tests MUST NOT continue treating v2 as an unsupported future version.

The optional `url` and `due_time` task fields are additive runtime-validated extensions in BOTH schema versions. The historical `schema-v1.json` and `schema.json` asset bytes MUST remain unchanged: their extensible `additionalProperties` already permits these fields. URL/time support MUST NOT upgrade a store or rewrite its schema/configuration. Runtime codecs, model validation, tolerant validation, and behavioral tests enforce their additional constraints.

## 9. Task storage schema

### 9.1 Path and identity

A task path is:

```text
<tasks-directory>/<optional-subdirectories>/<ULID>.md
```

Writers for both supported versions create:

```text
Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md
```

Task IDs MUST:

- Be valid 26-character Crockford Base32 ULIDs.
- Be uppercase in filenames emitted by the CLI.
- Be unique by case-insensitive basename across the recursive tasks tree.
- Be generated locally without coordination.
- Never be stored redundantly in front matter.

A task move within the tasks directory does not change its identity or require rewriting children's parent IDs. The CLI does not perform task moves in either supported version.

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
base: "[[Todo/todos.base]]"
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
| `parent` | quoted string | no; v2 only | Full ULID of one task in the same store; omission means root. In v1 this spelling remains an unknown property. |
| `url` | string | no; v1 and v2 | Absolute HTTP or HTTPS link with a nonempty host; null is accepted as absent and canonical writers omit absent values. |
| `due_date` | date scalar | no | Local calendar date in `YYYY-MM-DD`. |
| `due_time` | string | no; v1 and v2 | Minute-granularity local civil time `HH:MM` (00:00–23:59), requiring `due_date`; null is accepted as absent. |
| `recurrence` | string | conditional | Supported RRULE subset. |
| `recurrence_from` | string | conditional | `schedule` or `completion`. |
| `last_completed_date` | date scalar | no | Most recent completion date for a recurring series. |

The body after the closing delimiter is the task's `body` in API and JSON output.

CLI-created tasks MUST contain a string property named `base` whose value is an Obsidian wikilink to the generated `todos.base`. The link target MUST include `obsidian_link_prefix` when it is nonempty. This integration property is returned in `extra_properties` and follows the unknown-property preservation rules in section 9.9; externally authored tasks without it remain valid.

The `url` key is reserved in both versions and decoded into the logical task, never duplicated in `extra_properties`. Validation MUST reject nonstring/non-null values, non-web or relative URLs, missing hosts, whitespace/control characters, backslashes, raw RFC-invalid punctuation (`<`, `>`, `"`, `{`, `}`, `|`, `^`, and backtick), malformed percent escapes, malformed bracketed IPv6 hosts, and empty/nondecimal/out-of-range ports (0–65535). Percent-encoded ASCII host whitespace, controls, and authority delimiters are invalid; encoded spaces in a path are valid. Schemes are case-insensitive. CLI add/edit trims surrounding whitespace explicitly before validation; record parsing MUST NOT trim or normalize URL spelling. All remaining spelling, including host case, escapes, query, and fragment, MUST be preserved. Invalid values fail with `invalid_url`, exit 5, and `field: url`, without mutation. The CLI MUST NOT fetch or open links.

Completion, recurrence advancement, state changes, reparenting, attachments, and unrelated edits MUST retain a task's URL. Child tasks MUST NOT inherit their parent's URL.

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

Obsidian and community plugins allow arbitrary properties. Task mutations in both supported versions MUST preserve unknown front-matter keys and their YAML values semantically.

Rules:

- Core key names are reserved and case-sensitive for the selected store version. `parent` is reserved only in v2; v1 MUST preserve an arbitrary safely representable `parent` extra unchanged in meaning, including null, strings, lists, and mappings. It MUST NOT decode that extra as a typed relationship or remove it on a metadata edit.
- Duplicate YAML keys are invalid, including duplicate unknown keys.
- Unknown scalar, list, and mapping values MUST survive a known-field mutation.
- YAML comments and original formatting inside front matter SHOULD be preserved when the selected YAML editing library supports it, but comment preservation is not a v1 correctness requirement.
- If the parser cannot safely represent and re-emit an unknown value, the CLI MUST refuse to mutate the file rather than delete or coerce it.
- YAML custom tags, merge keys, and cyclic aliases MUST be rejected.
- YAML parsing MUST use a safe data-only mode and MUST NOT instantiate application objects from YAML tags.

Writers MUST emit present core properties first in this order, followed by unknown properties in their existing relative order (`parent` is a core field only in v2):

```text
name
state
projects
tags
parent
url
due_date
due_time
recurrence
recurrence_from
last_completed_date
```

Property order is presentational only. `validate` MUST accept any property order.

### 9.10 Parent relationships (v2)

The optional `parent` value MUST be a YAML string containing a full valid 26-character ULID, with the same Crockford alphabet and leading-digit/overflow bounds as task IDs. Reads accept either ASCII case and normalize the logical value to uppercase; canonical writers MUST quote and uppercase it, including all-digit ULIDs. Quotation is required of canonical output, not of an input scalar already resolved as a string.

Only omission means root. Explicit null, empty string, nonstring values, whitespace-padded values, prefixes, names, paths, wikilinks, child lists, or root sentinels MUST fail with `invalid_parent_id`. Do not coerce a numeric YAML value into a string or treat a malformed value as absent.

Each parent MUST resolve to exactly one physical task identity in the same recursive task tree. A task may have one parent and arbitrary depth; it MUST NOT parent itself or form a cycle. Terminal parents and children are allowed. No parent lookup may escape the store or infer identity from folder placement. Duplicate case-normalized IDs invalidate identity resolution; never select an arbitrary duplicate as the parent.

Parent shape validation is separate from graph validation. Well-typed missing/self/cyclic edges MUST remain representable for inspection and explicit repair; malformed YAML or parent values retain the existing safe failure/no-rewrite policy. Missing references do not turn into roots. Derive parent/child indexes and diagnostics from a complete identity snapshot, without persisting child arrays or retaining excluded bodies unnecessarily. Use iterative traversal so valid depth is not limited by the call stack.

All other fields remain independent per task: names, bodies, state, projects, tags, dates, recurrence, and completion metadata are never inherited. Parent relations point to stable task/series identities, not historical occurrences. Hierarchy is organization, not a dependency system or recurring checklist template.

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

Optional `due_time` in both store versions is a local civil time, serialized quoted as exactly `HH:MM`. It MUST require `due_date`, reject nonstring/non-null YAML and invalid hours/minutes, and reject seconds/subseconds in typed API values. Absent or null time is logically absent and canonical writers omit it. The key is reserved, not unknown metadata. Its meaning is independent of timezone storage; no timezone is persisted and no alarm is scheduled. Existing date-only recurrence calculations, filtering, overdue checks, and sort precedence remain unchanged.

All lifecycle operations, recurrence advancement, attachments, and unrelated edits MUST preserve a present time; children MUST NOT inherit it. Clearing the due date MUST clear the due time. Clearing only the time MUST retain the date. `input --today` uses local midnight on the supplied date as its injected reference timestamp, choosing the earlier occurrence of an ambiguous midnight and rejecting a nonexistent midnight; without the override input resolves each line from the local current timestamp.

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

The v2 parent invariant applies to every mutation entry point, not only parent flags. Changes MUST NOT introduce or worsen graph faults. Unrelated operations and explicit repairs MUST remain possible when other well-typed graph faults already exist; a global pre-edit graph rejection MUST NOT prevent detaching the offending edge. Malformed records, duplicate identity, unsafe paths, and concurrent changes retain their existing safe failure policy.

### 13.1 Add

```text
otodo add <name> [options]
```

Options:

```text
--state <state>
--project <slug>            repeatable
--tag <tag>                 repeatable
--parent <full-id>          v2 only
--url <http-or-https-url>
--due-date <YYYY-MM-DD>
--due-time <HH:MM>          requires --due-date
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
- Without `--parent`, create a root in v2. With it, validate the full-ID destination and its prospective ancestry under the operation lock immediately before writing. The destination may be terminal or itself a child; no metadata is copied from it.

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
--parent <full-id>          v2 only; direct children
--roots                    v2 only; no parent key
--query <text>              literal case-insensitive name/ID substring
--summary                  compact task projection
--limit <N>                1..1000; requires --summary
```

Default behavior excludes tasks in terminal states but remains flat: matching children appear even when their parent is terminal or fails another filter. `--parent` and `--roots` are mutually exclusive; neither changes terminal filtering, so use `--all` to include terminal children. `--parent` requires a full ULID and selects direct children only; a missing selected parent is an error, not an empty root set.

Filters combine with AND except repeated `--state`, which is an OR set. `--overdue` means a nonterminal task with `due_date < today`.

`--query` treats all characters literally, not as regex, query syntax, paths, or shell input. Match a case-insensitive substring of the task name or full ID; empty query matches every otherwise eligible task. Apply all predicates to individual tasks, then the existing deterministic sort, then the optional limit. `--limit` without `--summary`, noninteger/out-of-range limits, and contradictory flags are usage errors. `--summary` without a limit returns all matching compact rows and `has_more: false`. Compact queries work in both supported store versions; v1 summary parents are null.

Default sort:

1. Tasks with due dates before tasks without due dates.
2. `due_date` ascending.
3. Configured state order.
4. Unicode code-point order of `name`.
5. Full task ID.

Human output SHOULD show an unambiguous ID prefix, state, due date, recurrence indicator, projects, and name. Human output is not a stable scripting interface.

Before filtering, projection, sorting, or limiting, `list` MUST parse and validate the complete store's task identities, records, project references, and (v2) graph. Any invalid record or graph fault makes the command fail; limits MUST NOT hide errors or turn failure into partial success. Users run `validate` to see all errors. Ordinary full-list JSON stays unchanged except for the additive nullable `parent` field; only `--summary` uses compact rows and truncation metadata.

### 13.3 Show

```text
otodo show <id-or-prefix>
```

Human output shows all known properties, unknown properties, source path, and body. JSON output returns the normalized logical model plus unknown properties in a separate object.

Well-typed graph faults MUST NOT make the affected task uninspectable: `show` returns its stored normalized parent rather than silently clearing it or failing solely because that edge is missing/self/cyclic. `validate` provides full graph diagnostics. Record syntax/type, identity, project, and safety checks remain unchanged.

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
--parent <full-id>          v2 only; set or replace parent
--clear-parent             v2 only; omit parent
--url <http-or-https-url>    set or replace link
--clear-url                omit link
--due-date <date>
--clear-due-date
--due-time <HH:MM>          requires an existing or simultaneously supplied date
--clear-due-time
--recurrence <rule>
--recurrence-from <mode>
--clear-recurrence
--body <text>
--body-file <path|->
```

Rules:

- At least one change is required.
- Conflicting changes to the same field are usage errors.
- `--url` and `--clear-url` are mutually exclusive. Clearing an already absent URL is valid; an empty `--url` is invalid, not a clear operation.
- `--due-time` conflicts with `--clear-due-time` and `--clear-due-date`. Clearing a date also clears its time; clearing a time alone retains the date.
- Removing a missing project or tag is a domain error rather than a silent no-op.
- Adding an existing project or tag is a domain error rather than producing a duplicate.
- `--clear-due-date` is invalid while recurrence remains configured.
- `--clear-recurrence` removes `recurrence`, `recurrence_from`, and `last_completed_date` together. It leaves `due_date` unchanged unless `--clear-due-date` is also passed.
- Adding recurrence to a non-recurring task requires an existing or simultaneously supplied due date and an explicit recurrence mode.
- Metadata-only edits preserve the body exactly.
- All changes to one task are validated and written as one atomic replacement.
- `--parent` and `--clear-parent` are mutually exclusive. Resolve a full-ID destination, exclude self/descendants, and validate the effective post-edit ancestry against the freshest store, rather than trusting a prior picker/show result. Setting a parent requires valid resulting ancestry; clearing an edge is an explicit repair even if unrelated faults remain. Reparent/detach changes only the selected task file, preserving every other field and its actual path. Setting the existing parent or clearing an already absent parent may succeed without creating a second edge.

`edit` does not open an interactive editor. Obsidian or a text editor remains the record-editing surface; section 28 authorizes a focused input TUI in both versions.

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

In v2, complete, direct state edits, finish-series, cancel, and reopen affect ONLY the selected task and preserve its parent. There is no child-state prerequisite for completion and no automatic parent completion. Children of terminal parents remain ordinary active tasks. Recurring parent and child series advance independently under section 12: never clone/reset/reopen relatives, inherit dates, or create subtree occurrences. Preserve current independent recurrence advancement in both Rust and Swift completion paths.

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
- In v2, scan all direct inbound parent references, including terminal children, before deleting. If any exist, fail with `task_in_use` and sorted canonical child IDs; make no changes. The user must explicitly detach, reparent, or remove each child first. There is no force, cascade, or delete-and-detach mode.
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
Children count once as ordinary tasks; project membership and counts are never inherited from parents.

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

Every parent argument (`add --parent`, `edit --parent`, and `list --parent`) requires a full 26-character ULID, unlike the selected task's existing `<id-or-prefix>` syntax. Normalize valid ASCII case; reject prefixes, paths, names, empty strings, or invalid ULIDs as `invalid_parent_id`. A syntactically valid selected destination with no matching task uses existing `task_not_found`; a stored edge to an absent identity uses `missing_parent_reference`. Duplicate physical IDs remain `duplicate_task_id`, not an arbitrary match.

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
- Optional URL type and absolute HTTP(S)/host validation in both versions.
- Optional `due_time` type, exact HH:MM syntax and required date in both versions.
- Project link syntax and referential integrity.
- Conditional recurrence fields.
- Safe unknown property values.
- In v2, parent scalar/ULID shape, same-store existence, self references, and cycles across all tasks, including terminal and filtered-out records.

Graph analysis MUST be a second pass over safely discovered identity/edge facts. Preserve physical identity facts even when a record has a separate content error; an existing malformed parent is not a missing file. Do not invent graph nodes from fallback IDs used to accumulate other diagnostics. Report duplicate identities before resolving ambiguous edges; suppress derivative missing/self/cycle claims that depend on choosing a duplicate. Other independently discoverable errors MUST still be reported.

For graph diagnostics, a self edge yields `self_parent_reference` only. A cycle of two or more nodes yields `parent_cycle` on each cycle member, not every descendant that leads into that cycle. Missing edges yield `missing_parent_reference` on the referencing child. Diagnostics attach the child path and `field: parent`, with deterministic path/field/location/code ordering. Keep graph faults representable; only explicit user edits repair them.

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

The CLI MUST NOT run Git commands. Obsidian Git or another external tool may commit, pull, merge or rebase, and push independently during ordinary operations. The explicit schema upgrade/resume is an exception requiring quiescence under section 7.3.

The CLI MUST tolerate commits that mix todo files with unrelated vault files. No functional behavior may require one operation per Git commit.

### 18.2 Advisory process lock

Mutating CLI commands MUST acquire an advisory exclusive lock on the open `.todo/config.toml` file for the duration of the operation. Read-only scans SHOULD acquire a shared lock. This coordinates cooperating `otodo` processes without creating a lock file that Obsidian Git could commit.

Other editors and Git do not honor this lock, so it is not sufficient by itself.

After acquiring an operation lock, new clients MUST recheck on-disk config and schema contents and the current config path's file identity against the generation loaded by `Store::open`. If either changed, fail as a concurrent modification before using cached config/paths or writing. A reopened unsupported or partial pair must not become a bypass around schema gating. Perform equivalent checks at the final publication boundary. This applies to stores opened before another process replaced config/schema, including clients waiting on an old config inode. It does not retrofit this behavior into old binaries; those writers MUST be stopped for upgrade.

### 18.3 Optimistic concurrency

Before mutating an existing file, the CLI MUST:

1. Read the complete source bytes and file metadata.
2. Compute a cryptographic content hash.
3. Parse and validate the source.
4. Prepare and validate the replacement.
5. Immediately before replacement, reopen and rehash the current source.
6. Abort with a concurrent-modification error if the hash differs.

It MUST never use last-writer-wins after detecting an external change.

V2 relation-sensitive operations MUST additionally snapshot and recheck the identity/edge dependencies used to validate their result immediately before publication. Add/reparent depends on destination existence and ancestor edges; delete depends on the absence of all direct inbound children. Detect additions, removals, moves, duplicate IDs, and changed edges, not just changes to files already in a hash set. A compact whole relation snapshot/re-scan is permitted; no persistent index or general transaction manager is required. Observed changes abort with the existing concurrent-modification class, preserve external bytes, and do not partially publish the task.

These checks provide optimistic race detection, not a serializable filesystem snapshot against noncooperating editors. Another file can change after the final check; validation and external synchronization must expose resulting faults rather than promising impossible multi-file atomicity. Normal parent edits and lifecycle operations remain single-task writes.

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

Different-file changes can also combine into a missing reference or cycle. Such well-typed graph faults are explicit relationship diagnostics, not fabricated same-path conflicts; they remain visible and repairable. No import or reconciliation may silently delete descendants, clear parent IDs, or discard pending/conflict bytes to make a graph appear valid. A malformed record continues to fail import safely, retaining the previous durable state rather than silently skipping it.

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
- In v2, quote canonical uppercase parent ULIDs, place `parent` immediately after `tags`, and omit it for roots. V1 unknown `parent` properties retain unknown-property ordering and meaning.
- Quote a present URL after `parent` (after `tags` when no typed parent) and before `due_date`; omit absent URLs.
- Serialize dates as `YYYY-MM-DD`.
- Quote a present `due_time` immediately after `due_date`; omit absent times.
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
  "parent": null,
  "url": null,
  "due_date": "2026-09-06",
  "due_time": null,
  "recurrence": "FREQ=WEEKLY;INTERVAL=1;BYDAY=SU",
  "recurrence_from": "schedule",
  "last_completed_date": "2026-08-30",
  "body": "Review transactions.\n",
  "extra_properties": {}
}
```

Absent optional values MUST be JSON `null`, not omitted, in normalized task output. Arrays are always present.
In v2 a child returns its canonical full parent ULID; a root returns null. In v1 normalized `parent` is always null, while any legacy key of that spelling stays in `extra_properties` with its original YAML meaning. CLI JSON envelope version `1` is independent of store schema versions and of client-local cache envelopes.
Normalized task JSON always includes `url` as the original validated string or null, and `due_time` as `HH:MM` or null, in both store versions. Compact `--summary` projections remain unchanged.

List output:

```json
{
  "version": 1,
  "tasks": []
}
```

`list --summary --query TEXT --limit N` returns exactly the compact row fields below and an explicit truncation flag; bodies, arbitrary properties, and inherited fields are not included:

```json
{
  "version": 1,
  "tasks": [
    {
      "id": "01K4B0ZSBZZV25T1K0D3TA8JHR",
      "path": "Tasks/01K4B0ZSBZZV25T1K0D3TA8JHR.md",
      "name": "Review weekly finances",
      "state": "open",
      "terminal": false,
      "parent": null
    }
  ],
  "has_more": false
}
```

`has_more` is true exactly when additional matching tasks remain beyond the limit after full-store validation and sorting. Returning exactly N rows alone does not imply truncation.

`otodo --format json capabilities` succeeds without a root, config, environment-selected store, or filesystem discovery and returns:

```json
{"version":1,"store_schema_versions":[1,2],"features":["subtasks","task_candidates","store_upgrade","attachments","task_urls"]}
```

This describes the executable, not whether the user's current store enables parent operations. Unsupported stores and legacy-v1 parent operations still fail explicitly; callers MUST NOT silently retry a rejected child creation as a root.

`upgrade --to 2` JSON success is `{"version":1,"upgrade":{"from":1,"to":2,"dry_run":false,"status":"upgraded"}}`. `status` is `planned` for a valid dry-run, `resumed` for a completed intermediate-pair resume, or `already_current` for a coherent v2 no-op (`from:2`). A dry-run uses `dry_run:true`; failures use the normal error envelope and no success object.

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

Parent/version error codes:

| Code | Exit | Meaning |
|---|---:|---|
| `invalid_parent_id` | 5 | Present parent value or parent argument is not a full valid string ULID. |
| `invalid_url` | 5 | URL type or HTTP(S)/host syntax is invalid; includes `field: url`. |
| `invalid_due_time` | 5 | Invalid stored/API due time type, precision or range; includes `field: due_time`. Invalid explicit CLI flag values are usage errors. |
| `due_time_requires_due_date` | 5 | A due time has no effective due date; includes `field: due_time`. |
| `invalid_input` | 5 | Input is not single-line UTF-8 or contains control characters. |
| `input_too_large` | 5 | A raw stdin line or queued TUI paste exceeds 16 KiB. |
| `invalid_input_date` | 5 | The selected `--today` has no local midnight for input capture. |
| `unknown_input_command` | 2 | A leading slash token is not a supported input command; includes `field: input`. |
| `invalid_input_filter` | 2 | A `/list` argument is not a `#project`, `@tag`, or nonempty `!state` token; includes `field: input`. Project/tag validation and unknown configured states retain their existing error codes. |
| `missing_parent_reference` | 5 | A stored, well-typed parent ID has no physical task in this store. |
| `self_parent_reference` | 5 | Canonical child and parent IDs are equal. |
| `parent_cycle` | 5 | The task participates in a cycle of two or more nodes. |
| `task_in_use` | 5 | Deletion refused because direct children still reference the task. |
| `unsupported_schema` | 7 | Unsupported store/upgrade version, or a parent feature requested on legacy v1. |

Parent errors SHOULD include child `path` and `field: parent` when available. Existing `task_not_found` (3), `duplicate_task_id` (5), usage (2), concurrent-modification (6), schema-mismatch validation, and aggregate validation conventions remain in force. An upgrade's legacy-parent collision is a validation failure (5) identifying `parent` and the affected path; it MUST NOT mislabel an arbitrary legacy value as a malformed v2 record.

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
- Body-file and attachment-source reading are explicitly user-requested and may access outside the store; no other operation may do so.
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
- Shared v1/v2 parent corpus: omission, case normalization, numeric ULID quotation, null/type/path/prefix/overflow refusal, legacy unknown-parent preservation, and exact untouched bodies.
- Graph identity ambiguity, missing/self/multiple cycles, entering descendants, terminal parents, deep iterative traversal, and explicit repair without suppressing unrelated diagnostics.

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
20. Verify failed ordinary mutations leave no committed replacement and no persistent temporary file; explicit upgrade interruption follows section 7.3's fail-closed partial-state contract.
21. Add/reparent/detach with full IDs under nested and terminal parents; verify independent metadata, stable paths, and unrelated task bytes.
22. Refuse deletion with any direct child, including terminal children; allow explicit repair and later leaf deletion.
23. Prove every lifecycle and both recurrence modes preserve parent links and never change relatives.
24. Prove excluded records cannot hide a graph error from full or compact list; verify literal query/limit boundaries, sort, and exact `has_more`.
25. Detect parent/ancestor changes, newly added children, and stale config/schema generations at the operation's snapshot boundaries.
26. Exercise explicit v1/v2 support, no auto-upgrade, arbitrary v1 parent extras, collision refusal, dry-run, each resumable cutover state, unsupported/reversed pairs, unchanged record bytes, and custom Base preservation. Failed ordinary mutations remain no-write; an interrupted upgrade may leave only the documented fail-closed pair.
27. Prove rootless capabilities ignores unusable store discovery inputs and unsupported versions still fail before mutation.

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

### Scenario K: Independent subtasks

Create a terminal parent, an active child, and a grandchild in a v2 sparse store. Move the parent manually within the recursive task tree. The relation still resolves by ULID; default list shows the active child; roots/direct-child filters are explicit. Complete or recur one task without changing any relative. Deleting either parent with direct children refuses; explicit detach/reparent repairs the graph without rewriting relatives.

### Scenario L: Invalid graph and repair

Externally introduce an orphan, a self edge, and a cycle in otherwise readable records. `validate` reports all independently discoverable relationships; even bounded queries fail rather than hiding them. `show` preserves the faulty parent ID. Explicit clear/reparent repairs the selected edge while unrelated faults remain discoverable. Malformed records still fail safely without rewriting or silent omission.

### Scenario M: Explicit format activation

A new client edits a legacy v1 task containing arbitrary `parent` metadata without interpreting or losing it. Parent feature operations fail. After users explicitly handle every parent-key collision and quiesce writers, dry-run makes no changes, upgrade changes only config/schema, and a stopped intermediate cutover rejects normal writes until explicit resume completes. Every existing task/project and customized Base stays byte-identical. Newly launched old clients reject the completed v2 format; new clients reject stale generations. No claim is made that old already-running writers are safe without quiescence.

## 25. Recommended implementation order

The existing v1 implementation is the baseline; preserve its unrelated behavior rather than recreating the package. Implement the extension in dependency order:

1. Freeze v1/v2 schemas, external parent/JSON/error semantics, and shared conformance fixtures.
2. Add schema-aware codecs/models and pure iterative relation analysis, including legacy unknown-key preservation and representable graph faults.
3. Integrate tolerant validation, locked single-task operations, explicit repair, deletion guards, and config/schema/relation snapshot checks.
4. Add CLI parent flags, flat filters, compact candidates, rootless capabilities, and v2 initialization/Base generation.
5. Add quiesced explicit upgrade/dry-run/resume without record rewrites or custom Base loss.
6. Integrate cross-client compatibility and actual publication-subset safety, then execute focused behavior proofs and the repository's complete quality gate.

Each stage must be production behavior, not a stub. Do not expose a command until its invariants, error handling, JSON contract, and behavioral tests are complete.

## 26. Definition of done

The v2 extension with explicit legacy-v1 support is done when:

- Every product goal and acceptance scenario is implemented.
- Every non-goal remains absent rather than partially scaffolded.
- The library and CLI compile on stable Rust on Linux.
- The specified unit, integration, CLI, sparse-checkout, concurrency, and recurrence tests pass.
- `cargo fmt --check` passes.
- `cargo clippy --all-targets --all-features -- -D warnings` passes.
- `cargo test --all-targets --all-features` passes.
- The CLI demonstrably operates on an initialized store with no Git executable, no `.git`, no `.obsidian`, and no network.
- No normal operation changes files outside the selected store; only explicitly supplied body files and attachment import sources may be read externally.
- Obsidian-created unknown properties survive known-field mutations.
- Malformed or conflicted input produces an actionable error and no data loss.
- JSON output and exit codes match this specification.
- Legacy flat stores remain writable without interpreting their parent extras; v2 activation is explicit, fail-closed/resumable, and quiesced.
- Subtask mutations, independent lifecycle/recurrence, graph repair, candidate discovery, and snapshot-race behavior satisfy the shared contract and conformance corpus.

Any implementation that silently loses unknown properties, overwrites a concurrent edit, depends on Git history, mutates unrelated vault content, or accepts unsupported recurrence rules as if valid is incorrect.

## 27. Ordinary file attachments (store schemas 1 and 2)

Attachments MUST NOT require a store upgrade, new config key, or frontmatter property. A custom property named `attachments` remains unknown user metadata and MUST survive all attachment operations. CLI JSON stays at version 1 and rootless executable capabilities advertise `attachments` independently of `subtasks`, `task_candidates`, and the selected store schema.

Files MUST live below fixed `Attachments/`. Each import MUST allocate a fresh ULID directory, sanitize a UTF-8 filename, and copy bytes unchanged. Each file MUST be at most 20 MiB (20 × 1024 × 1024 bytes), inclusive. `Attachments/` is created on first import. Imports read only explicitly selected external source files, reject symlinks and nonregular sources, and stage all inputs before publishing any task. Filenames retain Unicode; controls and `/\:*?"<>|` become `_`, leading/trailing whitespace and dots are trimmed, empty names become `attachment`, and names are bounded to 200 UTF-8 bytes.

Canonical associations are relative Markdown links in a task body, generated from the task's actual parent directory. Images use embeds. Generated paths percent-encode non-ASCII and reserved bytes; labels escape Markdown brackets/backslashes. Inline Markdown links/images and explicit Obsidian wikilinks/embeds into `Attachments/` MUST be recognized, including manually placed files and nested tasks. Explicit `[[Attachments/...]]` paths are store-relative; an exact configured `obsidian_link_prefix` before `Attachments/` is also recognized for vault-qualified wikilinks; other relative paths resolve from the task file. Fragments and aliases do not change association identity. Fenced, indented, and inline code examples MUST be ignored. Shortened/ambiguous/unsupported attachment links remain untouched; diagnostics explain that an explicit path is required. References are deduplicated by normalized store-relative file path. The shared corpus is `tests/fixtures/attachments/links.json`.

Selectors MUST be explicit normalized store-relative `Attachments/...` file paths. Absolute paths, traversal, symlink components, and nonregular targets MUST be refused. Missing targets MUST be reported without blocking ordinary task editing. If `Attachments/` overlaps configured task/project paths, only attachment operations are disabled; ordinary todo functionality remains available and tolerant validation emits a warning.

Required commands:

```text
otodo add <name> --attach <source> [--attach <source> ...]
otodo attachment add <task-id> <source> [<source> ...]
otodo attachment link <task-id> Attachments/<path>
otodo attachment list <task-id> --format json
otodo attachment unlink <task-id> Attachments/<path>
otodo attachment path <task-id> Attachments/<path>
```

Attachment add/link/list/unlink results contain `version: 1` and an `attachments` array. Each result contains `path`, `display_name`, `byte_size` (null when missing), and `availability` (`available` or `missing`). `attachment path` returns the existing linked file's absolute path; JSON uses `version: 1` and `path`. Existing task outputs MUST remain unchanged. Link is idempotent. Unlink removes all recognized occurrences from that task without changing unrelated body bytes; it never deletes files. Completing, recurring, or deleting a task MUST retain stored files. Multiple tasks may share one file; children inherit none.

Attachment publications MUST use existing store locks, generation checks, and exclusive-create writes. Files publish before the task, and the task publishes once with its source snapshot and existing relationship checks. Failure before task publication MUST NOT create a task. Files may remain unreferenced after interruption or a later failed task write; no multi-file filesystem atomicity is promised. `attachment_source_invalid`, `attachment_too_large`, `unsafe_attachment_path`, and `attachments_disabled` from new-task imports guarantee that task publication did not occur. Clients MUST treat I/O and unrecognized errors as uncertain and MUST NOT blindly retry them.

The explicit v1→v2 upgrader MUST preserve attachment bytes and task links through dry-run, upgrade, resume, and already-current no-op. Targets other than 2, including 3, remain unsupported. CLI Git operations remain forbidden. Desktop synchronization and sparse checkouts MUST include `Attachments/`. Documentation MUST explain Obsidian attachment-location settings without changing vault-wide preferences. File deletion, orphan cleanup, camera capture, and scanning are outside this release.

## 28. One-line task and command input

```text
otodo input
otodo --format json input
```

Human input with terminal stdin/stderr and a non-dumb `TERM` MUST open a focused terminal UI with an editable line, parsed preview, completion suggestions, list results, and save/error status. Other input, including JSON mode, MUST read stdin as one task or slash command per line without terminal control sequences. No editor, shell, subprocess, Git command, network request, history file, or hidden database may be invoked/created by this mode.

For task capture, `#slug` and `@tag` MUST be recognized only as whole whitespace-delimited metadata tokens and removed from the name. Projects MUST already exist, using existing slug/reference validation; tags MAY be new and MUST retain case, Unicode and nested `/` spelling. Repeated exact metadata values MUST be deduplicated. Name case and Unicode MUST remain unchanged, with redundant whitespace collapsed and existing name validation applied after removing metadata/date phrases. Default state, root-parent behavior, URL validation, locking, generation checks and contained create-new publication MUST use the normal task-creation operation for every task line.

A first non-whitespace token beginning with `/` MUST dispatch a command, never create a task. The initial supported command is the exact lowercase token `/list`, optionally followed by whitespace-delimited `#project`, `@tag`, and `!state` filters. `/list` MUST include all tasks, including terminal states, unless excluded by explicit filters. Projects and tags combine with AND; repeated states form an OR set. Exact duplicates MUST be deduplicated. Project/tag spelling and configured state IDs MUST use existing list validation; unknown tags may yield no matches. `/list #personal @chores !open` selects open chores in personal. Commands MUST NOT parse dates or treat bare text as a name/query. Unknown slash commands and malformed arguments MUST fail without mutation; slashes and `!state` inside ordinary task names remain literal text. Listing MUST use the complete-store validation, locking, and deterministic sort of section 13.2 in both supported store versions, without changing ordinary `otodo list` defaults.

Natural date recognition MUST be case-insensitive and support:

- `today`/`tod`, `tomorrow`/`tom`.
- Full weekday names and `sun`, `mon`, `tue`/`tues`, `wed`, `thu`/`thur`/`thurs`, `fri`, `sat`; resolve to the strictly next occurrence, including seven days ahead on that weekday.
- `next week`, `next month`, and `in N day(s)/week(s)/month(s)` with positive decimal integers, calendar arithmetic, and month-end clamping.
- 24-hour `HH:MM` and 12-hour `h[:MM] am/pm` with optional spacing and leading `at`. Time alone supplies local today even when that clock has passed.
- `in N hour(s)/minute(s)` as elapsed local-timezone-aware arithmetic, including DST transitions, rounded upward to minute precision.

The last date and last time MUST win independently. Only contributing phrases and adjacent separator punctuation/whitespace are removed; superseded phrases remain name text. Invalid/unsupported phrases, including ISO date literals, MUST remain name text rather than being partially consumed. Resolved calendar overflow MUST fail. Dates/times embedded in metadata, URLs, emails, paths and identifiers MUST NOT be consumed. The first valid explicit absolute HTTP(S) URL MUST also populate `url`, preserving its spelling and retaining the URL in the title. Sentence punctuation and unbalanced closing delimiters are excluded from the captured field; balanced URL punctuation, query strings and fragments are retained. Invalid candidates remain name text; no bare-domain guessing occurs.

Suggestions MUST use existing project slugs and tags across all tasks, including terminal tasks, with case-insensitive prefix matching and original spelling on acceptance. They MUST also complete `/list` in the first token and configured `!state` IDs in `/list` arguments. Up/Down selects and Tab accepts the whole token at the cursor without corrupting surrounding Unicode text. New saved tags MUST be immediately available; F5 explicitly refreshes the suggestion catalog from disk. Store locks MUST NOT remain held while waiting for keyboard input. Enter submits exactly the current line; failed validation retains the draft for correction. Successful `/list` commands MUST display results without incrementing the created count, and PgUp/PgDn MUST allow every result row to be reached. Successful submissions advance to the next queued draft or clear the line; list results remain visible when suggestions are not displayed until replaced by another successful command or task save. Multiline paste MUST queue drafts for individual Enter confirmation. Esc/Ctrl-C exits and discards unsaved drafts; Ctrl-D exits when all drafts are empty. Successful saves remain committed. Terminal raw mode, bracketed paste, cursor visibility and alternate-screen state MUST be restored on normal/error exits. I/O, concurrency and unsupported-generation errors MUST exit rather than invite an uncertain creation retry.

Plain stdin MUST accept UTF-8 LF/CRLF lines and a final unterminated line, skip blank lines, and bound each raw line to 16 KiB. TUI input and queued paste together are bounded to 16 KiB. Pasted tabs MUST normalize to spaces and CRLF/CR to line breaks; other control characters MUST be rejected without losing the draft. Plain input MUST stop at the first failure; earlier saves remain durable and later lines are not processed. Parser/metadata errors MUST identify the stdin line in structured diagnostics. Each task save goes to stdout as an ID/name line or a version-1 `task` JSON envelope. Each `/list` success MUST use the ordinary human list output or a version-1 `tasks` JSON envelope, including an empty array when no tasks match. JSON mode is JSON Lines, one envelope per submitted nonblank line, with no summary document or progress noise. Human TUI exits with a created-task count on stdout; its interactive surface uses stderr.

Rootless executable capabilities MUST include `task_input` and `task_due_times`. Tests MUST defend requested examples, URL/title preservation, metadata and date/time boundaries, precedence, local/DST arithmetic, Unicode completion/editing, rejected-draft preservation, partial-stream failure, both store versions, time lifecycle/clear invariants, and unchanged schema/configuration bytes. Slash-command coverage MUST defend combined filters, terminal-state inclusion, read-only behavior, unknown-command/filter failures, mixed capture/list streams, and complete-store validation before filtered results.
