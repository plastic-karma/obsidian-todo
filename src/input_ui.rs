use std::collections::{BTreeSet, VecDeque};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::ops::Range;
use std::path::Path;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue, style::Print};
use serde_json::json;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use obsidian_todo::commands::task::{self, AddTask, TaskView};
use obsidian_todo::error::{Error, ErrorKind, Result};
use obsidian_todo::input::{parse_line, ParsedInput};
use obsidian_todo::model::Task;
use obsidian_todo::store::Store;

use crate::output::{write_success, CommandOutput, OutputFormat};

// Bound a line and a queued paste independently of the potentially much larger record body.
const MAX_INPUT_BYTES: usize = 16 * 1024;

pub fn run(store: &Store, today: Option<NaiveDate>, format: OutputFormat) -> Result<()> {
    let fixed_reference = today.map(reference_at_midnight).transpose()?;
    if format == OutputFormat::Human
        && io::stdin().is_terminal()
        && io::stderr().is_terminal()
        && std::env::var_os("TERM").is_some_and(|term| term != "dumb")
    {
        let created = run_terminal(store, fixed_reference)?;
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

fn create_task(store: &Store, parsed: ParsedInput) -> Result<Task> {
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
    store: &Store,
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
            let task = create_task(
                store,
                parse_line(line, &reference.unwrap_or_else(Local::now))?,
            )?;
            let view = TaskView::from_task(&task, store)?;
            write_success(
                &CommandOutput::new(
                    format!("{} {}", task.id, task.name),
                    json!({ "version": 1, "task": view }),
                ),
                format,
                writer,
            )?;
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

#[derive(Default)]
struct Catalog {
    projects: Vec<String>,
    tags: BTreeSet<String>,
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
        Ok(Self { projects, tags })
    }

    fn suggestions(&self, editor: &Editor) -> Vec<&str> {
        let range = editor.token_range();
        let prefix = &editor.line[range.start..editor.cursor];
        let normalized = prefix.to_lowercase();
        match prefix.as_bytes().first() {
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

struct Terminal {
    output: io::Stderr,
}

impl Terminal {
    fn open() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut terminal = Self {
            output: io::stderr(),
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

fn run_terminal(store: &Store, reference: Option<DateTime<Local>>) -> Result<usize> {
    let mut catalog = Catalog::load(store)?;
    let mut terminal = Terminal::open().map_err(terminal_error)?;
    let mut editor = Editor::default();
    let mut status =
        "Ready. Nothing is saved until Enter; pasted lines are reviewed one at a time.".to_owned();
    let mut created = 0;
    loop {
        let now = reference.unwrap_or_else(Local::now);
        draw(
            &mut terminal.output,
            &editor,
            &catalog,
            &now,
            &status,
            created,
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
                        KeyCode::Tab => {
                            let suggestions = catalog.suggestions(&editor);
                            if let Some(suggestion) = suggestions.get(editor.selected) {
                                editor.complete(suggestion)
                            } else {
                                Ok(())
                            }
                        }
                        KeyCode::F(5) => {
                            catalog = Catalog::load(store)?;
                            editor.selected = 0;
                            status = "Reloaded projects and tags from disk.".to_owned();
                            Ok(())
                        }
                        KeyCode::Enter if editor.line.trim().is_empty() => {
                            editor.advance();
                            Ok(())
                        }
                        KeyCode::Enter => {
                            match parse_line(&editor.line, &reference.unwrap_or_else(Local::now))
                                .and_then(|parsed| create_task(store, parsed))
                            {
                                Ok(task) => {
                                    created += 1;
                                    status = format!("Saved {}  {}", task.id, task.name);
                                    catalog
                                        .tags
                                        .extend(task.tags.into_iter().map(|tag| format!("@{tag}")));
                                    editor.advance();
                                    Ok(())
                                }
                                Err(error) => Err(error),
                            }
                        }
                        _ => Ok(()),
                    }
                }
            }
            Event::Paste(text) => editor.insert(&text),
            _ => Ok(()),
        };
        if let Err(error) = result {
            // Publication or generation failures can be uncertain: do not invite an accidental retry.
            if matches!(
                error.kind(),
                ErrorKind::Io | ErrorKind::Concurrent | ErrorKind::Unsupported
            ) {
                return Err(error);
            }
            status = format!(
                "{}: {} (not saved; edit this line)",
                error.code(),
                error.message()
            );
        }
    }
    Ok(created)
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

fn row(output: &mut impl Write, y: u16, width: u16, height: u16, text: &str) -> io::Result<()> {
    if y < height {
        queue!(
            output,
            MoveTo(0, y),
            Print(clipped(text, usize::from(width.saturating_sub(1))))
        )?;
    }
    Ok(())
}

fn draw(
    output: &mut impl Write,
    editor: &Editor,
    catalog: &Catalog,
    reference: &DateTime<Local>,
    status: &str,
    created: usize,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    queue!(output, Hide, MoveTo(0, 0), Clear(ClearType::All))?;
    if width < 32 || height < 14 {
        row(
            output,
            0,
            width,
            height,
            "Resize to at least 32 x 14. Esc exits.",
        )?;
        return output.flush();
    }
    row(
        output,
        0,
        width,
        height,
        &format!(
            "otodo input  |  {created} saved  |  {} queued",
            editor.queued.len()
        ),
    )?;
    row(
        output,
        1,
        width,
        height,
        "Enter: save  Tab: complete  Up/Down: select  F5: reload",
    )?;
    row(
        output,
        2,
        width,
        height,
        "Esc/Ctrl-C: exit  Ctrl-D: exit empty  Ctrl-U: clear line",
    )?;
    let available = usize::from(width - 4);
    let mut start = 0;
    let mut cursor_width = editor.line[..editor.cursor].width();
    for (index, grapheme) in editor.line[..editor.cursor].grapheme_indices(true) {
        if cursor_width < available {
            break;
        }
        cursor_width = cursor_width.saturating_sub(grapheme.width());
        start = index + grapheme.len();
    }
    row(
        output,
        4,
        width,
        height,
        &format!("> {}", clipped(&editor.line[start..], available)),
    )?;
    match parse_line(&editor.line, reference) {
        Ok(parsed) => {
            row(output, 6, width, height, &format!("Name: {}", parsed.name))?;
            let due = parsed
                .due_date
                .map_or_else(|| "none".to_owned(), |date| date.to_string());
            let time = parsed
                .due_time
                .map_or_else(String::new, |time| format!(" {}", time.format("%H:%M")));
            row(
                output,
                7,
                width,
                height,
                &format!(
                    "Due: {due}{time}   Projects: {}   Tags: {}",
                    parsed.projects.join(", "),
                    parsed.tags.join(", ")
                ),
            )?;
            row(
                output,
                8,
                width,
                height,
                &format!("URL: {}", parsed.url.as_deref().unwrap_or("none")),
            )?;
        }
        Err(error) if !editor.line.trim().is_empty() => row(
            output,
            6,
            width,
            height,
            &format!("Preview: {}", error.message()),
        )?,
        Err(_) => row(
            output,
            6,
            width,
            height,
            "Type a task, e.g. Call Plumber tom 9am #personal @chores",
        )?,
    }
    let suggestions = catalog.suggestions(editor);
    let slots = usize::from(height.saturating_sub(13)).max(1);
    let first = editor.selected.saturating_sub(slots - 1);
    for (index, suggestion) in suggestions.iter().enumerate().skip(first).take(slots) {
        let marker = if index == editor.selected { ">" } else { " " };
        row(
            output,
            10 + (index - first) as u16,
            width,
            height,
            &format!("{marker} {suggestion}"),
        )?;
    }
    row(output, height - 2, width, height, status)?;
    let x = 2 + cursor_width as u16;
    queue!(output, MoveTo(x, 4), Show)?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_replaces_whole_token_at_unicode_cursor_and_ignores_urls() {
        let catalog = Catalog {
            projects: vec!["#personal".into(), "#work".into()],
            tags: BTreeSet::from(["@Chörés/home".into()]),
        };
        let mut editor = Editor::default();
        editor.insert("Call Zoë #pex tomorrow").unwrap();
        editor.cursor = "Call Zoë #pe".len();
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
