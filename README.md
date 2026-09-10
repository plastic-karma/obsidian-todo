# obsidian-todo

`obsidian-todo` is a local-first Rust library and `otodo` CLI for managing structured tasks inside an existing Obsidian vault. Tasks and projects remain human-editable Markdown files with YAML front matter—no database or server. Ordinary commands are offline and Git-free; only explicit `/sync` in `input` invokes Git.

## Features

- Workflow states, projects, tags, local due dates/times, and recurrence
- Optional HTTP/HTTPS task links in both store versions
- Interactive one-line task capture with natural dates, completion, and filtered `/list` commands
- Explicit `/sync [ours|theirs]` to synchronize an existing Git branch with its upstream
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

The store contains `.todo/config.toml`, `.todo/schema.json`, `Tasks/**/*.md`, `Projects/*.md`, `todos.base`, and imported files below `Attachments/`. Ordinary commands leave other vault content untouched. Explicit `/sync` synchronizes the whole containing Git branch, not just the store.

## Interactive input

Open the task-entry TUI:

```sh
otodo --root Todo input
```

Enter one task per line:

```text
Call Plumber tom 9am #personal @chores
follow up with John about https://github.com/issues/124 #work @prs
```

The first saves `Call Plumber`, due tomorrow at `09:00`, in project `personal`
with tag `chores`. The second keeps the URL in the title and also sets the
task's `url`, project `work`, and tag `prs`. Title case and Unicode are preserved;
redundant whitespace is collapsed.

- `#slug` associates an **existing** project; it does not create one. Use
  `otodo project create personal --name Personal` first if needed.
- `@tag` adds a tag. New tags and nested tags such as `@home/chores` are allowed.
  Multiple projects/tags are supported; repeated identical values are deduplicated.
  Metadata must be separate whitespace-delimited tokens.
- Suggestions complete `/list` and `/sync` at the start of a line, `#projects`,
  `@tags`, configured `!states` and `due:` filters in `/list`, and
  `ours`/`theirs` in `/sync`.
  Use **Up/Down** to select and **Tab** to accept. Tags come from all tasks,
  including completed tasks and
  tasks saved in this session. **F5** reloads the suggestion catalog from disk.
- **Enter** submits the current line: save a task or run a slash command.
  A rejected line stays editable. Multiline paste queues drafts for review:
  press Enter for each, rather than submitting the entire paste immediately.
  Pasted tabs become spaces, and CRLF/CR become line breaks. Other control
  characters are rejected without changing the draft.
- **Left/Right**, **Home/End**, **Backspace/Delete**, and **Ctrl-U** edit the
  line. **Esc** or **Ctrl-C** exits; **Ctrl-D** exits when no drafts remain.
  Unsaved drafts are discarded; successful saves are retained.

The TUI uses a restrained, Omarchy-inspired palette: blue focus, violet projects,
teal tags, amber dates, and green/red save/error feedback. It inherits the
terminal's ANSI colors and background, so it follows your terminal theme without
reading desktop configuration. Selection markers and status labels also work
without color. Use `--color never` for an unstyled TUI; auto mode also respects a
nonempty `NO_COLOR`. `--color always` overrides that opt-out for the TUI, never
for piped input or JSON.

Date/time recognition follows `otodo-app`'s capture grammar, with `tom` added
as a tomorrow alias:

| Input | Meaning |
|---|---|
| `today`, `tod`, `tomorrow`, `tom` | Local today/tomorrow |
| `Monday`, `mon`, `tue`, `wed`, etc. | Strictly next occurrence of that weekday |
| `next week`, `next month` | One calendar week/month later |
| `in 3 days`, `in 2 weeks`, `in 1 month` | Calendar offsets; month-end is clamped |
| `9am`, `3:05 pm`, `at 14:30` | Local clock time; time alone uses today |
| `in 2 hours`, `in 15 minutes` | Elapsed time, including DST changes; rounds up to a minute |

Recognition is case-insensitive. The last date and last time win independently;
only contributing phrases are removed. URL/email/path text and metadata are not
parsed as dates. Unsupported expressions remain title text; use `add --due-date`
for explicit `YYYY-MM-DD` dates. The first valid explicit HTTP(S) link is captured;
surrounding sentence punctuation is excluded from the field, but remains in the
title. No links are fetched or opened.

`/list` queries the store without creating tasks:

```text
/list
/list #personal @chores !open
/list !open !active
/list due:today
/list due:tomorrow
/list due:overdue
/list due:none
/list #personal @chores !open due:today
```

Bare `/list` includes **all tasks, including completed and cancelled tasks**.
The second example returns open tasks tagged `chores` in project `personal`.
Projects and tags combine with AND; repeated states match any of those states.
Repeated identical filters are deduplicated. Projects/states must exist; tags
match exact spelling, including case and nested `/` names. Results use the
ordinary list validation and sort order.

Due filters combine with all other filters using AND:

| Filter | Matches |
|---|---|
| `due:today` | Tasks due on the local current date |
| `due:tomorrow` | Tasks due on the next local calendar date |
| `due:overdue` | Nonterminal tasks with a due date before today |
| `due:none` | Tasks with no due date, whether omitted or null |

`--today YYYY-MM-DD` overrides today for these filters. Due times do not affect
date matching. Use one distinct, lowercase `due:` selector per command; repeated
identical selectors are harmless, but different selectors together are errors.
Bare natural dates and other date expressions are not parsed inside commands.
**PgUp/PgDn** scroll results in the TUI; listing does not increase the saved count.

Commands are lowercase, whole tokens at the beginning of the line (leading
whitespace is allowed). Unknown slash commands are errors, never task titles.
`/list` accepts only `#project`, `@tag`, `!state`, and the four `due:` filters;
`/sync` accepts only an optional `ours` or `theirs`. Slashes and `!state` inside
ordinary task titles remain text.

### Explicit Git synchronization

In the input TUI, enter:

```text
/sync
/sync ours
/sync theirs
```

`/sync` fetches the configured upstream, stages and commits local changes **only
beneath the selected store** if any, merges the upstream, validates the merged
store, and pushes explicitly to that upstream branch. Fetch/merge/push synchronize
the **whole containing branch**, including already committed unrelated vault
paths. It is not a store-only publication filter. Ordinary commands—including
discovery, `init`, and `validate`—remain offline and never invoke Git.

Before syncing, stop all other writers and synchronization, including Obsidian
Git. Have Git installed, an existing attached branch with commits and a configured
upstream, and author identity/noninteractive authentication configured when
needed. Sync refuses detached or unborn branches, missing upstreams, pre-existing
merge/rebase/cherry-pick/revert operations or conflicts, and staged, unstaged, or
untracked changes outside the selected store. It does not initialize Git, change
Git configuration, force-push, stash, reset-hard, or rebase. Git hooks and
credential/editor prompts are disabled; configure authentication beforehand.
SSH runs in batch mode using SSH configuration/agents; `GIT_SSH_COMMAND`
overrides are not used.
Automatic sync commits are unsigned, and sync does not verify incoming commit
signatures. Use your external Git workflow when signature enforcement is required.
There is no automatic/background sync and no store schema change or hidden state.

**Ours means local; theirs means remote.** Both policies preserve nonconflicting
text edits and choose only conflicting hunks. With no policy, the TUI shows both
previews and lets you choose `o` or `t` per hunk. **Up/Down/PgUp/PgDn** scroll
both previews; **Left/Right** pan long lines. Binary, deletion, and file-type
conflicts use a whole-file choice; an absent side means deletion. File-mode
conflicts choose the local or remote mode. Submodule and file/directory collisions
require manual resolution: sync aborts its merge rather than deleting a tree or
publishing Git's temporary conflict paths. **Esc/Ctrl-C** cancels resolution.
No store lock is held across Git changes or conflict prompts; writer quiescence
is required, not enforced.

Cancellation or resolver failure aborts only the active merge started by this
invocation; any automatic local commit is retained. Sync is not transactional:
other failures can leave completed local steps, and a failed push retains local
commits and merge results. A failed validation can retain a completed fast-forward;
it never authorizes a push. Inspect the reported error before retrying. Before
accepting more TUI input, the store and suggestion catalog are reloaded, including
after cancellation. If reload fails, the session exits rather than use stale data;
the original sync failure takes precedence over any reload error. Plain input
stops immediately on sync failure.

Plain input and JSON cannot answer conflict prompts or use later task lines as
answers. Without a policy, an unresolved conflict reports `sync_conflict` and
stops input after aborting this invocation's merge. For scripted policy selection:

```sh
printf '%s\n' '/sync ours' | otodo --root Todo --format json input
```

Success is one JSON Lines envelope:

```json
{"version":1,"sync":{"branch":"main","upstream":"origin/main","committed":true,"conflicts_resolved":0}}
```

`committed` indicates an automatic local store commit, not a merge commit.
`conflicts_resolved` counts chosen hunks or whole-file fallback conflicts.
`capabilities` advertises `git_sync` without requiring Git or a store.

| Error | Exit | Meaning |
|---|---:|---|
| `invalid_sync_option` | 2 | Invalid `/sync` arguments; `field: input` |
| `sync_conflict` | 2 | Noninteractive conflict needs `ours` or `theirs` |
| `sync_cancelled` | 2 | TUI conflict resolution cancelled |
| `sync_precondition` | 5 | Unsafe repository state or missing prerequisites |
| `sync_unavailable` | 7 | Git executable unavailable |
| `sync_failed` | 8 | Git or synchronization I/O failure |

### Plain input and JSON

Pipes and `--format json` use plain stdin without the TUI:

```sh
printf '%s\n' \
  'Call Plumber tom 9am #personal @chores' \
  'Review PR https://github.com/issues/124 #work @prs' \
  '/list #personal @chores !open' |
  otodo --root Todo --format json input
```

JSON output is one version-1 `task` envelope per saved task, `tasks` envelope
per `/list` (including an empty array when nothing matches), or `sync` envelope
per successful `/sync` (JSON Lines). Git output does not leak into this stream.
Blank lines are skipped; LF, CRLF, and a final line without a newline work.
The first invalid line stops piped input with an error on stderr; earlier saves
remain durable and later lines are not processed. Input is UTF-8, bounded to
16 KiB per raw line or queued paste. No history file is written. Terminal control
sequences are used only for the human TUI, which requires terminal stdin/stderr
and a non-dumb `TERM`.

`--today YYYY-MM-DD` anchors capture to **local midnight** on that date, including
relative hours/minutes, for deterministic input. Without it, each submission
uses the current local timestamp.

### Stored due times

Times use the app-compatible additive `due_time` field in both store versions.
It is a minute-granularity local civil time, not a timestamp or reminder.
There is no schema upgrade or schema/configuration rewrite.

```sh
otodo --root Todo add "Call plumber" --due-date 2026-09-10 --due-time 09:00
otodo --root Todo edit <task-id> --due-time 14:30
otodo --root Todo edit <task-id> --clear-due-time
```

A time requires a due date. Markdown emits `due_time: "09:00"` immediately
after `due_date`; normalized task JSON includes `due_time` as `HH:MM` or null.
Clearing the date clears the time too. Recurrence advances the date and retains
the time; other lifecycle operations and unrelated edits retain it, and children
do not inherit it. Existing date-based filtering/sorting stays unchanged.
`validate` rejects malformed/orphan times; previously unknown `due_time`
metadata is now subject to this contract. Historical schema asset bytes and
customized Obsidian Bases remain untouched. `capabilities` advertises
`task_input` and `task_due_times`.

## Task links

Store an optional web link with a task:

```sh
otodo --root Todo add "Read proposal" --url 'https://example.com/proposal'
otodo --root Todo show <task-id> --format json
otodo --root Todo edit <task-id> --url 'https://example.com/revised'
otodo --root Todo edit <task-id> --clear-url
```

Links work in schema 1 and 2 without an upgrade or schema/configuration changes.
The existing extensible schemas remain byte-for-byte unchanged; the runtime
validates the additive `url` field. Use an absolute HTTP or HTTPS URL with a
nonempty host, no whitespace/control characters, and valid escapes/port syntax.
The CLI trims surrounding whitespace on explicit add/edit input, preserves all
remaining spelling, and never fetches or opens the link. Invalid values produce
`invalid_url` with `field: url` and no mutation. Setting and clearing together is
a usage error.

Task JSON includes `url` as a string or null. Markdown stores a quoted `url`
after `parent` and before `due_date`, omitting it when absent. Completion,
recurrence, attachments, and unrelated edits retain it; children do not inherit
their parent's link. Unknown YAML properties and untouched task bodies retain
their existing preservation guarantees.

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

To paste directly into this folder in Obsidian, open **Settings → Files and links → Default location for new attachments**, choose **In the folder specified below**, and select the vault-relative store folder, for example `Todo/Attachments`. This preference applies to the entire vault; OTodo never changes it. See [Obsidian's attachment documentation](https://help.obsidian.md/attachments). Ensure pasted links include an explicit path if Obsidian shortens them. Synchronization and sparse checkouts must include `Attachments/` alongside task files. Git may remain external, or you can invoke `/sync` explicitly under the safeguards above.
