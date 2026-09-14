use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

const POLICIES: [&str; 3] = ["balanced", "conservative", "aggressive"];
const VECTOR_WIDTHS: [&str; 5] = ["auto", "2", "4", "8", "16"];
const FIELD_COUNT: usize = 8;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunState {
    Ready,
    Success,
    Failed,
}

#[derive(Debug)]
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
    state: RunState,
    log: String,
    log_scroll: u16,
    quit: bool,
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
            state: RunState::Ready,
            log: "Configure the pass, then select Run or press Ctrl-R.".to_owned(),
            log_scroll: 0,
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
            KeyCode::PageUp => self.log_scroll = self.log_scroll.saturating_sub(5),
            KeyCode::PageDown => self.log_scroll = self.log_scroll.saturating_add(5),
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

    fn execute(&mut self) {
        self.state = RunState::Ready;
        self.log_scroll = 0;
        if self.input.value.trim().is_empty() || self.output.value.trim().is_empty() {
            self.state = RunState::Failed;
            self.log.clear();
            self.log.push_str("Input and output paths are required.");
            return;
        }

        let executable = match cli_executable() {
            Ok(executable) => executable,
            Err(error) => {
                self.state = RunState::Failed;
                self.log = error;
                return;
            }
        };
        let output_path = PathBuf::from(&self.output.value);
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(error) = fs::create_dir_all(parent) {
                    self.state = RunState::Failed;
                    self.log = format!(
                        "Cannot create output directory {}: {error}",
                        parent.display()
                    );
                    return;
                }
            }
        }
        let arguments = self.command_arguments();
        self.log = format!(
            "Running {} {}",
            executable.display(),
            arguments
                .iter()
                .map(|argument| shell_quote(argument))
                .collect::<Vec<_>>()
                .join(" ")
        );

        match Command::new(&executable).args(&arguments).output() {
            Ok(result) => {
                let stderr = String::from_utf8_lossy(&result.stderr);
                let stdout = String::from_utf8_lossy(&result.stdout);
                let mut details = if result.status.success() {
                    self.state = RunState::Success;
                    format!("Success: wrote {}.", self.output.value)
                } else {
                    self.state = RunState::Failed;
                    format!(
                        "Failed: rv-vectorize exited with status {}.",
                        result
                            .status
                            .code()
                            .map_or_else(|| "signal".to_owned(), |code| code.to_string())
                    )
                };
                if !stderr.trim().is_empty() {
                    details.push_str("\n\n");
                    details.push_str(stderr.trim());
                }
                if !stdout.trim().is_empty() {
                    details.push_str("\n\nstdout:\n");
                    details.push_str(stdout.trim());
                }
                self.log = details;
            }
            Err(error) => {
                self.state = RunState::Failed;
                self.log = format!("Could not start {}: {error}", executable.display());
            }
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
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn field_line<'a>(label: &'a str, value: &'a str, selected: bool) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), Style::default().fg(Color::Gray)),
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

fn draw(frame: &mut Frame, app: &App) {
    let [header_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(3),
    ])
    .areas(frame.area());
    let [form_area, log_area] =
        Layout::horizontal([Constraint::Percentage(47), Constraint::Percentage(53)])
            .areas(content_area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(" LLVM ", Style::default().fg(Color::Black).bg(Color::Cyan)),
        Span::styled(
            " Rust Loop Vectorizer ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" interactive compiler pass"),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, header_area);

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
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(form, form_area);

    let (status, color) = match app.state {
        RunState::Ready => ("READY", Color::Yellow),
        RunState::Success => ("SUCCESS", Color::Green),
        RunState::Failed => ("FAILED", Color::Red),
    };
    let mut log_lines = vec![
        Line::from(Span::styled(
            status,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
    ];
    log_lines.extend(app.log.lines().map(Line::raw));
    let log = Paragraph::new(Text::from(log_lines))
        .block(
            Block::default()
                .title(" Results ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(color)),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.log_scroll, 0));
    frame.render_widget(log, log_area);

    let footer = Paragraph::new(Line::from(vec![
        " Tab/↑↓ ".black().on_cyan().bold(),
        Span::raw(" navigate  "),
        "←→".cyan().bold(),
        Span::raw(" change  "),
        "Enter/Space".cyan().bold(),
        Span::raw(" select  "),
        "Ctrl-R".green().bold(),
        Span::raw(" run  "),
        "Esc".red().bold(),
        Span::raw(" quit "),
    ]))
    .block(Block::default().borders(Borders::ALL));
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
        if let Event::Key(key) = event::read()? {
            app.handle_key(key);
        }
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
    fn renders_at_typical_terminal_size() {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &App::default()))
            .expect("render TUI");
        let buffer = terminal.backend().buffer();
        let rendered = format!("{buffer:?}");
        assert!(rendered.contains("Rust Loop Vectorizer"));
        assert!(rendered.contains("Configuration"));
        assert!(rendered.contains("Results"));
    }
}
