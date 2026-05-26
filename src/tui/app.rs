//! Main ratatui App + event loop.
//!
//! Screen-stack pattern: push/pop screens as the user navigates.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::configure::ConfigureScreen;
use super::screens::{
    BrowseScreen, CompareScreen, HomeAction, HomeScreen, PickConfigScreen, RunScreen, Screen,
};

/// Set to `true` while the terminal is in raw mode + alt-screen + mouse
/// capture. Used by both the Drop guard and the panic hook so cleanup is
/// idempotent and doesn't fire on terminals we never touched.
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALL_HOOK_ONCE: Once = Once::new();

/// Owns the terminal state during the TUI. Restores raw mode, the alt
/// screen, and mouse capture on Drop. A panic hook is also installed once
/// per process: `panic = "abort"` in the release profile skips Drop on
/// panic, and without the hook the shell ends up stuck in mouse-tracking
/// mode after a crash (every keypress / mouse motion splatters SGR
/// escape sequences like "35;146;27M..." into the prompt).
struct TerminalGuard;

impl TerminalGuard {
    fn install() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let _ = stdout.flush();
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);

        INSTALL_HOOK_ONCE.call_once(|| {
            let original = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                Self::cleanup();
                original(info);
            }));
        });

        Ok(TerminalGuard)
    }

    /// Idempotent: only emits restore sequences if we actually entered raw
    /// mode. Safe to call from both Drop and the panic hook without double
    /// cleanup.
    ///
    /// We write raw ANSI bytes directly (rather than using crossterm's
    /// DisableMouseCapture + LeaveAlternateScreen commands) for two reasons:
    ///   1. crossterm 0.28's DisableMouseCapture sends 1006l/1015l/1002l/1000l
    ///      but NOT 1003l (any-event tracking). Real-world bug: on RHEL with
    ///      certain terminal emulators, SGR mouse events like "0;119;41m"
    ///      kept leaking into the shell after exit despite crossterm's
    ///      disable sequence. The 1003l covers that.
    ///   2. Writing to /dev/tty (the controlling terminal) bypasses any
    ///      stdout buffering or redirection races at process exit and is
    ///      strictly more reliable than relying on stdout to flush in time.
    fn cleanup() {
        if !TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
            return;
        }
        let _ = disable_raw_mode();

        // Order matters. Leave the alt screen FIRST so the disable codes
        // run in the same context as if the user typed them at the shell:
        // a bare `printf '\033[?1006l...'` from the prompt reliably disables
        // mouse tracking on Terminal.app, but sending the same bytes while
        // still inside the alt screen does not — Terminal.app appears to
        // save and restore mouse state across the alt-screen boundary, so
        // disables emitted inside the alt screen get undone the moment we
        // switch back to the main buffer.
        //
        // ?1049l = leave xterm alternate screen buffer (do this first)
        // ?1003l = any-event (motion) tracking off
        // ?1002l = button-event tracking off
        // ?1000l = X10 mouse mode off
        // ?1015l = urxvt mouse mode off
        // ?1006l = SGR mouse mode off (this is what produced the
        //          "0;...m" sequences leaking on weka54)
        // ?25h   = show cursor
        // [m     = reset SGR attributes (colors / bold / etc.)
        const RESTORE: &[u8] =
            b"\x1b[?1049l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?1015l\x1b[?1006l\x1b[?25h\x1b[m";

        // Prefer /dev/tty: hits the controlling terminal even if stdout is
        // redirected, and dodges stdout flush races at process teardown.
        if let Ok(mut tty) = OpenOptions::new().write(true).open("/dev/tty") {
            let _ = tty.write_all(RESTORE);
            let _ = tty.flush();
        }
        // Belt-and-suspenders fallback in case /dev/tty isn't openable
        // (containers without a controlling terminal, etc.).
        let mut stdout = io::stdout();
        let _ = stdout.write_all(RESTORE);
        let _ = stdout.flush();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::cleanup();
    }
}

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
        // Mouse capture is enabled (works in any terminal that supports it);
        // keyboard navigation works regardless, so this is no-loss in
        // tmux/screen sessions that don't pass mouse events.
        let _guard = TerminalGuard::install()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        let result = self.event_loop(&mut terminal);

        // _guard's Drop runs here and restores the terminal. We deliberately
        // do not swallow that cleanup with `.ok()` anywhere; if Drop fails it
        // crashes loudly rather than leaking mouse-tracking into the shell.
        drop(_guard);
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
                        Screen::Report(s) => s.render(frame, area),
                        Screen::Compared(s) => s.render(frame, area),
                    }
                }
            })?;

            // Poll for input with a short timeout so the run-screen tick stays
            // responsive when events come in from the worker channel.
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        // Ctrl+S / Ctrl+L are template hotkeys for the
                        // Configure screen. Intercept before normal dispatch
                        // so 's' / 'l' still work as text input on text fields.
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            if let Some(Screen::Configure(form)) =
                                self.stack.last_mut()
                            {
                                match key.code {
                                    KeyCode::Char('s') | KeyCode::Char('S') => {
                                        form.save_template();
                                        continue;
                                    }
                                    KeyCode::Char('l') | KeyCode::Char('L') => {
                                        form.load_template();
                                        continue;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        if self.handle_key(key.code) {
                            return Ok(());
                        }
                    }
                    Event::Mouse(m) => {
                        use crossterm::event::{MouseButton, MouseEventKind};
                        // Fire on Up (release), not Down (press). Two reasons:
                        //   1. That's how every conventional GUI button
                        //      behaves — press-and-drag-away cancels.
                        //   2. Avoids the v1.3.0-1.3.3 mouse-leak symptom:
                        //      if we exit on Down, the matching Up event
                        //      arrives at the shell some tens of ms later
                        //      (SSH round-trip vs click duration), and the
                        //      terminal hasn't applied our disable codes
                        //      yet — so it splatters "0;col;row;m" into
                        //      the prompt. Triggering on Up means the Up
                        //      IS our exit signal; no later event in flight.
                        if let MouseEventKind::Up(MouseButton::Left) = m.kind {
                            if self.handle_click(m.column, m.row) {
                                return Ok(());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// Mouse-click dispatch. Returns true if the app should exit.
    fn handle_click(&mut self, col: u16, row: u16) -> bool {
        match self.stack.last_mut() {
            Some(Screen::Home(home)) => {
                if let Some(action) = home.click_at(col, row) {
                    match action {
                        super::screens::HomeAction::Configure => {
                            self.stack.push(Screen::Configure(ConfigureScreen::new()));
                        }
                        super::screens::HomeAction::PickYaml => {
                            let start = std::env::current_dir()
                                .unwrap_or_else(|_| PathBuf::from("."));
                            self.stack
                                .push(Screen::PickConfigForRun(PickConfigScreen::new(start)));
                        }
                        super::screens::HomeAction::Browse => {
                            let root = default_results_root();
                            self.stack.push(Screen::Browse(BrowseScreen::new(root)));
                        }
                        super::screens::HomeAction::Compare => {
                            let root = default_results_root();
                            self.stack.push(Screen::Compare(CompareScreen::new(root)));
                        }
                        super::screens::HomeAction::Quit => return true,
                    }
                }
            }
            Some(Screen::Configure(form)) => {
                form.click_at(col, row);
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
            }
            Some(Screen::PickConfigForRun(picker)) => {
                if let Some(path) = picker.click_at(col, row) {
                    self.stack.pop();
                    self.stack.push(Screen::Run(RunScreen::new(path)));
                }
            }
            Some(Screen::Browse(b)) => {
                b.click_at(col, row);
                if let Some(path) = b.consume_pending_open() {
                    self.apply_nav(NavAction::PushReport(path));
                }
            }
            Some(Screen::Compare(c)) => {
                c.click_at(col, row);
            }
            Some(Screen::Run(_))
            | Some(Screen::Report(_))
            | Some(Screen::Compared(_))
            | None => {}
        }
        false
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
            Some(Screen::Run(run)) => {
                let nav = match code {
                    KeyCode::Char('r') if !run.running => {
                        run.start_run();
                        None
                    }
                    KeyCode::Esc | KeyCode::Char('q') if !run.running => Some(NavAction::Pop),
                    KeyCode::Up | KeyCode::Char('k') => {
                        run.select_prev();
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        run.select_next();
                        None
                    }
                    KeyCode::Enter => {
                        // Push an in-TUI ReportScreen for the highlighted
                        // row. No browser, no external dependency — works
                        // over SSH where xdg-open / `open` would fail.
                        run.selected_result_path().map(NavAction::PushReport)
                    }
                    _ => None,
                };
                if let Some(action) = nav {
                    self.apply_nav(action);
                }
            }
            Some(Screen::Browse(b)) => {
                let nav = match code {
                    KeyCode::Esc | KeyCode::Char('q') => Some(NavAction::Pop),
                    KeyCode::Up | KeyCode::Char('k') => {
                        b.select_prev();
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        b.select_next();
                        None
                    }
                    KeyCode::Enter => b.first_spec_result_path().map(NavAction::PushReport),
                    _ => None,
                };
                if let Some(action) = nav {
                    self.apply_nav(action);
                }
            }
            Some(Screen::Compare(c)) => {
                let nav = match code {
                    KeyCode::Esc | KeyCode::Char('q') => Some(NavAction::Pop),
                    KeyCode::Up | KeyCode::Char('k') => {
                        c.select_prev();
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        c.select_next();
                        None
                    }
                    KeyCode::Char(' ') => {
                        c.toggle_selected();
                        None
                    }
                    KeyCode::Char('c') | KeyCode::Enter => {
                        c.build_compared().map(NavAction::PushCompared)
                    }
                    _ => None,
                };
                if let Some(action) = nav {
                    self.apply_nav(action);
                }
            }
            Some(Screen::Report(r)) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => r.select_prev(),
                KeyCode::Down | KeyCode::Char('j') => r.select_next(),
                KeyCode::Char('b') | KeyCode::Char('B') => r.open_html(),
                _ => {}
            },
            Some(Screen::Compared(c)) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.stack.pop();
                    if self.stack.is_empty() {
                        return true;
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => c.select_prev(),
                KeyCode::Down | KeyCode::Char('j') => c.select_next(),
                KeyCode::Char('e') | KeyCode::Char('E') => c.export_html(),
                KeyCode::Char('b') | KeyCode::Char('B') => c.open_html(),
                _ => {}
            },
            None => return true,
        }
        false
    }

    fn apply_nav(&mut self, action: NavAction) {
        match action {
            NavAction::Pop => {
                self.stack.pop();
            }
            NavAction::PushReport(p) => {
                use super::screens::ReportScreen;
                self.stack.push(Screen::Report(ReportScreen::new(p)));
            }
            NavAction::PushCompared(c) => {
                self.stack.push(Screen::Compared(c));
            }
        }
    }
}

enum NavAction {
    Pop,
    PushReport(PathBuf),
    PushCompared(super::screens::ComparedScreen),
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
