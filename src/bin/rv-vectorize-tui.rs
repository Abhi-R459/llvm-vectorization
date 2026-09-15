use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

const POLICIES: [&str; 3] = ["balanced", "conservative", "aggressive"];
const VECTOR_WIDTHS: [&str; 5] = ["auto", "2", "4", "8", "16"];
const FIELD_COUNT: usize = 8;
const HISTORY_CAP: usize = 8;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const TICK: Duration = Duration::from_millis(80);

// A Claude-Code-inspired palette: warm rust/orange accent on a neutral,
// mostly-monochrome backdrop, rather than the primary cyan/yellow of a
// typical TUI form.
const ACCENT: Color = Color::Rgb(0xD9, 0x77, 0x57);
const ACCENT_DIM: Color = Color::Rgb(0x8A, 0x55, 0x42);
const INK: Color = Color::Rgb(0x16, 0x14, 0x12);
const TEXT: Color = Color::Rgb(0xE8, 0xE6, 0xE1);
const MUTED: Color = Color::Rgb(0x8A, 0x87, 0x82);
const SUCCESS: Color = Color::Rgb(0x5C, 0xB8, 0x5C);
const FAILURE: Color = Color::Rgb(0xE0, 0x5A, 0x4E);
const PENDING: Color = Color::Rgb(0xE0, 0xB0, 0x5A);

#[derive(Clone, Debug, Eq, PartialEq)]
struct TextField {
    value: String,
    cursor: usize,
}

impl TextField {
    fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.len();
        Self { value, cursor }
    }

    fn insert(&mut self, character: char) {
        self.value.insert(self.cursor, character);
        self.cursor += character.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some((index, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.value.remove(index);
            self.cursor = index;
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.value.len() {
            self.value.remove(self.cursor);
        }
    }

    fn move_left(&mut self) {
        if let Some((index, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.cursor = index;
        }
    }

    fn move_right(&mut self) {
        if let Some(character) = self.value[self.cursor..].chars().next() {
            self.cursor += character.len_utf8();
        }
    }
}

/// The status of one entry in the run transcript.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryStatus {
    Running,
    Success,
    Failed,
}

/// One invocation shown in the session transcript, styled like a single
/// turn of tool use in a coding-agent CLI: the command that ran, then its
/// outcome and any captured output.
#[derive(Clone, Debug)]
struct HistoryEntry {
    command: String,
    status: EntryStatus,
    detail: String,
}

/// Result of a finished background invocation, sent back over a channel so
/// the UI thread never blocks on the child process.
struct WorkerResult {
    success: bool,
    code: Option<i32>,
    stdout: String,
    stderr: String,
    spawn_error: Option<String>,
}

#[allow(clippy::struct_excessive_bools)]
struct App {
    input: TextField,
    output: TextField,
    policy: usize,
    vector_width: usize,
    report: bool,
    verify: bool,
    emit_bitcode: bool,
    focused: usize,
    history: Vec<HistoryEntry>,
    transcript_scroll: u16,
    spinner_frame: usize,
    worker: Option<Receiver<WorkerResult>>,
    quit: bool,
}

impl fmt::Debug for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("App")
            .field("input", &self.input)
            .field("output", &self.output)
            .field("focused", &self.focused)
            .field("history_len", &self.history.len())
            .field("running", &self.worker.is_some())
            .finish()
    }
}

impl Default for App {
    fn default() -> Self {
        Self {
            input: TextField::new("tests/fixtures/vectorizable.ll"),
            output: TextField::new("build/tui-vectorized.ll"),
            policy: 0,
            vector_width: 0,
            report: true,
            verify: true,
            emit_bitcode: false,
            focused: 0,
            history: Vec::new(),
            transcript_scroll: 0,
            spinner_frame: 0,
            worker: None,
            quit: false,
        }
    }
}

impl App {
    fn next_field(&mut self) {
        self.focused = (self.focused + 1) % FIELD_COUNT;
    }

    fn previous_field(&mut self) {
        self.focused = (self.focused + FIELD_COUNT - 1) % FIELD_COUNT;
    }

    fn selected_text_field(&mut self) -> Option<&mut TextField> {
        match self.focused {
            0 => Some(&mut self.input),
            1 => Some(&mut self.output),
            _ => None,
        }
    }

    fn cycle_selected(&mut self, forward: bool) {
        match self.focused {
            2 => {
                self.policy = if forward {
                    (self.policy + 1) % POLICIES.len()
                } else {
                    (self.policy + POLICIES.len() - 1) % POLICIES.len()
                };
            }
            3 => {
                self.vector_width = if forward {
                    (self.vector_width + 1) % VECTOR_WIDTHS.len()
                } else {
                    (self.vector_width + VECTOR_WIDTHS.len() - 1) % VECTOR_WIDTHS.len()
                };
            }
            _ => {}
        }
    }

    fn toggle_selected(&mut self) {
        match self.focused {
            4 => self.report = !self.report,
            5 => self.verify = !self.verify,
            6 => self.emit_bitcode = !self.emit_bitcode,
            _ => {}
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.execute();
            return;
        }
        match key.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Down => self.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.previous_field(),
            KeyCode::PageUp => self.transcript_scroll = self.transcript_scroll.saturating_sub(5),
            KeyCode::PageDown => self.transcript_scroll = self.transcript_scroll.saturating_add(5),
            KeyCode::Left => {
                if let Some(field) = self.selected_text_field() {
                    field.move_left();
                } else {
                    self.cycle_selected(false);
                }
            }
            KeyCode::Right => {
                if let Some(field) = self.selected_text_field() {
                    field.move_right();
                } else {
                    self.cycle_selected(true);
                }
            }
            KeyCode::Home => {
                if let Some(field) = self.selected_text_field() {
                    field.cursor = 0;
                }
            }
            KeyCode::End => {
                if let Some(field) = self.selected_text_field() {
                    field.cursor = field.value.len();
                }
            }
            KeyCode::Backspace => {
                if let Some(field) = self.selected_text_field() {
                    field.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(field) = self.selected_text_field() {
                    field.delete();
                }
            }
            KeyCode::Enter if self.focused == 7 => self.execute(),
            KeyCode::Char(' ') | KeyCode::Enter if (4..=6).contains(&self.focused) => {
                self.toggle_selected();
            }
            KeyCode::Char(character)
                if self.focused <= 1
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if let Some(field) = self.selected_text_field() {
                    field.insert(character);
                }
            }
            _ => {}
        }
    }

    fn command_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        if VECTOR_WIDTHS[self.vector_width] == "auto" {
            arguments.extend(["--policy".to_owned(), POLICIES[self.policy].to_owned()]);
        } else {
            arguments.extend([
                "--vf".to_owned(),
                VECTOR_WIDTHS[self.vector_width].to_owned(),
            ]);
        }
        if self.report {
            arguments.push("--report".to_owned());
        }
        if !self.verify {
            arguments.push("--no-verify".to_owned());
        }
        if self.emit_bitcode {
            arguments.push("--emit-bitcode".to_owned());
        }
        arguments.push(self.input.value.clone());
        arguments.extend(["-o".to_owned(), self.output.value.clone()]);
        arguments
    }

    /// Push a terminal (non-running) entry straight onto the transcript,
    /// for validation failures that never reach the child process.
    fn push_immediate_failure(&mut self, message: impl Into<String>) {
        self.push_history(HistoryEntry {
            command: String::new(),
            status: EntryStatus::Failed,
            detail: message.into(),
        });
    }

    fn push_history(&mut self, entry: HistoryEntry) {
        self.history.push(entry);
        while self.history.len() > HISTORY_CAP {
            self.history.remove(0);
        }
        self.transcript_scroll = 0;
    }

    /// Kick off the configured `rv-vectorize` invocation on a background
    /// thread so the interface can keep animating a spinner instead of
    /// freezing until the child process exits.
    fn execute(&mut self) {
        if self.worker.is_some() {
            // A run is already in flight; ignore the request rather than
            // starting a second overlapping child process.
            return;
        }
        if self.input.value.trim().is_empty() || self.output.value.trim().is_empty() {
            self.push_immediate_failure("Input and output paths are required.");
            return;
        }

        let executable = match cli_executable() {
            Ok(executable) => executable,
            Err(error) => {
                self.push_immediate_failure(error);
                return;
            }
        };
        let output_path = PathBuf::from(&self.output.value);
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = fs::create_dir_all(parent) {
                    self.push_immediate_failure(format!(
                        "Cannot create output directory {}: {error}",
                        parent.display()
                    ));
                    return;
                }
            }
        }

        let arguments = self.command_arguments();
        let command_display = format!(
            "{} {}",
            executable.display(),
            arguments
                .iter()
                .map(|argument| shell_quote(argument))
                .collect::<Vec<_>>()
                .join(" ")
        );
        self.push_history(HistoryEntry {
            command: command_display,
            status: EntryStatus::Running,
            detail: String::new(),
        });

        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let outcome = match Command::new(&executable).args(&arguments).output() {
                Ok(result) => WorkerResult {
                    success: result.status.success(),
                    code: result.status.code(),
                    stdout: String::from_utf8_lossy(&result.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
                    spawn_error: None,
                },
                Err(error) => WorkerResult {
                    success: false,
                    code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    spawn_error: Some(error.to_string()),
                },
            };
            let _ = sender.send(outcome);
        });
        self.worker = Some(receiver);
    }

    /// Advance the spinner while a background run is in flight, and check
    /// whether that run has just finished.
    fn tick(&mut self) {
        if self.worker.is_none() {
            return;
        }
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER.len();

        let finished = self
            .worker
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = finished else {
            return;
        };
        self.worker = None;
        if let Some(entry) = self.history.last_mut() {
            if let Some(error) = result.spawn_error {
                entry.status = EntryStatus::Failed;
                entry.detail = format!("Could not start rv-vectorize: {error}");
                return;
            }
            let mut detail = if result.success {
                entry.status = EntryStatus::Success;
                "wrote output successfully.".to_owned()
            } else {
                entry.status = EntryStatus::Failed;
                format!(
                    "rv-vectorize exited with status {}.",
                    result
                        .code
                        .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                )
            };
            if !result.stderr.trim().is_empty() {
                detail.push('\n');
                detail.push_str(result.stderr.trim());
            }
            if !result.stdout.trim().is_empty() {
                detail.push_str("\n\nstdout:\n");
                detail.push_str(result.stdout.trim());
            }
            entry.detail = detail;
        }
    }
}

fn cli_executable() -> Result<PathBuf, String> {
    let current =
        env::current_exe().map_err(|error| format!("Cannot locate TUI binary: {error}"))?;
    let directory = current
        .parent()
        .ok_or_else(|| "Cannot determine the executable directory.".to_owned())?;
    let sibling = directory.join(format!("rv-vectorize{}", env::consts::EXE_SUFFIX));
    sibling.is_file().then_some(sibling).ok_or_else(|| {
        "The rv-vectorize CLI is not beside the TUI. Run cargo build --release.".to_owned()
    })
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_+-.,/:=".contains(&byte))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn selected_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(INK)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TEXT)
    }
}

fn field_line<'a>(label: &'a str, value: &'a str, selected: bool) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::default().fg(MUTED)),
        Span::styled(value, selected_style(selected)),
    ])
}

fn toggle_line(label: &'static str, enabled: bool, selected: bool) -> Line<'static> {
    field_line(
        label,
        if enabled {
            "[x] enabled"
        } else {
            "[ ] disabled"
        },
        selected,
    )
}

/// Render one transcript entry as a few chat-like lines: the invoked
/// command, its status (or a live spinner), and any captured output.
fn history_lines(entry: &HistoryEntry, spinner: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !entry.command.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("❯ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(entry.command.clone(), Style::default().fg(MUTED)),
        ]));
    }
    match entry.status {
        EntryStatus::Running => {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{spinner} "),
                    Style::default().fg(PENDING).add_modifier(Modifier::BOLD),
                ),
                Span::styled("running…", Style::default().fg(PENDING)),
            ]));
        }
        EntryStatus::Success => {
            lines.push(Line::from(Span::styled(
                "✔ success",
                Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD),
            )));
        }
        EntryStatus::Failed => {
            lines.push(Line::from(Span::styled(
                "✘ failed",
                Style::default().fg(FAILURE).add_modifier(Modifier::BOLD),
            )));
        }
    }
    for detail_line in entry.detail.lines() {
        lines.push(Line::from(Span::styled(
            format!("  {detail_line}"),
            Style::default().fg(MUTED),
        )));
    }
    lines.push(Line::raw(""));
    lines
}

fn draw(frame: &mut Frame, app: &App) {
    let [header_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let [form_area, transcript_area] =
        Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)])
            .areas(content_area);

    let header = Paragraph::new(Line::from(vec![
        Span::styled("✳ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(
            "rv-vectorize",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  research LLVM loop vectorizer", Style::default().fg(MUTED)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT_DIM)),
    );
    frame.render_widget(header, header_area);

    let lines = vec![
        field_line("Input", &app.input.value, app.focused == 0),
        field_line("Output", &app.output.value, app.focused == 1),
        Line::raw(""),
        field_line("Policy", POLICIES[app.policy], app.focused == 2),
        field_line(
            "Vector width",
            VECTOR_WIDTHS[app.vector_width],
            app.focused == 3,
        ),
        toggle_line("Report", app.report, app.focused == 4),
        toggle_line("Verify", app.verify, app.focused == 5),
        toggle_line("Bitcode", app.emit_bitcode, app.focused == 6),
        Line::raw(""),
        field_line("Action", "[ Run vectorizer ]", app.focused == 7),
    ];
    let form = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" Configuration ")
                .title_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(ACCENT_DIM)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(form, form_area);

    let border_color = match (app.worker.is_some(), app.history.last()) {
        (true, _) => PENDING,
        (false, Some(entry)) => match entry.status {
            EntryStatus::Running => PENDING,
            EntryStatus::Success => SUCCESS,
            EntryStatus::Failed => FAILURE,
        },
        (false, None) => ACCENT_DIM,
    };
    let spinner = SPINNER[app.spinner_frame];
    let mut transcript_lines = Vec::new();
    if app.history.is_empty() {
        transcript_lines.push(Line::styled(
            "No runs yet. Configure the pass, then press Enter on Run or Ctrl-R.",
            Style::default().fg(MUTED),
        ));
    } else {
        for entry in &app.history {
            transcript_lines.extend(history_lines(entry, spinner));
        }
    }
    let transcript = Paragraph::new(Text::from(transcript_lines))
        .block(
            Block::default()
                .title(" Session ")
                .title_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.transcript_scroll, 0));
    frame.render_widget(transcript, transcript_area);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled(" ❯ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
        "tab/↑↓".fg(ACCENT).bold(),
        Span::styled(" next  ", Style::default().fg(MUTED)),
        "←→".fg(ACCENT).bold(),
        Span::styled(" change  ", Style::default().fg(MUTED)),
        "enter/space".fg(ACCENT).bold(),
        Span::styled(" select  ", Style::default().fg(MUTED)),
        "ctrl-r".fg(ACCENT).bold(),
        Span::styled(" run  ", Style::default().fg(MUTED)),
        "esc".fg(ACCENT).bold(),
        Span::styled(" quit ", Style::default().fg(MUTED)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT_DIM)),
    );
    frame.render_widget(footer, footer_area);

    draw_cursor(frame, app, form_area);
}

fn draw_cursor(frame: &mut Frame, app: &App, area: Rect) {
    let field = match app.focused {
        0 => &app.input,
        1 => &app.output,
        _ => return,
    };
    let row = u16::try_from(app.focused).unwrap_or_default();
    let cursor = u16::try_from(field.value[..field.cursor].chars().count()).unwrap_or(u16::MAX);
    let x = area.x.saturating_add(13).saturating_add(cursor);
    let y = area.y.saturating_add(1).saturating_add(row);
    if x < area.right().saturating_sub(1) && y < area.bottom().saturating_sub(1) {
        frame.set_cursor_position((x, y));
    }
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App::default();
    while !app.quit {
        terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }
        app.tick();
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn text_editor_handles_unicode_boundaries() {
        let mut field = TextField::new("aλ");
        field.move_left();
        field.backspace();
        assert_eq!(field.value, "λ");
        assert_eq!(field.cursor, 0);
        field.delete();
        assert!(field.value.is_empty());
    }

    #[test]
    fn command_uses_policy_for_automatic_width() {
        let app = App::default();
        assert_eq!(
            app.command_arguments(),
            [
                "--policy",
                "balanced",
                "--report",
                "tests/fixtures/vectorizable.ll",
                "-o",
                "build/tui-vectorized.ll"
            ]
        );
    }

    #[test]
    fn command_uses_forced_width_and_toggles() {
        let app = App {
            vector_width: 3,
            verify: false,
            emit_bitcode: true,
            ..App::default()
        };
        let arguments = app.command_arguments();
        assert!(arguments.windows(2).any(|pair| pair == ["--vf", "8"]));
        assert!(arguments.iter().any(|argument| argument == "--no-verify"));
        assert!(
            arguments
                .iter()
                .any(|argument| argument == "--emit-bitcode")
        );
        assert!(!arguments.iter().any(|argument| argument == "--policy"));
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let mut app = App::default();
        app.previous_field();
        assert_eq!(app.focused, FIELD_COUNT - 1);
        app.next_field();
        assert_eq!(app.focused, 0);
    }

    #[test]
    fn selectors_cycle_in_both_directions() {
        let mut app = App {
            focused: 2,
            ..App::default()
        };
        app.cycle_selected(false);
        assert_eq!(POLICIES[app.policy], "aggressive");
        app.cycle_selected(true);
        assert_eq!(POLICIES[app.policy], "balanced");

        app.focused = 3;
        app.cycle_selected(false);
        assert_eq!(VECTOR_WIDTHS[app.vector_width], "16");
    }

    #[test]
    fn immediate_validation_failure_is_recorded_without_spawning_a_worker() {
        let mut app = App {
            input: TextField::new(""),
            ..App::default()
        };
        app.execute();
        assert!(app.worker.is_none());
        assert_eq!(app.history.len(), 1);
        assert_eq!(app.history[0].status, EntryStatus::Failed);
    }

    #[test]
    fn history_is_capped() {
        let mut app = App::default();
        for _ in 0..(HISTORY_CAP + 3) {
            app.push_history(HistoryEntry {
                command: "x".to_owned(),
                status: EntryStatus::Success,
                detail: String::new(),
            });
        }
        assert_eq!(app.history.len(), HISTORY_CAP);
    }

    #[test]
    fn renders_at_typical_terminal_size() {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &App::default()))
            .expect("render TUI");
        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("rv-vectorize"));
        assert!(rendered.contains("Configuration"));
        assert!(rendered.contains("Session"));
        assert!(rendered.contains("No runs yet"));
    }
}
