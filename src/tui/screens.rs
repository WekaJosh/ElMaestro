//! ratatui Screens. Mirrors python-legacy/src/.../tui/screens.py.
//!
//! Five screens:
//!   - Home          main menu (Run / Browse / Compare / Quit)
//!   - PickConfig    file picker, filtered to .yaml / .yml
//!   - Run           spec table + live progress
//!   - BrowseResults pick a recent run, open report.html in browser
//!   - Compare       multi-select recent runs, render compare.html

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Padding, Paragraph, Row, Table,
    TableState, Wrap,
};
use ratatui::Frame;

use super::runner::{plan_pairs, RunEvent};

/// Top-level screen identity. Screens are stack-pushed via App::push.
pub enum Screen {
    Home(HomeScreen),
    PickConfigForRun(PickConfigScreen),
    Run(RunScreen),
    Browse(BrowseScreen),
    Compare(CompareScreen),
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

const HOME_ITEMS: &[&str] = &[
    "Run a benchmark",
    "Browse past results",
    "Compare runs",
    "Quit",
];

pub struct HomeScreen {
    state: ListState,
}

impl HomeScreen {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self { state }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff)))
            .padding(Padding::new(4, 4, 2, 2))
            .title(" ElMaestro ")
            .title_style(Style::default().add_modifier(Modifier::BOLD));
        let card = centered(area, 70, 16);
        frame.render_widget(Clear, card);
        frame.render_widget(outer.clone(), card);

        let inner = inner_rect(card, 4, 2);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(inner);
        let sub = Paragraph::new("IO benchmarking harness on elbencho + fio")
            .style(Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)))
            .alignment(ratatui::layout::Alignment::Center)
            .wrap(Wrap { trim: true });
        frame.render_widget(sub, chunks[0]);

        let items: Vec<ListItem> = HOME_ITEMS
            .iter()
            .map(|s| ListItem::new(Line::from(Span::raw(format!("  {}  ", s)))))
            .collect();
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(0x21, 0x26, 0x2d))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, chunks[1], &mut self.state);
    }

    pub fn select_next(&mut self) {
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + 1) % HOME_ITEMS.len()));
    }

    pub fn select_prev(&mut self) {
        let i = self.state.selected().unwrap_or(0);
        self.state
            .select(Some((i + HOME_ITEMS.len() - 1) % HOME_ITEMS.len()));
    }

    pub fn selected_action(&self) -> HomeAction {
        match self.state.selected().unwrap_or(0) {
            0 => HomeAction::Run,
            1 => HomeAction::Browse,
            2 => HomeAction::Compare,
            _ => HomeAction::Quit,
        }
    }
}

pub enum HomeAction {
    Run,
    Browse,
    Compare,
    Quit,
}

// ---------------------------------------------------------------------------
// File picker
// ---------------------------------------------------------------------------

pub struct PickConfigScreen {
    pub cwd: PathBuf,
    entries: Vec<PathBuf>,
    state: ListState,
    pub error: Option<String>,
}

impl PickConfigScreen {
    pub fn new(start_dir: PathBuf) -> Self {
        let mut s = Self {
            cwd: start_dir,
            entries: Vec::new(),
            state: ListState::default(),
            error: None,
        };
        s.refresh();
        s
    }

    fn refresh(&mut self) {
        self.entries.clear();
        // Parent first if not at root.
        if let Some(parent) = self.cwd.parent() {
            if parent != self.cwd {
                self.entries.push(parent.to_path_buf());
            }
        }
        if let Ok(read) = std::fs::read_dir(&self.cwd) {
            let mut dirs: Vec<PathBuf> = Vec::new();
            let mut files: Vec<PathBuf> = Vec::new();
            for entry in read.flatten() {
                let p = entry.path();
                let name = entry.file_name();
                let n_str = name.to_string_lossy();
                if n_str.starts_with('.') {
                    continue;
                }
                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() {
                        dirs.push(p);
                    } else if matches!(p.extension().and_then(|e| e.to_str()), Some("yaml") | Some("yml")) {
                        files.push(p);
                    }
                }
            }
            dirs.sort();
            files.sort();
            self.entries.extend(dirs);
            self.entries.extend(files);
        }
        self.state.select(Some(0));
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        let header = Paragraph::new(Span::styled(
            "Pick a config (Enter to select, Esc to go back)",
            Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff)),
        ));
        frame.render_widget(header, chunks[0]);

        let path_text = Paragraph::new(Span::styled(
            format!("  {}", self.cwd.display()),
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
        ));
        frame.render_widget(path_text, chunks[1]);

        let items: Vec<ListItem> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let is_parent = i == 0 && p == self.cwd.parent().unwrap_or(&self.cwd);
                let is_dir = p.is_dir();
                let label = if is_parent {
                    "..".into()
                } else {
                    let name = p
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if is_dir {
                        format!("{}/", name)
                    } else {
                        name
                    }
                };
                let style = if is_dir {
                    Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff))
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(Span::styled(label, style)))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(0x21, 0x26, 0x2d))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, chunks[2], &mut self.state);

        let footer_text = self
            .error
            .clone()
            .unwrap_or_else(|| "↑/↓ move · Enter select · Esc back".into());
        let footer_style = if self.error.is_some() {
            Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49))
        } else {
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
        };
        frame.render_widget(
            Paragraph::new(Span::styled(footer_text, footer_style)),
            chunks[3],
        );
    }

    pub fn select_next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + 1) % self.entries.len()));
    }

    pub fn select_prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state
            .select(Some((i + self.entries.len() - 1) % self.entries.len()));
    }

    pub fn activate_selected(&mut self) -> Option<PathBuf> {
        let idx = self.state.selected()?;
        let p = self.entries.get(idx)?.clone();
        if p.is_dir() {
            self.cwd = p;
            self.error = None;
            self.refresh();
            None
        } else {
            Some(p)
        }
    }
}

// ---------------------------------------------------------------------------
// Run screen
// ---------------------------------------------------------------------------

pub struct RunScreen {
    pub config: PathBuf,
    pub status_text: String,
    pub rows: Vec<RowState>,
    pub table_state: TableState,
    pub running: bool,
    pub rx: Option<Receiver<RunEvent>>,
    pub finished: bool,
}

pub struct RowState {
    pub idx: usize,
    pub target: String,
    pub workload: String,
    pub axis_label: String,
    pub status: String,
    pub duration: String,
}

impl RunScreen {
    pub fn new(config: PathBuf) -> Self {
        let mut screen = Self {
            config,
            status_text: String::new(),
            rows: Vec::new(),
            table_state: TableState::default(),
            running: false,
            rx: None,
            finished: false,
        };
        match plan_pairs(&screen.config) {
            Ok(pairs) => {
                for (idx, (point, spec)) in pairs.iter().enumerate() {
                    let axis_label = point.as_ref().map(|p| p.short_label()).unwrap_or_default();
                    screen.rows.push(RowState {
                        idx: idx + 1,
                        target: spec.target_name().into(),
                        workload: spec.workload.name.clone(),
                        axis_label,
                        status: "queued".into(),
                        duration: String::new(),
                    });
                }
                screen.status_text = format!(
                    "Loaded {} spec(s) from {}. Press [r] to run, [esc] back.",
                    screen.rows.len(),
                    screen.config.display(),
                );
            }
            Err(e) => {
                screen.status_text = format!("Failed to load: {:#}", e);
            }
        }
        screen
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(1)])
            .split(area);
        let status_style = if self.running {
            Style::default().fg(Color::Rgb(0xd2, 0x99, 0x22))
        } else if self.finished {
            Style::default().fg(Color::Rgb(0x3f, 0xb9, 0x50))
        } else {
            Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff))
        };
        frame.render_widget(
            Paragraph::new(Span::styled(self.status_text.clone(), status_style))
                .wrap(Wrap { trim: true }),
            chunks[0],
        );

        let header = Row::new(vec!["#", "target", "workload", "axes", "status", "duration"])
            .style(
                Style::default()
                    .fg(Color::Rgb(0x8b, 0x94, 0x9e))
                    .add_modifier(Modifier::BOLD),
            );
        let rows: Vec<Row> = self
            .rows
            .iter()
            .map(|r| {
                let status_style = match r.status.as_str() {
                    "completed" => Style::default().fg(Color::Rgb(0x3f, 0xb9, 0x50)),
                    "running" => Style::default().fg(Color::Rgb(0xd2, 0x99, 0x22)),
                    "error" => Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
                    s if s.starts_with("failed") => {
                        Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49))
                    }
                    _ => Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
                };
                Row::new(vec![
                    Cell::from(format!("{:04}", r.idx)),
                    Cell::from(r.target.clone()),
                    Cell::from(r.workload.clone()),
                    Cell::from(r.axis_label.clone()),
                    Cell::from(r.status.clone()).style(status_style),
                    Cell::from(r.duration.clone()),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(6),
            Constraint::Length(18),
            Constraint::Length(18),
            Constraint::Length(18),
            Constraint::Length(14),
            Constraint::Length(10),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(
                Style::default()
                    .bg(Color::Rgb(0x21, 0x26, 0x2d))
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL).title(" Specs "));
        frame.render_stateful_widget(table, chunks[1], &mut self.table_state);

        let footer_text = if self.running {
            "Running... [esc] cannot interrupt mid-run"
        } else if self.finished {
            "Done. [esc] back to home"
        } else {
            "[r] run · [esc] back · [↑/↓] navigate"
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                footer_text,
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
            chunks[2],
        );
    }

    pub fn start_run(&mut self) {
        if self.running {
            return;
        }
        self.running = true;
        self.status_text = "Running…".into();
        let (tx, rx): (Sender<RunEvent>, Receiver<RunEvent>) = std::sync::mpsc::channel();
        let cfg = self.config.clone();
        std::thread::spawn(move || super::runner::execute(&cfg, tx));
        self.rx = Some(rx);
    }

    /// Drain any pending events from the worker channel. Called from the
    /// main TUI tick.
    pub fn drain_events(&mut self) {
        if self.rx.is_none() {
            return;
        }
        loop {
            let next = match self.rx.as_ref() {
                Some(rx) => rx.try_recv(),
                None => return,
            };
            match next {
                Ok(event) => self.handle_event(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx = None;
                    return;
                }
            }
        }
    }

    fn handle_event(&mut self, event: RunEvent) {
        match event {
            RunEvent::RunStarted { run_dir, total } => {
                self.status_text =
                    format!("Running {} spec(s) → {}", total, run_dir.display());
            }
            RunEvent::SpecPlanned { .. } => {
                // Already in the table from new(); skip.
            }
            RunEvent::SpecStarted { index } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.idx == index) {
                    row.status = "running".into();
                }
            }
            RunEvent::SpecFinished {
                index,
                status,
                duration_s,
                ..
            } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.idx == index) {
                    row.status = status.label();
                    row.duration = format!("{:.1}s", duration_s);
                }
            }
            RunEvent::RunFinished {
                run_dir,
                completed,
                failed,
            } => {
                self.running = false;
                self.finished = true;
                self.status_text = format!(
                    "Done. completed={} failed={} → {}",
                    completed,
                    failed,
                    run_dir.display()
                );
            }
            RunEvent::Crashed { message } => {
                self.running = false;
                self.finished = true;
                self.status_text = format!("Worker crashed: {}", message);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Browse results
// ---------------------------------------------------------------------------

pub struct BrowseScreen {
    pub results_root: PathBuf,
    runs: Vec<RunRow>,
    state: TableState,
    pub error: Option<String>,
}

struct RunRow {
    name: String,
    path: PathBuf,
    specs: usize,
    engine: String,
}

impl BrowseScreen {
    pub fn new(results_root: PathBuf) -> Self {
        let mut s = Self {
            results_root,
            runs: Vec::new(),
            state: TableState::default(),
            error: None,
        };
        s.refresh();
        s
    }

    fn refresh(&mut self) {
        self.runs.clear();
        if !self.results_root.is_dir() {
            self.error = Some(format!(
                "no results directory at {}",
                self.results_root.display()
            ));
            return;
        }
        let mut dirs: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        if let Ok(read) = std::fs::read_dir(&self.results_root) {
            for entry in read.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let p = entry.path();
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    dirs.push((p, mtime));
                }
            }
        }
        dirs.sort_by(|a, b| b.1.cmp(&a.1));
        for (p, _) in dirs.iter().take(50) {
            let specs = std::fs::read_dir(p)
                .map(|r| {
                    r.flatten()
                        .filter(|e| {
                            e.path().is_dir()
                                && e.path().join("result.json").is_file()
                        })
                        .count()
                })
                .unwrap_or(0);
            let engine = sniff_engine(p);
            self.runs.push(RunRow {
                name: p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                path: p.clone(),
                specs,
                engine,
            });
        }
        self.state.select(if self.runs.is_empty() { None } else { Some(0) });
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Browse past runs",
                Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff)),
            )),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                format!("  {}", self.results_root.display()),
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
            chunks[1],
        );

        let header = Row::new(vec!["run dir", "specs", "engine"]).style(
            Style::default()
                .fg(Color::Rgb(0x8b, 0x94, 0x9e))
                .add_modifier(Modifier::BOLD),
        );
        let rows: Vec<Row> = self
            .runs
            .iter()
            .map(|r| {
                Row::new(vec![
                    Cell::from(r.name.clone()),
                    Cell::from(r.specs.to_string()),
                    Cell::from(r.engine.clone()),
                ])
            })
            .collect();
        let table = Table::new(
            rows,
            [Constraint::Min(36), Constraint::Length(8), Constraint::Length(12)],
        )
        .header(header)
        .row_highlight_style(
            Style::default()
                .bg(Color::Rgb(0x21, 0x26, 0x2d))
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL).title(" Runs "));
        frame.render_stateful_widget(table, chunks[2], &mut self.state);

        let footer_style = if self.error.is_some() {
            Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49))
        } else {
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
        };
        let footer = self
            .error
            .clone()
            .unwrap_or_else(|| "↑/↓ move · Enter open report · Esc back".into());
        frame.render_widget(
            Paragraph::new(Span::styled(footer, footer_style)),
            chunks[3],
        );
    }

    pub fn select_next(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + 1) % self.runs.len()));
    }

    pub fn select_prev(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + self.runs.len() - 1) % self.runs.len()));
    }

    pub fn open_selected(&mut self) {
        let Some(idx) = self.state.selected() else {
            return;
        };
        let Some(run) = self.runs.get(idx) else {
            return;
        };
        let report = run.path.join("report.html");
        let final_path = if report.is_file() {
            report
        } else {
            // Fall back to first spec's report.
            let mut alt = None;
            if let Ok(read) = std::fs::read_dir(&run.path) {
                for e in read.flatten() {
                    if e.path().is_dir() {
                        let candidate = e.path().join("report.html");
                        if candidate.is_file() {
                            alt = Some(candidate);
                            break;
                        }
                    }
                }
            }
            match alt {
                Some(p) => p,
                None => {
                    self.error = Some("no report.html found".into());
                    return;
                }
            }
        };
        open_in_browser(&final_path);
        self.error = Some(format!("opened {}", final_path.display()));
    }
}

fn sniff_engine(run_dir: &Path) -> String {
    if let Ok(read) = std::fs::read_dir(run_dir) {
        for e in read.flatten() {
            let p = e.path().join("result.json");
            if p.is_file() {
                if let Ok(text) = std::fs::read_to_string(&p) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(s) = v.get("engine").and_then(|x| x.as_str()) {
                            return s.into();
                        }
                    }
                }
                return "?".into();
            }
        }
    }
    "?".into()
}

fn open_in_browser(path: &Path) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(path).spawn();
}

// ---------------------------------------------------------------------------
// Compare
// ---------------------------------------------------------------------------

pub struct CompareScreen {
    pub results_root: PathBuf,
    runs: Vec<ComparePick>,
    state: ListState,
    pub message: Option<String>,
}

struct ComparePick {
    name: String,
    path: PathBuf,
    selected: bool,
}

impl CompareScreen {
    pub fn new(results_root: PathBuf) -> Self {
        let mut s = Self {
            results_root,
            runs: Vec::new(),
            state: ListState::default(),
            message: None,
        };
        s.refresh();
        s
    }

    fn refresh(&mut self) {
        self.runs.clear();
        if !self.results_root.is_dir() {
            self.message = Some(format!(
                "no results directory at {}",
                self.results_root.display()
            ));
            return;
        }
        let mut dirs: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        if let Ok(read) = std::fs::read_dir(&self.results_root) {
            for entry in read.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let p = entry.path();
                    let mtime = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(std::time::UNIX_EPOCH);
                    dirs.push((p, mtime));
                }
            }
        }
        dirs.sort_by(|a, b| b.1.cmp(&a.1));
        for (p, _) in dirs.iter().take(30) {
            self.runs.push(ComparePick {
                name: p.file_name().unwrap_or_default().to_string_lossy().into_owned(),
                path: p.clone(),
                selected: false,
            });
        }
        self.state.select(if self.runs.is_empty() { None } else { Some(0) });
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Compare runs",
                Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff)),
            )),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                "Space toggles, [c] renders, [Esc] back. Pick 2+.",
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
            chunks[1],
        );

        let items: Vec<ListItem> = self
            .runs
            .iter()
            .map(|r| {
                let mark = if r.selected { "[x]" } else { "[ ]" };
                ListItem::new(Line::from(format!("  {}  {}", mark, r.name)))
            })
            .collect();
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(0x21, 0x26, 0x2d))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");
        frame.render_stateful_widget(list, chunks[2], &mut self.state);

        let footer_style = if self.message.is_some() {
            Style::default().fg(Color::Rgb(0x3f, 0xb9, 0x50))
        } else {
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
        };
        let footer = self
            .message
            .clone()
            .unwrap_or_else(|| "↑/↓ move · Space toggle · [c] compare · Esc back".into());
        frame.render_widget(
            Paragraph::new(Span::styled(footer, footer_style)),
            chunks[3],
        );
    }

    pub fn select_next(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + 1) % self.runs.len()));
    }

    pub fn select_prev(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + self.runs.len() - 1) % self.runs.len()));
    }

    pub fn toggle_selected(&mut self) {
        if let Some(idx) = self.state.selected() {
            if let Some(r) = self.runs.get_mut(idx) {
                r.selected = !r.selected;
            }
        }
    }

    pub fn render_compare(&mut self) {
        let picked: Vec<PathBuf> = self
            .runs
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.path.clone())
            .collect();
        if picked.len() < 2 {
            self.message = Some("Pick at least 2 runs.".into());
            return;
        }
        let loaded: Result<Vec<_>, _> = picked
            .iter()
            .map(|p| crate::report::load_run(p, None))
            .collect();
        match loaded {
            Ok(runs) => {
                let out = self.results_root.join(format!(
                    "compare-{}-vs-{}.html",
                    runs[0].label,
                    runs[runs.len() - 1].label,
                ));
                match crate::report::render_compare(&runs, &out, None, "elmaestro compare") {
                    Ok(_) => {
                        open_in_browser(&out);
                        self.message = Some(format!("Wrote {}", out.display()));
                    }
                    Err(e) => {
                        self.message = Some(format!("Compare failed: {:#}", e));
                    }
                }
            }
            Err(e) => {
                self.message = Some(format!("Failed to load runs: {:#}", e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let x = area.x + (area.width - w) / 2;
    let y = area.y + (area.height - h) / 2;
    Rect::new(x, y, w, h)
}

fn inner_rect(area: Rect, pad_x: u16, pad_y: u16) -> Rect {
    Rect::new(
        area.x + pad_x,
        area.y + pad_y,
        area.width.saturating_sub(pad_x * 2),
        area.height.saturating_sub(pad_y * 2),
    )
}
