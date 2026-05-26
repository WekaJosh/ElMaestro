//! Main ratatui App + event loop.
//!
//! Screen-stack pattern matching python-legacy/src/.../tui/app.py.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::configure::ConfigureScreen;
use super::screens::{
    BrowseScreen, CompareScreen, HomeAction, HomeScreen, PickConfigScreen, RunScreen, Screen,
};

pub struct App {
    stack: Vec<Screen>,
}

impl App {
    pub fn new(initial: Option<PathBuf>) -> Self {
        let initial_screen = match initial {
            Some(config) => Screen::Run(RunScreen::new(config)),
            None => Screen::Home(HomeScreen::new()),
        };
        App {
            stack: vec![initial_screen],
        }
    }

    pub fn run(&mut self) -> Result<()> {
        // Init terminal. Mouse capture is enabled (works in any terminal
        // that supports it); keyboard navigation works regardless, so this
        // is no-loss in tmux/screen sessions that don't pass mouse events.
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.event_loop(&mut terminal);

        // Restore terminal regardless of how we exit.
        disable_raw_mode().ok();
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture).ok();
        terminal.show_cursor().ok();
        result
    }

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        loop {
            // Drain run events (if a RunScreen is at the top).
            if let Some(Screen::Run(run)) = self.stack.last_mut() {
                run.drain_events();
            }

            terminal.draw(|frame| {
                let area = frame.area();
                if let Some(top) = self.stack.last_mut() {
                    match top {
                        Screen::Home(s) => s.render(frame, area),
                        Screen::Configure(s) => s.render(frame, area),
                        Screen::PickConfigForRun(s) => s.render(frame, area),
                        Screen::Run(s) => s.render(frame, area),
                        Screen::Browse(s) => s.render(frame, area),
                        Screen::Compare(s) => s.render(frame, area),
                    }
                }
            })?;

            // Poll for input with a short timeout so the run-screen tick stays
            // responsive when events come in from the worker channel.
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    let exit = self.handle_key(key.code);
                    if exit {
                        return Ok(());
                    }
                }
            }
        }
    }

    /// Returns true if the app should exit.
    fn handle_key(&mut self, code: KeyCode) -> bool {
        // Ctrl+C / 'q' at top-level Home exits the app. Other screens treat
        // those keys as "go back".
        match self.stack.last_mut() {
            Some(Screen::Home(home)) => match code {
                KeyCode::Char('q') | KeyCode::Esc => return true,
                KeyCode::Up | KeyCode::Char('k') => home.select_prev(),
                KeyCode::Down | KeyCode::Char('j') => home.select_next(),
                KeyCode::Enter => match home.selected_action() {
                    HomeAction::Configure => {
                        self.stack.push(Screen::Configure(ConfigureScreen::new()));
                    }
                    HomeAction::PickYaml => {
                        let start = std::env::current_dir()
                            .unwrap_or_else(|_| PathBuf::from("."));
                        self.stack
                            .push(Screen::PickConfigForRun(PickConfigScreen::new(start)));
                    }
                    HomeAction::Browse => {
                        let root = default_results_root();
                        self.stack.push(Screen::Browse(BrowseScreen::new(root)));
                    }
                    HomeAction::Compare => {
                        let root = default_results_root();
                        self.stack.push(Screen::Compare(CompareScreen::new(root)));
                    }
                    HomeAction::Quit => return true,
                },
                _ => {}
            },
            Some(Screen::Configure(form)) => {
                // Check for the user pressing Run / Cancel before reading
                // for ordinary nav keys.
                if form.cancelled {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                    return false;
                }
                if let Some((plan, label, repeats)) = form.built_plan.take() {
                    self.stack.pop();
                    self.stack
                        .push(Screen::Run(RunScreen::from_plan(plan, label, repeats)));
                    return false;
                }
                match code {
                    KeyCode::Esc => {
                        self.stack.pop();
                        if self.stack.is_empty() {
                            return true;
                        }
                    }
                    KeyCode::Tab | KeyCode::Down => form.focus_next(),
                    KeyCode::BackTab | KeyCode::Up => form.focus_prev(),
                    KeyCode::Right => form.nudge_right(),
                    KeyCode::Left => form.nudge_left(),
                    KeyCode::Home => form.home(),
                    KeyCode::End => form.end(),
                    KeyCode::Backspace => form.backspace(),
                    KeyCode::Delete => form.delete(),
                    KeyCode::Enter => form.activate(),
                    KeyCode::Char(' ') => {
                        if form.is_text_focused() {
                            form.insert_char(' ');
                        } else {
                            form.activate();
                        }
                    }
                    KeyCode::Char(c) => form.insert_char(c),
                    _ => {}
                }
            }
            Some(Screen::PickConfigForRun(picker)) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => picker.select_prev(),
                KeyCode::Down | KeyCode::Char('j') => picker.select_next(),
                KeyCode::Enter => {
                    if let Some(path) = picker.activate_selected() {
                        // Replace picker with RunScreen.
                        self.stack.pop();
                        self.stack.push(Screen::Run(RunScreen::new(path)));
                    }
                }
                _ => {}
            },
            Some(Screen::Run(run)) => match code {
                KeyCode::Char('r') if !run.running => run.start_run(),
                KeyCode::Esc | KeyCode::Char('q') if !run.running => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                }
                _ => {}
            },
            Some(Screen::Browse(b)) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => b.select_prev(),
                KeyCode::Down | KeyCode::Char('j') => b.select_next(),
                KeyCode::Enter => b.open_selected(),
                _ => {}
            },
            Some(Screen::Compare(c)) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => c.select_prev(),
                KeyCode::Down | KeyCode::Char('j') => c.select_next(),
                KeyCode::Char(' ') => c.toggle_selected(),
                KeyCode::Char('c') | KeyCode::Enter => c.render_compare(),
                _ => {}
            },
            None => return true,
        }
        false
    }
}

fn default_results_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join("results")
}

/// Open the TUI at the home screen.
pub fn run_home() -> Result<()> {
    App::new(None).run()
}

/// Open the TUI straight to a config's run screen.
pub fn run_tui(config: Option<&std::path::Path>) -> Result<()> {
    let initial = config.map(PathBuf::from);
    App::new(initial).run()
}
