use std::borrow::Cow;
use std::collections::{BTreeSet, VecDeque};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::ops::Range;
use std::path::Path;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::style::{Attribute, Color, ContentStyle, Print, SetAttribute, SetStyle, Stylize};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};
use serde_json::json;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use obsidian_todo::commands::task::{self, AddTask, TaskView};
use obsidian_todo::error::{Error, ErrorKind, Result};
use obsidian_todo::input::{parse_line, ParsedInput, ParsedTaskInput};
use obsidian_todo::model::Task;
use obsidian_todo::store::Store;
use obsidian_todo::sync::{self, ConflictChoice, SyncConflict, SyncResult};

use crate::output::{
    human_task_row, task_list_output, write_success, ColorChoice, CommandOutput, OutputFormat,
};

// Bound a line and a queued paste independently of the potentially much larger record body.
const MAX_INPUT_BYTES: usize = 16 * 1024;
const INPUT_PROMPT: &str = "│ › ";

pub fn run(
    store: Store,
    today: Option<NaiveDate>,
    format: OutputFormat,
    color: ColorChoice,
) -> Result<()> {
    let fixed_reference = today.map(reference_at_midnight).transpose()?;
    if format == OutputFormat::Human
        && io::stdin().is_terminal()
        && io::stderr().is_terminal()
        && std::env::var_os("TERM").is_some_and(|term| term != "dumb")
    {
        let created = run_terminal(store, fixed_reference, color)?;
        writeln!(io::stdout().lock(), "Created {created} task(s)")
            .map_err(|source| Error::io("write input summary", Path::new("-"), &source))
    } else {
        run_lines(
            store,
            fixed_reference,
            format,
            io::stdin().lock(),
            &mut io::stdout().lock(),
        )
    }
}

fn reference_at_midnight(date: NaiveDate) -> Result<DateTime<Local>> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight"))
        .earliest()
        .ok_or_else(|| {
            Error::validation(
                "invalid_input_date",
                "The selected date has no local midnight",
            )
            .with_field("today")
        })
}

fn input_too_large() -> Error {
    Error::validation(
        "input_too_large",
        "An input line or queued paste may contain at most 16 KiB",
    )
    .with_field("input")
}

fn create_task(store: &Store, parsed: ParsedTaskInput) -> Result<Task> {
    task::add(
        store,
        &AddTask {
            name: parsed.name,
            state: None,
            projects: parsed.projects,
            tags: parsed.tags,
            parent: None,
            url: parsed.url,
            due_date: parsed.due_date,
            due_time: parsed.due_time,
            recurrence: None,
            recurrence_from: None,
            body: String::new(),
        },
    )
}

fn run_lines(
    mut store: Store,
    reference: Option<DateTime<Local>>,
    format: OutputFormat,
    mut reader: impl BufRead,
    writer: &mut impl Write,
) -> Result<()> {
    let mut bytes = Vec::new();
    let mut line_number = 0;
    loop {
        bytes.clear();
        let length = (&mut reader)
            .take((MAX_INPUT_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .map_err(|source| Error::io("read task input", Path::new("-"), &source))?;
        if length == 0 {
            return Ok(());
        }
        line_number += 1;
        let result = (|| {
            if length > MAX_INPUT_BYTES {
                return Err(input_too_large());
            }
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            let line = std::str::from_utf8(&bytes).map_err(|_| {
                Error::validation("invalid_input", "Task input must be UTF-8").with_field("input")
            })?;
            if line.trim().is_empty() {
                return Ok(());
            }
            let now = reference.unwrap_or_else(Local::now);
            let output = match parse_line(line, &now)? {
                ParsedInput::Task(parsed) => {
                    let task = create_task(&store, parsed)?;
                    let view = TaskView::from_task(&task, &store)?;
                    CommandOutput::new(
                        format!("{} {}", task.id, task.name),
                        json!({ "version": 1, "task": view }),
                    )
                }
                ParsedInput::List(filter) => {
                    let tasks = task::list(&store, &filter, now.date_naive())?;
                    task_list_output(&store, &tasks)?
                }
                ParsedInput::Sync(choice) => {
                    let result = sync::synchronize(store.root(), choice, |conflict| {
                        Err(Error::usage(
                            "sync_conflict",
                            format!(
                                "Conflict in {:?}; use the interactive TUI to choose per conflict, or /sync ours or /sync theirs",
                                conflict.path
                            ),
                        )
                        .with_field("input"))
                    })?;
                    store = Store::open(store.root())?;
                    CommandOutput::new(
                        sync_summary(&result),
                        json!({ "version": 1, "sync": result }),
                    )
                }
            };
            write_success(&output, format, writer)?;
            writer
                .flush()
                .map_err(|source| Error::io("flush task output", Path::new("-"), &source))
        })();
        result.map_err(|error: Error| {
            if error.path().is_none() {
                error.with_path("-").with_location(line_number, 1)
            } else {
                error
            }
        })?;
    }
}

fn sync_summary(result: &SyncResult) -> String {
    format!(
        "Synced {} with {}; {}local commit; {} conflict(s) resolved",
        result.branch,
        result.upstream,
        if result.committed { "" } else { "no " },
        result.conflicts_resolved
    )
}

#[derive(Default)]
struct Catalog {
    projects: Vec<String>,
    tags: BTreeSet<String>,
    states: Vec<String>,
}

impl Catalog {
    fn load(store: &Store) -> Result<Self> {
        let mut projects: Vec<_> = store
            .list_projects()?
            .into_iter()
            .map(|project| format!("#{}", project.slug))
            .collect();
        projects.sort_unstable();
        let tags = store
            .list_tasks()?
            .into_iter()
            .flat_map(|task| task.tags)
            .map(|tag| format!("@{tag}"))
            .collect();
        let states = store
            .config()
            .states
            .iter()
            .map(|state| format!("!{}", state.id))
            .collect();
        Ok(Self {
            projects,
            tags,
            states,
        })
    }

    fn suggestions(&self, editor: &Editor) -> Vec<&str> {
        let range = editor.token_range();
        let prefix = &editor.line[range.start..editor.cursor];
        let normalized = prefix.to_lowercase();
        let list_command = editor.line.split_whitespace().next() == Some("/list");
        if editor.line[..range.start].split_whitespace().eq(["/sync"]) {
            return ["ours", "theirs"]
                .into_iter()
                .filter(|option| option.starts_with(&normalized))
                .collect();
        }
        match prefix.as_bytes().first() {
            Some(b'/') if editor.line[..range.start].trim().is_empty() => ["/list", "/sync"]
                .into_iter()
                .filter(|command| command.starts_with(&normalized))
                .collect(),
            Some(b'#') => self
                .projects
                .iter()
                .map(String::as_str)
                .filter(|value| value.starts_with(&normalized))
                .collect(),
            Some(b'@') => self
                .tags
                .iter()
                .map(String::as_str)
                .filter(|value| value.to_lowercase().starts_with(&normalized))
                .collect(),
            Some(b'!') if list_command => self
                .states
                .iter()
                .map(String::as_str)
                .filter(|value| value.starts_with(&normalized))
                .collect(),
            Some(b'd' | b'D') if list_command => {
                ["due:today", "due:tomorrow", "due:overdue", "due:none"]
                    .into_iter()
                    .filter(|value| value.starts_with(&normalized))
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

#[derive(Default)]
struct Editor {
    line: String,
    cursor: usize,
    queued: VecDeque<String>,
    selected: usize,
}

impl Editor {
    fn token_range(&self) -> Range<usize> {
        let start = self.line[..self.cursor]
            .char_indices()
            .rev()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(0, |(index, ch)| index + ch.len_utf8());
        let end = self.line[self.cursor..]
            .find(char::is_whitespace)
            .map_or(self.line.len(), |index| self.cursor + index);
        start..end
    }

    fn insert(&mut self, text: &str) -> Result<()> {
        if self.line.len() + text.len() + self.queued.iter().map(String::len).sum::<usize>()
            > MAX_INPUT_BYTES
        {
            return Err(input_too_large());
        }
        if text
            .chars()
            .any(|ch| ch.is_control() && ch != '\t' && ch != '\n' && ch != '\r')
        {
            return Err(Error::validation(
                "invalid_input",
                "Task input must not contain control characters",
            )
            .with_field("input"));
        }
        let normalized = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\t', " ");
        self.line.insert_str(self.cursor, &normalized);
        self.cursor += normalized.len();
        if self.line.contains('\n') {
            let mut parts = self.line.split('\n').map(str::to_owned);
            let first = parts.next().expect("at least one line");
            let mut pending: VecDeque<_> = parts.collect();
            if pending.back().is_some_and(String::is_empty) {
                pending.pop_back();
            }
            pending.append(&mut self.queued);
            self.queued = pending;
            self.line = first;
            self.cursor = self.line.len();
        }
        self.selected = 0;
        Ok(())
    }

    fn previous(&self) -> usize {
        self.line[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next(&self) -> usize {
        self.cursor
            + self.line[self.cursor..]
                .graphemes(true)
                .next()
                .map_or(0, str::len)
    }

    fn backspace(&mut self) {
        let previous = self.previous();
        self.line.replace_range(previous..self.cursor, "");
        self.cursor = previous;
        self.selected = 0;
    }

    fn delete(&mut self) {
        self.line.replace_range(self.cursor..self.next(), "");
        self.selected = 0;
    }

    fn complete(&mut self, suggestion: &str) -> Result<()> {
        let range = self.token_range();
        if self.line.len() - range.len()
            + suggestion.len()
            + 1
            + self.queued.iter().map(String::len).sum::<usize>()
            > MAX_INPUT_BYTES
        {
            return Err(input_too_large());
        }
        self.line.replace_range(range.clone(), suggestion);
        self.cursor = range.start + suggestion.len();
        if self.cursor == self.line.len() {
            self.line.push(' ');
            self.cursor += 1;
        } else if let Some(ch) = self.line[self.cursor..]
            .chars()
            .next()
            .filter(|ch| ch.is_whitespace())
        {
            self.cursor += ch.len_utf8();
        }
        self.selected = 0;
        Ok(())
    }

    fn advance(&mut self) {
        self.line = self.queued.pop_front().unwrap_or_default();
        self.cursor = self.line.len();
        self.selected = 0;
    }
}

struct ListResults {
    command: String,
    rows: Vec<String>,
    offset: usize,
}

fn page_size(height: u16) -> usize {
    usize::from(height.saturating_sub(13)).max(1)
}

enum Status {
    Ready,
    Info(String),
    Saved(String),
    Error(String),
}

struct Terminal {
    output: io::BufWriter<io::Stderr>,
    color: bool,
}

impl Terminal {
    fn open(color: ColorChoice) -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut terminal = Self {
            output: io::BufWriter::new(io::stderr()),
            color: match color {
                ColorChoice::Always => true,
                ColorChoice::Never => false,
                ColorChoice::Auto => {
                    !std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
                }
            },
        };
        execute!(
            terminal.output,
            EnterAlternateScreen,
            EnableBracketedPaste,
            Show
        )?;
        Ok(terminal)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.color {
            let _ = queue!(self.output, SetAttribute(Attribute::Reset));
        }
        let _ = execute!(
            self.output,
            DisableBracketedPaste,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn terminal_error(source: io::Error) -> Error {
    Error::io("use the input terminal", Path::new("<terminal>"), &source)
}

fn run_terminal(
    mut store: Store,
    reference: Option<DateTime<Local>>,
    color: ColorChoice,
) -> Result<usize> {
    let mut catalog = Catalog::load(&store)?;
    let mut terminal = Terminal::open(color).map_err(terminal_error)?;
    let mut editor = Editor::default();
    let mut status = Status::Ready;
    let mut created = 0;
    let mut results: Option<ListResults> = None;
    loop {
        let mut sync_attempted = false;
        let mut reload_failed = false;
        let now = reference.unwrap_or_else(Local::now);
        draw(
            &mut terminal,
            &editor,
            &catalog,
            &now,
            &status,
            created,
            results.as_ref(),
        )
        .map_err(terminal_error)?;
        let event = event::read().map_err(terminal_error)?;
        let result = match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Esc
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c'))
                {
                    break;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    match key.code {
                        KeyCode::Char('d')
                            if editor.line.is_empty() && editor.queued.is_empty() =>
                        {
                            break
                        }
                        KeyCode::Char('u') => {
                            editor.line.clear();
                            editor.cursor = 0;
                            editor.selected = 0;
                        }
                        KeyCode::Char('a') => {
                            editor.cursor = 0;
                            editor.selected = 0;
                        }
                        KeyCode::Char('e') => {
                            editor.cursor = editor.line.len();
                            editor.selected = 0;
                        }
                        _ => {}
                    }
                    Ok(())
                } else {
                    match key.code {
                        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::ALT) => {
                            editor.insert(ch.encode_utf8(&mut [0; 4]))
                        }
                        KeyCode::Backspace => {
                            editor.backspace();
                            Ok(())
                        }
                        KeyCode::Delete => {
                            editor.delete();
                            Ok(())
                        }
                        KeyCode::Left => {
                            editor.cursor = editor.previous();
                            editor.selected = 0;
                            Ok(())
                        }
                        KeyCode::Right => {
                            editor.cursor = editor.next();
                            editor.selected = 0;
                            Ok(())
                        }
                        KeyCode::Home => {
                            editor.cursor = 0;
                            editor.selected = 0;
                            Ok(())
                        }
                        KeyCode::End => {
                            editor.cursor = editor.line.len();
                            editor.selected = 0;
                            Ok(())
                        }
                        KeyCode::Up | KeyCode::Down | KeyCode::BackTab => {
                            let count = catalog.suggestions(&editor).len();
                            if count > 0 {
                                editor.selected = if key.code == KeyCode::Down {
                                    (editor.selected + 1) % count
                                } else {
                                    (editor.selected + count - 1) % count
                                };
                            }
                            Ok(())
                        }
                        KeyCode::PageUp | KeyCode::PageDown => {
                            if let Some(results) = &mut results {
                                let slots = page_size(terminal::size().map_err(terminal_error)?.1);
                                let last = results.rows.len().saturating_sub(slots);
                                let first = results.offset.min(last);
                                results.offset = if key.code == KeyCode::PageDown {
                                    first.saturating_add(slots).min(last)
                                } else {
                                    first.saturating_sub(slots)
                                };
                            }
                            Ok(())
                        }
                        KeyCode::Tab => {
                            let suggestions = catalog.suggestions(&editor);
                            if let Some(suggestion) = suggestions.get(editor.selected) {
                                editor.complete(suggestion)
                            } else {
                                Ok(())
                            }
                        }
                        KeyCode::F(5) => {
                            store = Store::open(store.root())?;
                            catalog = Catalog::load(&store)?;
                            editor.selected = 0;
                            status = Status::Info(
                                "Reloaded projects, tags, and states from disk.".into(),
                            );
                            Ok(())
                        }
                        KeyCode::Enter if editor.line.trim().is_empty() => {
                            editor.advance();
                            Ok(())
                        }
                        KeyCode::Enter => (|| {
                            let now = reference.unwrap_or_else(Local::now);
                            match parse_line(&editor.line, &now)? {
                                ParsedInput::Task(parsed) => {
                                    let task = create_task(&store, parsed)?;
                                    created += 1;
                                    status =
                                        Status::Saved(format!("{}  ·  {}", task.name, task.id));
                                    catalog
                                        .tags
                                        .extend(task.tags.into_iter().map(|tag| format!("@{tag}")));
                                    results = None;
                                }
                                ParsedInput::List(filter) => {
                                    let tasks = task::list(&store, &filter, now.date_naive())?;
                                    status = Status::Info(format!(
                                        "Listed {} task(s); no tasks created.",
                                        tasks.len()
                                    ));
                                    results = Some(ListResults {
                                        command: editor.line.trim().to_owned(),
                                        rows: tasks.iter().map(human_task_row).collect(),
                                        offset: 0,
                                    });
                                }
                                ParsedInput::Sync(choice) => {
                                    sync_attempted = true;
                                    status = Status::Info(
                                        "Syncing: fetch, commit store changes, merge, push…".into(),
                                    );
                                    draw(
                                        &mut terminal,
                                        &editor,
                                        &catalog,
                                        &now,
                                        &status,
                                        created,
                                        results.as_ref(),
                                    )
                                    .map_err(terminal_error)?;
                                    let outcome =
                                        sync::synchronize(store.root(), choice, |conflict| {
                                            resolve_conflict(&mut terminal, conflict)
                                        });
                                    // Even an aborted merge can replace the config inode.
                                    // Refresh before continuing, but never hide the sync error.
                                    let refreshed = Store::open(store.root()).and_then(|fresh| {
                                        Catalog::load(&fresh).map(|catalog| (fresh, catalog))
                                    });
                                    match refreshed {
                                        Ok((fresh, refreshed_catalog)) => {
                                            store = fresh;
                                            catalog = refreshed_catalog;
                                        }
                                        Err(error) => {
                                            reload_failed = true;
                                            return Err(outcome.err().unwrap_or(error));
                                        }
                                    }
                                    results = None;
                                    status = Status::Info(sync_summary(&outcome?));
                                }
                            }
                            editor.advance();
                            Ok(())
                        })(),
                        _ => Ok(()),
                    }
                }
            }
            Event::Paste(text) => editor.insert(&text),
            _ => Ok(()),
        };
        if let Err(error) = result {
            // Publication or generation failures can be uncertain: do not invite an accidental retry.
            if reload_failed
                || matches!(
                    error.kind(),
                    ErrorKind::Io | ErrorKind::Concurrent | ErrorKind::Unsupported
                )
            {
                return Err(error);
            }
            status = Status::Error(if sync_attempted {
                format!("{}: {}", error.code(), error.message())
            } else {
                format!(
                    "{}: {} (not submitted; edit this line)",
                    error.code(),
                    error.message()
                )
            });
        }
    }
    Ok(created)
}

fn conflict_preview(bytes: Option<&[u8]>) -> Cow<'_, str> {
    match bytes {
        None => Cow::Borrowed("(deleted)"),
        Some([]) => Cow::Borrowed("(empty file or removed text)"),
        Some(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) if !bytes.contains(&0) => Cow::Borrowed(text),
            _ => Cow::Owned(format!("(binary content: {} bytes)", bytes.len())),
        },
    }
}

fn from_column(value: &str, column: usize) -> &str {
    let mut used = 0;
    for (index, grapheme) in value.grapheme_indices(true) {
        if used >= column {
            return &value[index..];
        }
        used += grapheme.width();
    }
    ""
}

fn resolve_conflict(terminal: &mut Terminal, conflict: &SyncConflict) -> Result<ConflictChoice> {
    let ours = conflict_preview(conflict.ours.as_deref());
    let theirs = conflict_preview(conflict.theirs.as_deref());
    let rows = std::iter::once(("OURS — local", Ink::Success))
        .chain(ours.lines().map(|line| (line, Ink::Plain)))
        .chain([("", Ink::Plain), ("THEIRS — remote", Ink::Date)])
        .chain(theirs.lines().map(|line| (line, Ink::Plain)));
    let row_count = rows.clone().count();
    let mut offset = 0usize;
    let mut column = 0usize;
    loop {
        let (width, height) = terminal::size().map_err(terminal_error)?;
        let slots = usize::from(height.saturating_sub(7)).max(1);
        offset = offset.min(row_count.saturating_sub(slots));
        let mut screen = Screen {
            output: &mut terminal.output,
            width,
            height,
            color: terminal.color,
        };
        (|| -> io::Result<()> {
            if screen.color {
                queue!(screen.output, SetAttribute(Attribute::Reset))?;
            }
            queue!(screen.output, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
            screen.row(0, &[(" Sync conflict ", Ink::Selected)])?;
            screen.row(
                1,
                &[(conflict.path.to_string_lossy().as_ref(), Ink::Accent)],
            )?;
            screen.row(2, &[(&conflict.description, Ink::Muted)])?;
            screen.row(
                3,
                &[
                    ("O", Ink::Key),
                    (" ours/local   ", Ink::Muted),
                    ("T", Ink::Key),
                    (" theirs/remote   ", Ink::Muted),
                    ("Esc", Ink::Key),
                    (" cancel sync", Ink::Muted),
                ],
            )?;
            screen.row(
                4,
                &[(
                    "↑/↓ PgUp/PgDn scroll · ←/→ pan · choices affect this conflict only",
                    Ink::Muted,
                )],
            )?;
            for (index, (text, ink)) in rows.clone().skip(offset).take(slots).enumerate() {
                screen.row(5 + index as u16, &[(from_column(text, column), ink)])?;
            }
            screen.row(
                height.saturating_sub(1),
                &[(
                    &format!(
                        "Lines {}–{} / {} · column {}",
                        offset + 1,
                        (offset + slots).min(row_count),
                        row_count,
                        column + 1
                    ),
                    Ink::Muted,
                )],
            )?;
            screen.output.flush()
        })()
        .map_err(terminal_error)?;
        if let Event::Key(key) = event::read().map_err(terminal_error)? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.code == KeyCode::Esc
                || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            {
                return Err(Error::usage(
                    "sync_cancelled",
                    "Sync cancelled; any local sync commit is retained",
                ));
            }
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            {
                continue;
            }
            match key.code {
                KeyCode::Char('o' | 'O') => return Ok(ConflictChoice::Ours),
                KeyCode::Char('t' | 'T') => return Ok(ConflictChoice::Theirs),
                KeyCode::Up => offset = offset.saturating_sub(1),
                KeyCode::Down => offset = offset.saturating_add(1),
                KeyCode::PageUp => offset = offset.saturating_sub(slots),
                KeyCode::PageDown => offset = offset.saturating_add(slots),
                KeyCode::Home => {
                    offset = 0;
                    column = 0;
                }
                KeyCode::End => offset = row_count.saturating_sub(slots),
                KeyCode::Left => column = column.saturating_sub(8),
                KeyCode::Right => column = column.saturating_add(8),
                _ => {}
            }
        }
    }
}

fn clipped(value: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        let safe = if grapheme.chars().any(char::is_control) {
            "�"
        } else {
            grapheme
        };
        let cells = safe.width();
        if used + cells > width {
            break;
        }
        result.push_str(safe);
        used += cells;
    }
    result
}

// Use the terminal's ANSI palette: Omarchy themes already tune these colors
// together. Leave the canvas and ordinary text at the terminal defaults.
#[derive(Clone, Copy)]
enum Ink {
    Plain,
    Muted,
    Accent,
    Key,
    Project,
    Tag,
    Date,
    Success,
    Error,
    Selected,
}

impl Ink {
    fn style(self) -> ContentStyle {
        let plain = ContentStyle::default();
        match self {
            Self::Plain => plain,
            Self::Muted => plain.dim(),
            Self::Accent => plain.with(Color::DarkBlue).bold(),
            Self::Key => plain.bold(),
            Self::Project => plain.with(Color::DarkMagenta),
            Self::Tag => plain.with(Color::DarkCyan),
            Self::Date => plain.with(Color::DarkYellow),
            Self::Success => plain.with(Color::DarkGreen),
            Self::Error => plain.with(Color::DarkRed).bold(),
            Self::Selected => plain.reverse().bold(),
        }
    }
}

struct Screen<'a, W> {
    output: &'a mut W,
    width: u16,
    height: u16,
    color: bool,
}

impl<W: Write> Screen<'_, W> {
    fn row(&mut self, y: u16, spans: &[(&str, Ink)]) -> io::Result<()> {
        if y >= self.height || self.width <= 2 {
            return Ok(());
        }
        queue!(self.output, MoveTo(1, y))?;
        let mut remaining = usize::from(self.width - 2);
        'spans: for (text, ink) in spans {
            if self.color {
                queue!(
                    self.output,
                    SetAttribute(Attribute::Reset),
                    SetStyle(ink.style())
                )?;
            }
            // Clip visible cells, never styled escape sequences; sanitize all
            // store-derived strings just as strictly as the editable draft.
            for grapheme in text.graphemes(true) {
                let safe = if grapheme.chars().any(char::is_control) {
                    "�"
                } else {
                    grapheme
                };
                let cells = safe.width();
                if cells > remaining {
                    break 'spans;
                }
                queue!(self.output, Print(safe))?;
                remaining -= cells;
            }
        }
        if self.color {
            queue!(self.output, SetAttribute(Attribute::Reset))?;
        }
        Ok(())
    }
}

fn draw(
    terminal: &mut Terminal,
    editor: &Editor,
    catalog: &Catalog,
    reference: &DateTime<Local>,
    status: &Status,
    created: usize,
    results: Option<&ListResults>,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let mut screen = Screen {
        output: &mut terminal.output,
        width,
        height,
        color: terminal.color,
    };
    if screen.color {
        queue!(screen.output, SetAttribute(Attribute::Reset))?;
    }
    queue!(screen.output, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
    if width < 32 || height < 14 {
        screen.row(0, &[("Resize to at least 32 x 14. Esc exits.", Ink::Date)])?;
        return screen.output.flush();
    }
    screen.row(
        0,
        &[
            (" otodo ", Ink::Selected),
            (" input", Ink::Accent),
            (
                &format!(" {created} saved"),
                if created == 0 {
                    Ink::Muted
                } else {
                    Ink::Success
                },
            ),
            (
                &format!(" {} queued", editor.queued.len()),
                if editor.queued.is_empty() {
                    Ink::Muted
                } else {
                    Ink::Date
                },
            ),
        ],
    )?;
    screen.row(
        1,
        &[
            ("Enter", Ink::Key),
            (" submit   ", Ink::Muted),
            ("Tab", Ink::Key),
            (" complete   ", Ink::Muted),
            ("↑/↓", Ink::Key),
            (" select   ", Ink::Muted),
            ("F5", Ink::Key),
            (" reload", Ink::Muted),
        ],
    )?;
    screen.row(
        2,
        &[
            ("Esc/Ctrl-C", Ink::Key),
            (" exit   ", Ink::Muted),
            ("Ctrl-D", Ink::Key),
            (" exit empty   ", Ink::Muted),
            ("Ctrl-U", Ink::Key),
            (" clear", Ink::Muted),
        ],
    )?;

    let rule = "─".repeat(usize::from(width - 4));
    screen.row(
        3,
        &[("╭", Ink::Accent), (&rule, Ink::Muted), ("╮", Ink::Accent)],
    )?;
    screen.row(3, &[("╭─ capture ", Ink::Accent)])?;
    screen.row(
        5,
        &[("╰", Ink::Accent), (&rule, Ink::Muted), ("╯", Ink::Accent)],
    )?;
    let input_x = 1 + INPUT_PROMPT.width() as u16;
    // Reserve the right border, its inner padding, and the outer margin.
    let available = usize::from(width - input_x - 3);
    let mut start = 0;
    let mut cursor_width = editor.line[..editor.cursor].width();
    for (index, grapheme) in editor.line[..editor.cursor].grapheme_indices(true) {
        if cursor_width < available {
            break;
        }
        cursor_width = cursor_width.saturating_sub(grapheme.width());
        start = index + grapheme.len();
    }
    let visible = clipped(
        if editor.line.is_empty() {
            "What needs doing?"
        } else {
            &editor.line[start..]
        },
        available,
    );
    screen.row(
        4,
        &[
            (INPUT_PROMPT, Ink::Accent),
            (
                &visible,
                if editor.line.is_empty() {
                    Ink::Muted
                } else {
                    Ink::Plain
                },
            ),
        ],
    )?;
    queue!(screen.output, MoveTo(width - 2, 4))?;
    if screen.color {
        queue!(screen.output, SetStyle(Ink::Accent.style()))?;
    }
    queue!(screen.output, Print("│"))?;
    if screen.color {
        queue!(screen.output, SetAttribute(Attribute::Reset))?;
    }

    match parse_line(&editor.line, reference) {
        Ok(ParsedInput::Task(parsed)) => {
            screen.row(6, &[("Name  ", Ink::Muted), (&parsed.name, Ink::Plain)])?;
            let due = parsed
                .due_date
                .map_or_else(|| "none".to_owned(), |date| date.to_string());
            let time = parsed
                .due_time
                .map_or_else(String::new, |time| format!(" {}", time.format("%H:%M")));
            screen.row(
                7,
                &[
                    ("Due ", Ink::Muted),
                    (
                        &due,
                        if parsed.due_date.is_some() {
                            Ink::Date
                        } else {
                            Ink::Muted
                        },
                    ),
                    (&time, Ink::Date),
                    ("  Projects ", Ink::Muted),
                    (&parsed.projects.join(", "), Ink::Project),
                    ("  Tags ", Ink::Muted),
                    (&parsed.tags.join(", "), Ink::Tag),
                ],
            )?;
            screen.row(
                8,
                &[
                    ("URL   ", Ink::Muted),
                    (
                        parsed.url.as_deref().unwrap_or("none"),
                        if parsed.url.is_some() {
                            Ink::Accent
                        } else {
                            Ink::Muted
                        },
                    ),
                ],
            )?;
        }
        Ok(ParsedInput::List(filter)) => {
            screen.row(
                6,
                &[
                    ("List tasks", Ink::Accent),
                    ("  read-only · all states unless filtered", Ink::Muted),
                ],
            )?;
            screen.row(
                7,
                &[
                    ("Projects ", Ink::Muted),
                    (&filter.projects.join(", "), Ink::Project),
                    ("  Tags ", Ink::Muted),
                    (&filter.tags.join(", "), Ink::Tag),
                    ("  States ", Ink::Muted),
                    (&filter.states.join(", "), Ink::Accent),
                ],
            )?;
            let due = if filter.no_due {
                "none".to_owned()
            } else if filter.overdue {
                "overdue (nonterminal, before today)".to_owned()
            } else {
                filter
                    .due_on
                    .map(|date| date.to_string())
                    .unwrap_or_else(|| "any".to_owned())
            };
            screen.row(8, &[("Due ", Ink::Muted), (&due, Ink::Date)])?;
        }
        Ok(ParsedInput::Sync(choice)) => {
            screen.row(
                6,
                &[
                    ("Sync Git branch", Ink::Accent),
                    ("  commit store · pull/merge · push", Ink::Muted),
                ],
            )?;
            screen.row(
                7,
                &[(
                    match choice {
                        None => "Conflicts: choose ours/local or theirs/remote for each conflict",
                        Some(ConflictChoice::Ours) => {
                            "Conflicts: prefer ours/local; preserve non-conflicting remote edits"
                        }
                        Some(ConflictChoice::Theirs) => {
                            "Conflicts: prefer theirs/remote; preserve non-conflicting local edits"
                        }
                    },
                    Ink::Date,
                )],
            )?;
            screen.row(
                8,
                &[(
                    "Stop other writers/sync first; containing branch is synchronized.",
                    Ink::Muted,
                )],
            )?;
        }
        Err(error) if !editor.line.trim().is_empty() => {
            screen.row(6, &[("Preview  ", Ink::Date), (error.message(), Ink::Date)])?
        }
        Err(_) => {
            screen.row(
                6,
                &[
                    ("Try  ", Ink::Muted),
                    ("Call Plumber ", Ink::Plain),
                    ("tom 9am ", Ink::Date),
                    ("#project ", Ink::Project),
                    ("@tag", Ink::Tag),
                ],
            )?;
            screen.row(
                7,
                &[
                    ("Or   ", Ink::Muted),
                    ("/list ", Ink::Accent),
                    ("#project ", Ink::Project),
                    ("@tag ", Ink::Tag),
                    ("!open ", Ink::Accent),
                    ("due:today", Ink::Date),
                ],
            )?;
            screen.row(
                8,
                &[("Or   ", Ink::Muted), ("/sync [ours|theirs]", Ink::Accent)],
            )?;
        }
    }
    let suggestions = catalog.suggestions(editor);
    let slots = page_size(height);
    if suggestions.is_empty() {
        if let Some(results) = results {
            let first = results.offset.min(results.rows.len().saturating_sub(slots));
            let end = (first + slots).min(results.rows.len());
            screen.row(
                9,
                &[
                    ("Results  ", Ink::Accent),
                    (
                        &format!(
                            "{}–{} / {}",
                            usize::from(!results.rows.is_empty()) + first,
                            end,
                            results.rows.len(),
                        ),
                        Ink::Plain,
                    ),
                    ("  PgUp/PgDn", Ink::Key),
                    (" scroll  ", Ink::Muted),
                    (&results.command, Ink::Muted),
                ],
            )?;
            if results.rows.is_empty() {
                screen.row(10, &[("No matching tasks.", Ink::Muted)])?;
            }
            for (index, task) in results.rows[first..end].iter().enumerate() {
                screen.row(10 + index as u16, &[(task, Ink::Plain)])?;
            }
        }
    } else {
        let (label, ink) = match suggestions[0].as_bytes().first() {
            Some(b'#') => ("Projects", Ink::Project),
            Some(b'@') => ("Tags", Ink::Tag),
            Some(b'!') => ("States", Ink::Accent),
            Some(b'd') => ("Due filters", Ink::Date),
            _ => ("Commands", Ink::Accent),
        };
        screen.row(
            9,
            &[
                (label, ink),
                (
                    &format!("  {} / {}  ", editor.selected + 1, suggestions.len()),
                    Ink::Muted,
                ),
                ("Tab", Ink::Key),
                (" accept", Ink::Muted),
            ],
        )?;
        let first = editor.selected.saturating_sub(slots - 1);
        for (index, suggestion) in suggestions.iter().enumerate().skip(first).take(slots) {
            let selected = index == editor.selected;
            let ink = if selected { Ink::Selected } else { ink };
            screen.row(
                10 + (index - first) as u16,
                &[
                    (if selected { " › " } else { "   " }, ink),
                    (suggestion, ink),
                    (" ", ink),
                ],
            )?;
        }
    }
    let (label, message, ink) = match status {
        Status::Ready => (
            "ready",
            "Enter submits · pasted lines are reviewed one at a time",
            Ink::Muted,
        ),
        Status::Info(message) => ("info", message.as_str(), Ink::Accent),
        Status::Saved(message) => ("saved", message.as_str(), Ink::Success),
        Status::Error(message) => ("error", message.as_str(), Ink::Error),
    };
    screen.row(
        height - 2,
        &[(label, ink), ("  ", Ink::Plain), (message, Ink::Plain)],
    )?;
    let x = input_x + cursor_width as u16;
    queue!(screen.output, MoveTo(x, 4), Show)?;
    screen.output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styled_rows_clip_visible_graphemes_and_sanitize_store_text() {
        let controls = regex::Regex::new(r"\x1b\[[0-9;]*[Hm]").unwrap();
        let styles = regex::Regex::new(r"\x1b\[[0-9;]*m").unwrap();
        for color in [false, true] {
            let mut output = Vec::new();
            Screen {
                output: &mut output,
                width: 8,
                height: 1,
                color,
            }
            .row(
                0,
                &[
                    ("A", Ink::Accent),
                    ("界e\u{301}\u{1b}", Ink::Tag),
                    ("界\u{301}", Ink::Project),
                    ("must not skip past the clipped grapheme", Ink::Plain),
                ],
            )
            .unwrap();
            let output = String::from_utf8(output).unwrap();
            assert_eq!(controls.replace_all(&output, ""), "A界e\u{301}�");
            if !color {
                assert!(
                    !styles.is_match(&output),
                    "monochrome rows must not emit SGR"
                );
            }
        }
    }

    #[test]
    fn slash_and_state_completion_respect_command_and_unicode_token_boundaries() {
        let catalog = Catalog {
            states: vec!["!open".into(), "!done".into(), "!waiting_review".into()],
            ..Catalog::default()
        };
        let mut editor = Editor::default();
        editor.insert("  /LI").unwrap();
        assert_eq!(catalog.suggestions(&editor), ["/list"]);
        editor.complete("/list").unwrap();
        editor.insert("@Équipe/Été !OPx").unwrap();
        editor.cursor -= 1;
        assert_eq!(catalog.suggestions(&editor), ["!open"]);
        editor.complete("!open").unwrap();
        assert_eq!(editor.line, "  /list @Équipe/Été !open ");
        editor.insert("!w").unwrap();
        assert_eq!(catalog.suggestions(&editor), ["!waiting_review"]);
        for line in [
            "Review /li",
            "Review !op",
            "/listing !op",
            "https://example.test/",
        ] {
            let mut editor = Editor::default();
            editor.insert(line).unwrap();
            assert!(catalog.suggestions(&editor).is_empty(), "{line}");
        }
    }

    #[test]
    fn due_completion_is_list_only_and_preserves_surrounding_unicode() {
        let catalog = Catalog::default();
        let mut editor = Editor::default();
        editor.insert("/list due:").unwrap();
        assert_eq!(
            catalog.suggestions(&editor),
            ["due:today", "due:tomorrow", "due:overdue", "due:none"]
        );
        let mut editor = Editor::default();
        editor.insert("  /list @Équipe/Été DUEx !open").unwrap();
        editor.cursor = "  /list @Équipe/Été DUE".len();
        editor.complete(catalog.suggestions(&editor)[3]).unwrap();
        assert_eq!(editor.line, "  /list @Équipe/Été due:none !open");
        for line in ["Discuss due:", "/listing due:", "/sync due:", "/list @due:"] {
            let mut editor = Editor::default();
            editor.insert(line).unwrap();
            assert!(catalog.suggestions(&editor).is_empty(), "{line}");
        }
    }

    #[test]
    fn sync_completion_replaces_options_without_touching_surrounding_text() {
        let catalog = Catalog::default();
        let mut editor = Editor::default();
        editor.insert("  /SY").unwrap();
        assert_eq!(catalog.suggestions(&editor), ["/sync"]);
        editor.complete("/sync").unwrap();
        assert_eq!(catalog.suggestions(&editor), ["ours", "theirs"]);
        editor.insert("THx").unwrap();
        editor.cursor -= 1;
        assert_eq!(catalog.suggestions(&editor), ["theirs"]);
        editor.complete("theirs").unwrap();
        assert_eq!(editor.line, "  /sync theirs ");
        assert!(catalog.suggestions(&editor).is_empty());
        for line in ["Discuss /sync o", "/syncing o"] {
            let mut editor = Editor::default();
            editor.insert(line).unwrap();
            assert!(catalog.suggestions(&editor).is_empty());
        }
    }

    #[test]
    fn completion_replaces_whole_token_at_unicode_cursor_and_ignores_urls() {
        let catalog = Catalog {
            projects: vec!["#personal".into(), "#work".into()],
            tags: BTreeSet::from(["@Chörés/home".into()]),
            states: Vec::new(),
        };
        let mut editor = Editor::default();
        editor.insert("Call Zoë #PERx tomorrow").unwrap();
        editor.cursor = "Call Zoë #PER".len();
        assert_eq!(catalog.suggestions(&editor), ["#personal"]);
        editor.complete("#personal").unwrap();
        assert_eq!(editor.line, "Call Zoë #personal tomorrow");
        assert_eq!(&editor.line[..editor.cursor], "Call Zoë #personal ");
        editor.line.clear();
        editor.cursor = 0;
        editor.insert("Read https://example.test/#per").unwrap();
        assert!(catalog.suggestions(&editor).is_empty());
        editor.insert(" @chÖ").unwrap();
        assert_eq!(catalog.suggestions(&editor), ["@Chörés/home"]);
        editor.complete("@Chörés/home").unwrap();
        assert!(editor.line.ends_with(" @Chörés/home "));
    }

    #[test]
    fn unicode_editing_and_multiline_paste_preserve_queued_drafts() {
        let mut editor = Editor::default();
        editor.insert("Café e\u{301}").unwrap();
        editor.backspace();
        assert_eq!(editor.line, "Café ");
        editor.insert("first\r\nsecond #work\nthird\n").unwrap();
        assert_eq!(editor.line, "Café first");
        assert_eq!(editor.queued, ["second #work", "third"]);
        editor.cursor = 0;
        editor.delete();
        assert_eq!(editor.line, "afé first");
        editor.advance();
        assert_eq!(editor.line, "second #work");
        editor.advance();
        assert_eq!(editor.line, "third");
    }

    #[test]
    fn rejected_paste_keeps_draft_and_rendering_never_emits_controls() {
        let mut editor = Editor::default();
        editor.insert("Keep this").unwrap();
        assert_eq!(
            editor
                .insert(&"x".repeat(MAX_INPUT_BYTES))
                .unwrap_err()
                .code(),
            "input_too_large"
        );
        assert_eq!(
            editor.insert("\u{1b}[2J").unwrap_err().code(),
            "invalid_input"
        );
        assert_eq!(editor.line, "Keep this");
        assert_eq!(clipped("界a\u{1b}b", 4), "界a�");
    }
}
