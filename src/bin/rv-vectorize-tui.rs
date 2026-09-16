use std::cell::Cell;
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use similar::{DiffOp, TextDiff};

const POLICIES: [&str; 3] = ["balanced", "conservative", "aggressive"];
const VECTOR_WIDTHS: [&str; 5] = ["auto", "2", "4", "8", "16"];
const FIELD_COUNT: usize = 9;
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
const PENDING_DIM: Color = Color::Rgb(0x6E, 0x56, 0x30);

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

/// Which screen is currently shown: the configuration form (with the run
/// transcript beside it), or the side-by-side before/after diff.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewMode {
    Config,
    Diff,
}

/// How a single diff row should be colored, GitHub-review style.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowKind {
    Context,
    Removed,
    Added,
    Empty,
}

/// One aligned row of a side-by-side diff: the original file's line (if
/// any) paired with the optimized file's line (if any) at the same row.
#[derive(Clone, Debug)]
struct DiffRow {
    left_no: Option<usize>,
    left_text: String,
    left_kind: RowKind,
    right_no: Option<usize>,
    right_text: String,
    right_kind: RowKind,
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
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
    /// Stick to the newest output until the reader scrolls up by hand.
    transcript_follow: bool,
    /// Wrapped row count and visible height measured by the last render. The
    /// draw path is the only place the real geometry is known, so scrolling
    /// clamps against what was actually shown.
    transcript_content: Cell<u16>,
    transcript_viewport: Cell<u16>,
    spinner_frame: usize,
    tick_count: u64,
    worker: Option<Receiver<WorkerResult>>,
    run_input: String,
    run_output: String,
    view: ViewMode,
    diff_rows: Vec<DiffRow>,
    diff_labels: Option<(String, String)>,
    diff_scroll: u16,
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
            .field("view", &self.view)
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
            transcript_follow: true,
            transcript_content: Cell::new(0),
            transcript_viewport: Cell::new(0),
            spinner_frame: 0,
            tick_count: 0,
            worker: None,
            run_input: String::new(),
            run_output: String::new(),
            view: ViewMode::Config,
            diff_rows: Vec::new(),
            diff_labels: None,
            diff_scroll: 0,
            quit: false,
        }
    }
}

impl App {
    /// Largest useful scroll offset for the session pane, from the geometry
    /// measured during the previous render.
    fn transcript_max_scroll(&self) -> u16 {
        self.transcript_content
            .get()
            .saturating_sub(self.transcript_viewport.get())
    }

    /// Offset to render with. Following pins the view to the newest output;
    /// otherwise a stale offset is clamped so the pane can never scroll off
    /// into empty space below the transcript.
    fn transcript_offset(&self) -> u16 {
        if self.transcript_follow {
            self.transcript_max_scroll()
        } else {
            self.transcript_scroll.min(self.transcript_max_scroll())
        }
    }

    fn scroll_transcript(&mut self, delta: i32) {
        let maximum = self.transcript_max_scroll();
        let current = i64::from(self.transcript_offset());
        let target = (current + i64::from(delta)).clamp(0, i64::from(maximum));
        self.transcript_scroll = u16::try_from(target).unwrap_or(maximum);
        // Reaching the bottom re-arms following, so a reader who scrolls back
        // down keeps seeing new output without pressing End.
        self.transcript_follow = self.transcript_scroll >= maximum;
    }

    fn scroll_transcript_to_top(&mut self) {
        self.transcript_scroll = 0;
        self.transcript_follow = self.transcript_max_scroll() == 0;
    }

    fn scroll_transcript_to_bottom(&mut self) {
        self.transcript_scroll = self.transcript_max_scroll();
        self.transcript_follow = true;
    }

    fn handle_mouse(&mut self, event: MouseEvent) {
        let delta = match event.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => return,
        };
        if self.view == ViewMode::Diff {
            self.diff_scroll = if delta < 0 {
                self.diff_scroll.saturating_sub(3)
            } else {
                self.diff_scroll.saturating_add(3)
            };
            return;
        }
        self.scroll_transcript(delta);
    }

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

    /// Flip between the configuration screen and the diff screen. A no-op
    /// until at least one run has produced a diff to show.
    fn toggle_diff_view(&mut self) {
        if self.diff_labels.is_none() {
            return;
        }
        self.view = match self.view {
            ViewMode::Config => ViewMode::Diff,
            ViewMode::Diff => ViewMode::Config,
        };
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
            self.toggle_diff_view();
            return;
        }

        if self.view == ViewMode::Diff {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.view = ViewMode::Config,
                KeyCode::Up => self.diff_scroll = self.diff_scroll.saturating_sub(1),
                KeyCode::Down => self.diff_scroll = self.diff_scroll.saturating_add(1),
                KeyCode::PageUp => self.diff_scroll = self.diff_scroll.saturating_sub(10),
                KeyCode::PageDown => self.diff_scroll = self.diff_scroll.saturating_add(10),
                _ => {}
            }
            return;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.execute();
            return;
        }
        match key.code {
            KeyCode::Esc => self.quit = true,
            // Ctrl with the arrow keys scrolls the session pane line by line,
            // leaving the bare arrows for moving between controls.
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_transcript(-1);
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_transcript(1);
            }
            KeyCode::Tab | KeyCode::Down => self.next_field(),
            KeyCode::BackTab | KeyCode::Up => self.previous_field(),
            KeyCode::PageUp => self.scroll_transcript(-5),
            KeyCode::PageDown => self.scroll_transcript(5),
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
            // On a text control these move the caret; anywhere else they jump
            // the session pane to the oldest or newest output.
            KeyCode::Home => {
                if let Some(field) = self.selected_text_field() {
                    field.cursor = 0;
                } else {
                    self.scroll_transcript_to_top();
                }
            }
            KeyCode::End => {
                if let Some(field) = self.selected_text_field() {
                    field.cursor = field.value.len();
                } else {
                    self.scroll_transcript_to_bottom();
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
            KeyCode::Enter if self.focused == 8 => self.toggle_diff_view(),
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
        // New output jumps back to the newest entry rather than to the top of
        // the transcript, which is what a reader watching a run expects.
        self.transcript_follow = true;
    }

    /// Kick off the configured `rv-vectorize` invocation on a background
    /// thread so the interface can keep animating instead of freezing
    /// until the child process exits.
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

        self.run_input = self.input.value.clone();
        self.run_output = self.output.value.clone();

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

    /// Advance the animations every frame, and check whether a background
    /// run has just finished.
    fn tick(&mut self) {
        self.tick_count = self.tick_count.wrapping_add(1);
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
        if result.success {
            self.load_diff();
        }
    }

    /// Read the input and (now freshly written) output IR back off disk
    /// and diff them for the side-by-side view. Silently leaves the diff
    /// empty if either file cannot be read as text (for example, when
    /// `--emit-bitcode` was used).
    fn load_diff(&mut self) {
        let (Ok(original), Ok(optimized)) = (
            fs::read_to_string(&self.run_input),
            fs::read_to_string(&self.run_output),
        ) else {
            return;
        };
        self.diff_rows = build_diff_rows(&original, &optimized);
        self.diff_labels = Some((self.run_input.clone(), self.run_output.clone()));
        self.diff_scroll = 0;
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

/// Build the aligned rows of a side-by-side diff from two whole-file
/// strings, GitHub-review style: unchanged lines stay level on both
/// sides, and removed/added lines pad the other side with a blank row so
/// everything stays aligned row-for-row.
fn build_diff_rows(original: &str, optimized: &str) -> Vec<DiffRow> {
    let old_lines: Vec<&str> = original.lines().collect();
    let new_lines: Vec<&str> = optimized.lines().collect();
    let diff = TextDiff::from_lines(original, optimized);
    let mut rows = Vec::new();

    for op in diff.ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for offset in 0..len {
                    let text = old_lines.get(old_index + offset).copied().unwrap_or("");
                    rows.push(DiffRow {
                        left_no: Some(old_index + offset + 1),
                        left_text: text.to_owned(),
                        left_kind: RowKind::Context,
                        right_no: Some(new_index + offset + 1),
                        right_text: text.to_owned(),
                        right_kind: RowKind::Context,
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for offset in 0..old_len {
                    rows.push(DiffRow {
                        left_no: Some(old_index + offset + 1),
                        left_text: old_lines
                            .get(old_index + offset)
                            .copied()
                            .unwrap_or("")
                            .to_owned(),
                        left_kind: RowKind::Removed,
                        right_no: None,
                        right_text: String::new(),
                        right_kind: RowKind::Empty,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for offset in 0..new_len {
                    rows.push(DiffRow {
                        left_no: None,
                        left_text: String::new(),
                        left_kind: RowKind::Empty,
                        right_no: Some(new_index + offset + 1),
                        right_text: new_lines
                            .get(new_index + offset)
                            .copied()
                            .unwrap_or("")
                            .to_owned(),
                        right_kind: RowKind::Added,
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let rows_needed = old_len.max(new_len);
                for offset in 0..rows_needed {
                    let (left_no, left_text, left_kind) = if offset < old_len {
                        (
                            Some(old_index + offset + 1),
                            old_lines
                                .get(old_index + offset)
                                .copied()
                                .unwrap_or("")
                                .to_owned(),
                            RowKind::Removed,
                        )
                    } else {
                        (None, String::new(), RowKind::Empty)
                    };
                    let (right_no, right_text, right_kind) = if offset < new_len {
                        (
                            Some(new_index + offset + 1),
                            new_lines
                                .get(new_index + offset)
                                .copied()
                                .unwrap_or("")
                                .to_owned(),
                            RowKind::Added,
                        )
                    } else {
                        (None, String::new(), RowKind::Empty)
                    };
                    rows.push(DiffRow {
                        left_no,
                        left_text,
                        left_kind,
                        right_no,
                        right_text,
                        right_kind,
                    });
                }
            }
        }
    }
    rows
}

fn diff_pane_lines(rows: &[DiffRow], side: Side) -> Vec<Line<'static>> {
    rows.iter()
        .map(|row| {
            let (number, text, kind) = match side {
                Side::Left => (row.left_no, &row.left_text, row.left_kind),
                Side::Right => (row.right_no, &row.right_text, row.right_kind),
            };
            let (marker, color) = match kind {
                RowKind::Context => (" ", TEXT),
                RowKind::Removed => ("-", FAILURE),
                RowKind::Added => ("+", SUCCESS),
                RowKind::Empty => (" ", MUTED),
            };
            let gutter = number.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
            Line::from(vec![
                Span::styled(format!("{gutter} {marker} "), Style::default().fg(MUTED)),
                Span::styled(text.clone(), Style::default().fg(color)),
            ])
        })
        .collect()
}

fn file_label(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.to_owned(), ToOwned::to_owned)
}

/// Linearly interpolate between two RGB colors; falls back to `a` for any
/// non-RGB color (every palette constant above is RGB, so this only ever
/// takes the fast path in practice).
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let Color::Rgb(ar, ag, ab) = a else { return a };
    let Color::Rgb(br, bg, bb) = b else { return a };
    let t = t.clamp(0.0, 1.0);
    let mix = |from: u8, to: u8| (f32::from(from) + (f32::from(to) - f32::from(from)) * t) as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// A moving highlight sweeping across `text`, the same shimmering-text
/// effect Claude's own CLI uses while it's working, built from a per-
/// character sine wave so it needs no extra crate or animation state.
fn shimmer_spans(text: &str, tick: u64, dim: Color, bright: Color) -> Vec<Span<'static>> {
    text.chars()
        .enumerate()
        .map(|(index, character)| {
            let phase = (index as f32).mul_add(0.6, -(tick as f32) * 0.35);
            let brightness = phase.sin().mul_add(0.5, 0.5);
            Span::styled(
                character.to_string(),
                Style::default().fg(lerp_color(dim, bright, brightness)),
            )
        })
        .collect()
}

/// A slow "breathing" pulse between two colors, used on the header icon
/// so the interface feels alive even when idle.
fn breathing_color(tick: u64, dim: Color, bright: Color) -> Color {
    let brightness = ((tick as f32) * 0.08).sin().mul_add(0.5, 0.5);
    lerp_color(dim, bright, brightness)
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
    let (marker, marker_color) = if enabled {
        ("● On ", SUCCESS)
    } else {
        ("○ Off", MUTED)
    };
    let bracket_color = if selected { ACCENT } else { ACCENT_DIM };
    let pill_style = if selected {
        Style::default()
            .fg(INK)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(marker_color)
    };
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::default().fg(MUTED)),
        Span::styled("[ ", Style::default().fg(bracket_color)),
        Span::styled(marker, pill_style),
        Span::styled(" ]", Style::default().fg(bracket_color)),
    ])
}

/// A row of options rendered like a segmented control (all choices
/// visible at once, the active one filled in), rather than hiding every
/// option behind a single cycling value. Takes its own line below a
/// `label_line`, so it has the panel's full width to work with instead
/// of competing with a label prefix.
fn segmented_line<'a>(options: &'a [&'a str], current: usize, selected: bool) -> Line<'a> {
    let mut spans = vec![Span::raw("  ")];
    for (index, option) in options.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("│", Style::default().fg(ACCENT_DIM)));
        }
        let is_current = index == current;
        let style = match (is_current, selected) {
            (true, true) => Style::default()
                .fg(INK)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            (false, _) => Style::default().fg(MUTED),
        };
        spans.push(Span::styled(format!(" {option} "), style));
    }
    Line::from(spans)
}

fn label_line(label: &'static str) -> Line<'static> {
    Line::styled(label, Style::default().fg(MUTED))
}

/// Render a real bordered button widget (not just inline text) — a
/// rounded box that fills solid with the accent color while focused, and
/// dims down when its action isn't currently available.
fn draw_button(
    frame: &mut Frame,
    area: Rect,
    icon: &str,
    label: &str,
    selected: bool,
    enabled: bool,
) {
    let block = if selected && enabled {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT))
            .style(Style::default().bg(ACCENT))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT_DIM))
    };
    let text_style = if selected && enabled {
        Style::default().fg(INK).add_modifier(Modifier::BOLD)
    } else if enabled {
        Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    let button = Paragraph::new(Line::from(Span::styled(
        format!("{icon} {label}"),
        text_style,
    )))
    .alignment(Alignment::Center)
    .block(block);
    frame.render_widget(button, area);
}

/// Render one transcript entry as a few chat-like lines: the invoked
/// command, its status (or a live spinner and shimmer), and any captured
/// output.
fn history_lines(entry: &HistoryEntry, spinner: &str, tick: u64) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !entry.command.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(
                "❯ ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(entry.command.clone(), Style::default().fg(MUTED)),
        ]));
    }
    match entry.status {
        EntryStatus::Running => {
            let mut spans = vec![Span::styled(
                format!("{spinner} "),
                Style::default().fg(PENDING).add_modifier(Modifier::BOLD),
            )];
            spans.extend(shimmer_spans("running…", tick, PENDING_DIM, PENDING));
            lines.push(Line::from(spans));
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

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let icon_color = breathing_color(app.tick_count, ACCENT_DIM, ACCENT);
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "✳ ",
            Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "rv-vectorize",
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  research LLVM loop vectorizer",
            Style::default().fg(MUTED),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT_DIM)),
    );
    frame.render_widget(header, area);
}

fn draw_config(frame: &mut Frame, app: &App, form_area: Rect, transcript_area: Rect) -> Rect {
    let config_block = Block::default()
        .title(" Configuration ")
        .title_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT_DIM))
        .padding(Padding::new(1, 1, 0, 0));
    let inner = config_block.inner(form_area);
    frame.render_widget(config_block, form_area);

    let [fields_area, buttons_area] =
        Layout::vertical([Constraint::Length(10), Constraint::Min(7)]).areas(inner);

    let lines = vec![
        field_line("Input", &app.input.value, app.focused == 0),
        field_line("Output", &app.output.value, app.focused == 1),
        Line::raw(""),
        label_line("Policy"),
        segmented_line(&POLICIES, app.policy, app.focused == 2),
        label_line("Vector width"),
        segmented_line(&VECTOR_WIDTHS, app.vector_width, app.focused == 3),
        toggle_line("Report", app.report, app.focused == 4),
        toggle_line("Verify", app.verify, app.focused == 5),
        toggle_line("Bitcode", app.emit_bitcode, app.focused == 6),
    ];
    // Deliberately not wrapped: every entry above is exactly one row, and
    // draw_cursor()/the field indices below assume that row == field
    // index for the text fields. Wrapping a too-long value would push
    // every row after it down and misalign both the cursor and the
    // lower toggles, so a too-long value is clipped instead.
    let fields = Paragraph::new(lines);
    frame.render_widget(fields, fields_area);

    let [run_area, _gap, diff_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(3),
    ])
    .areas(buttons_area);
    draw_button(
        frame,
        run_area,
        "▶",
        "Run vectorizer",
        app.focused == 7,
        true,
    );
    let diff_label = if app.view == ViewMode::Diff {
        "Hide diff"
    } else {
        "View diff"
    };
    draw_button(
        frame,
        diff_area,
        "▤",
        diff_label,
        app.focused == 8,
        app.diff_labels.is_some(),
    );

    draw_transcript(frame, app, transcript_area);

    fields_area
}

/// Renders the session transcript, which is the one scrollable pane on the
/// configuration screen.
fn draw_transcript(frame: &mut Frame, app: &App, transcript_area: Rect) {
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
            transcript_lines.extend(history_lines(entry, spinner, app.tick_count));
        }
    }
    // Wrap here rather than leaving it to Paragraph, so the rendered row count
    // is known exactly and scrolling can be clamped to real content.
    let inner_width = transcript_area.width.saturating_sub(4);
    let wrapped = wrap_lines(&transcript_lines, inner_width);
    let viewport = transcript_area.height.saturating_sub(2);
    app.transcript_content
        .set(u16::try_from(wrapped.len()).unwrap_or(u16::MAX));
    app.transcript_viewport.set(viewport);
    let offset = app.transcript_offset();

    let title = if app.transcript_content.get() > viewport && !app.transcript_follow {
        " Session (scrolled) "
    } else {
        " Session "
    };
    let transcript = Paragraph::new(Text::from(wrapped))
        .block(
            Block::default()
                .title(title)
                .title_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color))
                .padding(Padding::new(1, 1, 0, 0)),
        )
        .scroll((offset, 0));
    frame.render_widget(transcript, transcript_area);

    if app.transcript_content.get() > viewport {
        let mut state = ScrollbarState::new(usize::from(app.transcript_max_scroll()))
            .position(usize::from(offset));
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("│"))
                .thumb_symbol("█")
                .track_style(Style::default().fg(ACCENT_DIM))
                .thumb_style(Style::default().fg(ACCENT)),
            transcript_area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }
}

/// Greedily word-wraps styled lines to `width`, preserving each span's style
/// and hard-splitting words that cannot fit on a line of their own.
fn wrap_lines(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return lines.to_vec();
    }
    let limit = usize::from(width);
    let mut wrapped = Vec::new();

    for line in lines {
        let mut current: Vec<Span<'static>> = Vec::new();
        let mut used = 0_usize;

        for span in &line.spans {
            let style = span.style;
            for (index, word) in span.content.split(' ').enumerate() {
                // `split` yields an empty leading item for a leading space, so
                // index > 0 is exactly "there was a separator before this".
                if index > 0 && used < limit && !current.is_empty() {
                    current.push(Span::styled(" ".to_owned(), style));
                    used += 1;
                }
                let mut rest = word;
                while !rest.is_empty() {
                    let remaining = limit.saturating_sub(used);
                    if remaining == 0 {
                        wrapped.push(Line::from(std::mem::take(&mut current)));
                        used = 0;
                        continue;
                    }
                    let take = rest.chars().count().min(remaining);
                    let split_at = rest
                        .char_indices()
                        .nth(take)
                        .map_or(rest.len(), |(offset, _)| offset);
                    let (head, tail) = rest.split_at(split_at);
                    current.push(Span::styled(head.to_owned(), style));
                    used += take;
                    rest = tail;
                    if !rest.is_empty() {
                        wrapped.push(Line::from(std::mem::take(&mut current)));
                        used = 0;
                    }
                }
            }
        }
        // Always emit the line, so blank separator lines keep their height.
        wrapped.push(Line::from(current));
    }
    wrapped
}

fn draw_diff(frame: &mut Frame, app: &App, area: Rect) {
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    let (input_label, output_label) = app
        .diff_labels
        .as_ref()
        .map_or((String::new(), String::new()), |(input, output)| {
            (file_label(input), file_label(output))
        });

    let left = Paragraph::new(Text::from(diff_pane_lines(&app.diff_rows, Side::Left)))
        .block(
            Block::default()
                .title(format!(" {input_label} (original) "))
                .title_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(FAILURE))
                .padding(Padding::new(1, 1, 0, 0)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.diff_scroll, 0));
    frame.render_widget(left, left_area);

    let right = Paragraph::new(Text::from(diff_pane_lines(&app.diff_rows, Side::Right)))
        .block(
            Block::default()
                .title(format!(" {output_label} (optimized) "))
                .title_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD))
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(SUCCESS))
                .padding(Padding::new(1, 1, 0, 0)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.diff_scroll, 0));
    frame.render_widget(right, right_area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let spans = if app.view == ViewMode::Diff {
        vec![
            Span::styled(
                " ❯ ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            "↑↓/pgup/pgdn".fg(ACCENT).bold(),
            Span::styled(" scroll  ", Style::default().fg(MUTED)),
            "d/esc".fg(ACCENT).bold(),
            Span::styled(" back  ", Style::default().fg(MUTED)),
            "ctrl-c".fg(ACCENT).bold(),
            Span::styled(" quit ", Style::default().fg(MUTED)),
        ]
    } else {
        let mut spans = vec![
            Span::styled(
                " ❯ ",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            "tab/↑↓".fg(ACCENT).bold(),
            Span::styled(" next  ", Style::default().fg(MUTED)),
            "←→".fg(ACCENT).bold(),
            Span::styled(" change  ", Style::default().fg(MUTED)),
            "enter/space".fg(ACCENT).bold(),
            Span::styled(" select  ", Style::default().fg(MUTED)),
            "ctrl-r".fg(ACCENT).bold(),
            Span::styled(" run  ", Style::default().fg(MUTED)),
            "wheel/pgup/pgdn".fg(ACCENT).bold(),
            Span::styled(" scroll  ", Style::default().fg(MUTED)),
        ];
        if app.diff_labels.is_some() {
            spans.push("ctrl-d".fg(ACCENT).bold());
            spans.push(Span::styled(" diff  ", Style::default().fg(MUTED)));
        }
        spans.push("esc".fg(ACCENT).bold());
        spans.push(Span::styled(" quit ", Style::default().fg(MUTED)));
        spans
    };
    let footer = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(ACCENT_DIM)),
    );
    frame.render_widget(footer, area);
}

fn draw(frame: &mut Frame, app: &App) {
    let [header_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    draw_header(frame, app, header_area);

    match app.view {
        ViewMode::Config => {
            let [form_area, transcript_area] =
                Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)])
                    .areas(content_area);
            let fields_area = draw_config(frame, app, form_area, transcript_area);
            draw_cursor(frame, app, fields_area);
        }
        ViewMode::Diff => draw_diff(frame, app, content_area),
    }

    draw_footer(frame, app, footer_area);
}

fn draw_cursor(frame: &mut Frame, app: &App, fields_area: Rect) {
    let field = match app.focused {
        0 => &app.input,
        1 => &app.output,
        _ => return,
    };
    let row = u16::try_from(app.focused).unwrap_or_default();
    let cursor = u16::try_from(field.value[..field.cursor].chars().count()).unwrap_or(u16::MAX);
    let x = fields_area.x.saturating_add(12).saturating_add(cursor);
    let y = fields_area.y.saturating_add(row);
    if x < fields_area.right() && y < fields_area.bottom() {
        frame.set_cursor_position((x, y));
    }
}

fn run(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let mut app = App::default();
    while !app.quit {
        terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                _ => {}
            }
        }
        app.tick();
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut terminal = ratatui::init();
    // Wheel events require mouse reporting. While it is on, the terminal's own
    // drag-select is suppressed; most terminals still select with Shift held.
    let mouse = execute!(io::stdout(), EnableMouseCapture);
    let result = run(&mut terminal);
    if mouse.is_ok() {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
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

    /// Pretend the last render measured `content` rows in a `viewport`-row pane.
    fn measured(content: u16, viewport: u16) -> App {
        let app = App::default();
        app.transcript_content.set(content);
        app.transcript_viewport.set(viewport);
        app
    }

    #[test]
    fn transcript_scroll_is_clamped_to_measured_content() {
        let mut app = measured(50, 20);
        assert_eq!(app.transcript_max_scroll(), 30);
        app.scroll_transcript(1000);
        assert_eq!(app.transcript_scroll, 30);
        app.scroll_transcript(-1000);
        assert_eq!(app.transcript_scroll, 0);
    }

    #[test]
    fn short_transcripts_cannot_scroll_at_all() {
        let mut app = measured(5, 20);
        app.scroll_transcript(10);
        assert_eq!(app.transcript_offset(), 0);
        assert_eq!(app.transcript_max_scroll(), 0);
    }

    #[test]
    fn scrolling_up_stops_following_and_returning_to_the_bottom_resumes_it() {
        let mut app = measured(50, 20);
        assert!(app.transcript_follow);
        app.scroll_transcript(-5);
        assert!(!app.transcript_follow);
        assert_eq!(app.transcript_offset(), 25);
        app.scroll_transcript(5);
        assert!(app.transcript_follow);
    }

    #[test]
    fn new_output_returns_to_the_newest_entry() {
        let mut app = measured(50, 20);
        app.scroll_transcript_to_top();
        assert!(!app.transcript_follow);
        app.push_history(HistoryEntry {
            command: "x".to_owned(),
            status: EntryStatus::Success,
            detail: String::new(),
        });
        assert!(app.transcript_follow);
        assert_eq!(app.transcript_offset(), app.transcript_max_scroll());
    }

    #[test]
    fn a_grown_transcript_keeps_a_pinned_offset_in_range() {
        let mut app = measured(50, 20);
        app.scroll_transcript(-10);
        assert_eq!(app.transcript_offset(), 20);
        // The pane shrinks; the stale offset must not point past the content.
        app.transcript_content.set(25);
        assert_eq!(app.transcript_offset(), 5);
    }

    #[test]
    fn mouse_wheel_scrolls_the_session_pane() {
        let mut app = measured(50, 20);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.transcript_offset(), 27);
        assert!(!app.transcript_follow);
    }

    #[test]
    fn wrapping_splits_long_lines_and_preserves_blank_rows() {
        let lines = vec![
            Line::from("aaa bbb ccc"),
            Line::from(""),
            Line::from("short"),
        ];
        let wrapped = wrap_lines(&lines, 7);
        let rendered: Vec<String> = wrapped
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(rendered, vec!["aaa bbb", "ccc", "", "short"]);
    }

    #[test]
    fn wrapping_hard_splits_a_word_longer_than_the_pane() {
        let lines = vec![Line::from("abcdefghij")];
        let wrapped = wrap_lines(&lines, 4);
        assert_eq!(wrapped.len(), 3);
    }

    #[test]
    fn zero_width_wrapping_does_not_loop_forever() {
        let lines = vec![Line::from("anything at all")];
        assert_eq!(wrap_lines(&lines, 0).len(), 1);
    }

    #[test]
    fn diff_view_is_unreachable_without_a_completed_run() {
        let mut app = App::default();
        app.toggle_diff_view();
        assert_eq!(app.view, ViewMode::Config);
    }

    #[test]
    fn diff_rows_align_context_additions_and_removals() {
        let original = "a\nb\nc\n";
        let optimized = "a\nb2\nc\nd\n";
        let rows = build_diff_rows(original, optimized);

        // "a" and "c" are unchanged context lines present on both sides.
        assert!(rows.iter().any(|row| row.left_kind == RowKind::Context
            && row.left_text == "a"
            && row.right_text == "a"));
        // "b" -> "b2" shows as a removal paired with an addition.
        assert!(
            rows.iter()
                .any(|row| row.left_kind == RowKind::Removed && row.left_text == "b")
        );
        assert!(
            rows.iter()
                .any(|row| row.right_kind == RowKind::Added && row.right_text == "b2")
        );
        // The trailing "d" is a pure addition with an empty left side.
        assert!(rows.iter().any(|row| row.right_text == "d"
            && row.right_kind == RowKind::Added
            && row.left_kind == RowKind::Empty));
    }

    #[test]
    fn view_diff_button_only_activates_once_a_diff_exists() {
        let mut app = App {
            focused: 8,
            ..App::default()
        };
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.view, ViewMode::Config);

        app.diff_labels = Some(("in.ll".to_owned(), "out.ll".to_owned()));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.view, ViewMode::Diff);
    }

    /// Renders a transcript that cannot fit, then checks that the pane really
    /// moves on screen and that the scrollbar only appears when it should.
    #[test]
    fn overflowing_transcript_renders_a_scrollbar_and_moves_when_scrolled() {
        let mut app = App::default();
        for index in 0..12 {
            app.push_history(HistoryEntry {
                command: format!("run-number-{index}"),
                status: EntryStatus::Success,
                detail: format!("line one of {index}\nline two of {index}"),
            });
        }
        let mut terminal = Terminal::new(TestBackend::new(120, 32)).expect("test terminal");

        terminal.draw(|frame| draw(frame, &app)).expect("render");
        let following = format!("{:?}", terminal.backend().buffer());
        assert!(
            app.transcript_max_scroll() > 0,
            "the fixture should overflow the pane"
        );
        assert!(following.contains('█'), "a scrollbar thumb should be drawn");
        // Following pins the newest entry into view.
        assert!(following.contains("run-number-11"));

        app.scroll_transcript_to_top();
        terminal.draw(|frame| draw(frame, &app)).expect("render");
        let scrolled = format!("{:?}", terminal.backend().buffer());
        assert_ne!(
            following, scrolled,
            "scrolling must change what is displayed"
        );
        assert!(scrolled.contains("Session (scrolled)"));
        assert!(!scrolled.contains("run-number-11"));

        // A transcript that fits needs no scrollbar.
        let empty = App::default();
        terminal.draw(|frame| draw(frame, &empty)).expect("render");
        let short = format!("{:?}", terminal.backend().buffer());
        assert!(!short.contains('█'));
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
        // The CTA controls render as their own boxed buttons now, not
        // plain "[ Run vectorizer ]" text inside the form list.
        assert!(rendered.contains("Run vectorizer"));
        assert!(rendered.contains("View diff"));
        // Policy is a segmented control showing every option at once.
        assert!(rendered.contains("balanced"));
        assert!(rendered.contains("conservative"));
        assert!(rendered.contains("aggressive"));
    }

    #[test]
    fn renders_diff_view_when_active() {
        let mut app = App {
            diff_labels: Some(("in.ll".to_owned(), "out.ll".to_owned())),
            diff_rows: build_diff_rows("a\nb\n", "a\nc\n"),
            view: ViewMode::Diff,
            ..App::default()
        };
        app.tick_count = 0;
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &app))
            .expect("render TUI");
        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("original"));
        assert!(rendered.contains("optimized"));
    }
}
