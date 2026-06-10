//! ratatui Screens.
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

use super::runner::{plan_pairs, RunEvent, SpecMetrics};

/// Top-level screen identity. Screens are stack-pushed via App::push.
pub enum Screen {
    Home(HomeScreen),
    Configure(super::configure::ConfigureScreen),
    PickConfigForRun(PickConfigScreen),
    /// Same picker widget as PickConfigForRun, but the Enter handler
    /// dispatches the chosen path into the Configure screen underneath
    /// (via load_template_from) instead of starting a Run.
    PickTemplateForLoad(PickConfigScreen),
    Run(RunScreen),
    Browse(BrowseScreen),
    Compare(CompareScreen),
    Report(ReportScreen),
    Compared(ComparedScreen),
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

const HOME_ITEMS: &[&str] = &[
    "Configure & run a benchmark",
    "Open an existing YAML config",
    "Browse past results",
    "Compare runs",
    "Quit",
];

pub struct HomeScreen {
    state: ListState,
    /// Rect of the menu list, captured at render time so the mouse-click
    /// handler can map row -> item index.
    list_rect: Rect,
}

impl HomeScreen {
    pub fn new() -> Self {
        let mut state = ListState::default();
        state.select(Some(0));
        Self {
            state,
            list_rect: Rect::default(),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let outer = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff)))
            .padding(Padding::new(4, 4, 2, 2))
            .title(format!(" ElMaestro v{} ", crate::VERSION))
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
        self.list_rect = chunks[1];
        frame.render_stateful_widget(list, chunks[1], &mut self.state);
    }

    /// Hit-test a click against the menu. Returns the action to invoke,
    /// or None if the click was outside the list.
    pub fn click_at(&mut self, col: u16, row: u16) -> Option<HomeAction> {
        if !rect_contains(self.list_rect, col, row) {
            return None;
        }
        let idx = (row - self.list_rect.y) as usize;
        if idx >= HOME_ITEMS.len() {
            return None;
        }
        self.state.select(Some(idx));
        Some(self.selected_action())
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
            0 => HomeAction::Configure,
            1 => HomeAction::PickYaml,
            2 => HomeAction::Browse,
            3 => HomeAction::Compare,
            _ => HomeAction::Quit,
        }
    }
}

pub enum HomeAction {
    Configure,
    PickYaml,
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
    list_rect: Rect,
}

impl PickConfigScreen {
    pub fn new(start_dir: PathBuf) -> Self {
        let mut s = Self {
            cwd: start_dir,
            entries: Vec::new(),
            state: ListState::default(),
            error: None,
            list_rect: Rect::default(),
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
        self.list_rect = chunks[2];
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

    /// Hit-test a click. Returns Some(path) if the click selected a file
    /// (caller treats it as "picked"). Returns None for dir-navigation or
    /// clicks outside the list (in either case state is updated as needed).
    pub fn click_at(&mut self, col: u16, row: u16) -> Option<PathBuf> {
        if !rect_contains(self.list_rect, col, row) {
            return None;
        }
        let idx = (row - self.list_rect.y) as usize;
        if idx >= self.entries.len() {
            return None;
        }
        self.state.select(Some(idx));
        self.activate_selected()
    }
}

// ---------------------------------------------------------------------------
// Run screen
// ---------------------------------------------------------------------------

pub enum RunSource {
    Config(PathBuf),
    Plan {
        plan: crate::config::RunPlan,
        label: String,
        repeats: usize,
    },
}

pub struct RunScreen {
    pub source: RunSource,
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
    /// Metrics shown live as each spec finishes. Populated from
    /// RunEvent::SpecFinished.
    pub metrics: Option<SpecMetrics>,
    /// Path to result.json on disk so the Report viewer can load it
    /// when the user presses Enter on this row.
    pub result_path: Option<PathBuf>,
    /// Full error message when status is "error". Shown in the footer
    /// of the Run screen when the user highlights an errored row so
    /// they can see what actually went wrong (ssh failure, service
    /// start timeout, missing binary, etc.).
    pub error_detail: Option<String>,
}

impl RunScreen {
    pub fn new(config: PathBuf) -> Self {
        let mut screen = Self {
            source: RunSource::Config(config.clone()),
            status_text: String::new(),
            rows: Vec::new(),
            table_state: TableState::default(),
            running: false,
            rx: None,
            finished: false,
        };
        match plan_pairs(&config) {
            Ok(pairs) => {
                screen.populate_rows(&pairs);
                screen.status_text = format!(
                    "Loaded {} spec(s) from {}. Press [r] to run, [esc] back.",
                    screen.rows.len(),
                    config.display(),
                );
            }
            Err(e) => {
                screen.status_text = format!("Failed to load: {:#}", e);
            }
        }
        screen
    }

    /// Build a RunScreen from an in-memory plan (Configure flow).
    pub fn from_plan(plan: crate::config::RunPlan, label: String, repeats: usize) -> Self {
        let mut screen = Self {
            source: RunSource::Plan {
                plan: plan.clone(),
                label,
                repeats,
            },
            status_text: String::new(),
            rows: Vec::new(),
            table_state: TableState::default(),
            running: false,
            rx: None,
            finished: false,
        };
        match super::runner::plan_pairs_from(&plan) {
            Ok(pairs) => {
                screen.populate_rows(&pairs);
                let suffix = if repeats > 1 {
                    format!(" × {} runs", repeats)
                } else {
                    String::new()
                };
                screen.status_text = format!(
                    "Configured {} spec(s){}. Press [r] to run, [esc] back.",
                    screen.rows.len(),
                    suffix
                );
            }
            Err(e) => {
                screen.status_text = format!("Plan invalid: {:#}", e);
            }
        }
        screen
    }

    fn populate_rows(
        &mut self,
        pairs: &[(Option<crate::config::sweep::SweepPoint>, crate::config::RunSpec)],
    ) {
        for (idx, (point, spec)) in pairs.iter().enumerate() {
            let axis_label = point.as_ref().map(|p| p.short_label()).unwrap_or_default();
            self.rows.push(RowState {
                idx: idx + 1,
                target: spec.target_name().into(),
                workload: spec.workload.name.clone(),
                axis_label,
                status: "queued".into(),
                duration: String::new(),
                metrics: None,
                result_path: None,
                error_detail: None,
            });
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Footer is 3 rows so a multi-line error message (typically a
        // wrapped 2-line message like "ssh to 10.10.10.5: connection
        // refused: ...") is fully visible.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0), Constraint::Length(3)])
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

        let header = Row::new(vec![
            "#",
            "target",
            "workload",
            "axes",
            "status",
            "dur",
            "throughput",
            "iops",
            "p95 lat",
        ])
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
                    "running…" | "layout…" | "starting…" => {
                        Style::default().fg(Color::Rgb(0xd2, 0x99, 0x22))
                    }
                    "error" => Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
                    s if s.starts_with("failed") => {
                        Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49))
                    }
                    _ => Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
                };
                let (tput, iops, p95) = match &r.metrics {
                    Some(m) => (
                        format_throughput(m.throughput_mib_s),
                        format_iops(m.iops),
                        format_latency(m.lat_p95_us),
                    ),
                    None => (String::new(), String::new(), String::new()),
                };
                Row::new(vec![
                    Cell::from(format!("{:04}", r.idx)),
                    Cell::from(r.target.clone()),
                    Cell::from(r.workload.clone()),
                    Cell::from(r.axis_label.clone()),
                    Cell::from(r.status.clone()).style(status_style),
                    Cell::from(r.duration.clone()),
                    Cell::from(tput),
                    Cell::from(iops),
                    Cell::from(p95),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(9), // dur: fits "120s left"
            Constraint::Length(13),
            Constraint::Length(9),
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

        // If the highlighted row has an error captured, show it in the
        // footer so the user can actually see what failed (ssh
        // unreachable, service-start timeout, etc.) instead of a bare
        // "error" label in the table.
        let selected_err = self
            .table_state
            .selected()
            .and_then(|i| self.rows.get(i))
            .and_then(|r| r.error_detail.as_deref());
        let (footer_text, footer_style) = if let Some(err) = selected_err {
            (
                format!("✗ {}", err),
                Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
            )
        } else if self.running {
            (
                "Running... [↑/↓] highlight · [enter] view report · [esc] cannot interrupt mid-run".into(),
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )
        } else if self.finished {
            (
                "Done. [↑/↓] select · [enter] view report · [esc] back to home".into(),
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )
        } else {
            (
                "[r] run · [esc] back · [↑/↓] navigate".into(),
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )
        };
        frame.render_widget(
            Paragraph::new(Span::styled(footer_text, footer_style))
                .wrap(Wrap { trim: true }),
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
        match &self.source {
            RunSource::Config(path) => {
                let cfg = path.clone();
                std::thread::spawn(move || super::runner::execute(&cfg, tx));
            }
            RunSource::Plan {
                plan,
                label,
                repeats,
            } => {
                let plan = plan.clone();
                let label = label.clone();
                let repeats = *repeats;
                std::thread::spawn(move || {
                    super::runner::execute_plan(plan, &label, repeats, tx)
                });
            }
        }
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
                    row.status = "starting…".into();
                }
            }
            RunEvent::SpecProgress {
                index,
                phase,
                elapsed_s,
                remaining_s,
                throughput_mib_s,
                iops,
            } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.idx == index) {
                    row.status = match phase.as_str() {
                        "layout" => "layout…".into(),
                        "measure" => "running…".into(),
                        _ => "starting…".into(),
                    };
                    // Time-bound measure shows countdown (more useful than
                    // elapsed); everything else shows elapsed.
                    row.duration = match remaining_s {
                        Some(r) => format!("{}s left", r),
                        None => format!("{}s", elapsed_s),
                    };
                    // Live numbers light up the metrics columns while the
                    // engine runs; SpecFinished replaces them with finals.
                    if throughput_mib_s.is_some() || iops.is_some() {
                        row.metrics = Some(SpecMetrics {
                            primary_phase: phase,
                            throughput_mib_s,
                            iops,
                            ..Default::default()
                        });
                    }
                }
            }
            RunEvent::SpecFinished {
                index,
                status,
                duration_s,
                metrics,
                result_path,
                ..
            } => {
                if let Some(row) = self.rows.iter_mut().find(|r| r.idx == index) {
                    let detail = status.error_detail().map(String::from);
                    row.status = status.label();
                    row.duration = format!("{:.1}s", duration_s);
                    row.metrics = metrics;
                    row.result_path = result_path;
                    row.error_detail = detail;
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

    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let next = match self.table_state.selected() {
            Some(i) if i + 1 < self.rows.len() => i + 1,
            Some(_) => self.rows.len() - 1,
            None => 0,
        };
        self.table_state.select(Some(next));
    }

    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let prev = match self.table_state.selected() {
            Some(0) | None => 0,
            Some(i) => i - 1,
        };
        self.table_state.select(Some(prev));
    }

    /// Path to result.json for the currently-highlighted row, if it
    /// finished successfully. Used by the Enter handler to push a
    /// ReportScreen.
    pub fn selected_result_path(&self) -> Option<PathBuf> {
        let idx = self.table_state.selected()?;
        self.rows.get(idx).and_then(|r| r.result_path.clone())
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
    table_rect: Rect,
    /// Set by click_at when the user clicks a row. The app reads + clears
    /// this to push a ReportScreen for the selected run's first spec
    /// (replaces the old open-in-browser behavior that didn't work over
    /// SSH).
    pending_open: Option<PathBuf>,
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
            table_rect: Rect::default(),
            pending_open: None,
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
        self.table_rect = chunks[2];
        frame.render_stateful_widget(table, chunks[2], &mut self.state);

        let footer_style = if self.error.is_some() {
            Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49))
        } else {
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
        };
        let footer = self
            .error
            .clone()
            .unwrap_or_else(|| "↑/↓ move · Enter view report (in-TUI) · Esc back".into());
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

    /// Hit-test a click. Selects the row and queues a report-open
    /// (consumed by the app event loop, which pushes a ReportScreen).
    pub fn click_at(&mut self, col: u16, row: u16) {
        if !rect_contains(self.table_rect, col, row) {
            return;
        }
        let header_offset = 2u16; // top border + header line
        if row < self.table_rect.y + header_offset {
            return;
        }
        let idx = (row - self.table_rect.y - header_offset) as usize;
        if idx >= self.runs.len() {
            return;
        }
        self.state.select(Some(idx));
        self.pending_open = self.first_spec_result_path();
    }

    /// App reads + clears this after dispatching the click. Avoids the
    /// BrowseScreen needing a reference to the app's screen stack.
    pub fn consume_pending_open(&mut self) -> Option<PathBuf> {
        self.pending_open.take()
    }

    /// Path to result.json for the highlighted run's first spec. Used to
    /// push an in-TUI ReportScreen on Enter / click.
    pub fn first_spec_result_path(&mut self) -> Option<PathBuf> {
        let idx = self.state.selected()?;
        let run = self.runs.get(idx)?;
        if let Ok(read) = std::fs::read_dir(&run.path) {
            let mut dirs: Vec<PathBuf> = read
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            for d in dirs {
                let rp = d.join("result.json");
                if rp.is_file() {
                    return Some(rp);
                }
            }
        }
        self.error = Some("no result.json in this run".into());
        None
    }
}

// Human-formatted metric helpers shared by the Run screen and the in-TUI
// report viewer. All take `Option<f64>` so a missing metric renders as a
// dim "—" rather than blowing up the table layout.

pub(super) fn format_throughput(v: Option<f64>) -> String {
    match v {
        Some(x) if x >= 1024.0 => format!("{:.2} GiB/s", x / 1024.0),
        Some(x) if x >= 1.0 => format!("{:.0} MiB/s", x),
        Some(x) => format!("{:.2} MiB/s", x),
        None => "—".into(),
    }
}

pub(super) fn format_iops(v: Option<f64>) -> String {
    match v {
        Some(x) if x >= 1_000_000.0 => format!("{:.2}M", x / 1_000_000.0),
        Some(x) if x >= 1_000.0 => format!("{:.1}k", x / 1_000.0),
        Some(x) => format!("{:.0}", x),
        None => "—".into(),
    }
}

pub(super) fn format_latency(v: Option<f64>) -> String {
    match v {
        Some(x) if x >= 1000.0 => format!("{:.1} ms", x / 1000.0),
        Some(x) => format!("{:.0} µs", x),
        None => "—".into(),
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
    list_rect: Rect,
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
            list_rect: Rect::default(),
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
        self.list_rect = chunks[2];
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

    /// Hit-test a click. Click on a row selects it and toggles its checkbox.
    pub fn click_at(&mut self, col: u16, row: u16) {
        if !rect_contains(self.list_rect, col, row) {
            return;
        }
        let idx = (row - self.list_rect.y) as usize;
        if idx >= self.runs.len() {
            return;
        }
        self.state.select(Some(idx));
        self.toggle_selected();
    }

    /// Load the picked runs and build an in-TUI compare view. Returns
    /// the new screen for the app to push. On error, sets `self.message`
    /// and returns None so the user can fix selection and try again.
    pub fn build_compared(&mut self) -> Option<ComparedScreen> {
        let picked: Vec<PathBuf> = self
            .runs
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.path.clone())
            .collect();
        if picked.len() < 2 {
            self.message = Some("Pick at least 2 runs (space to toggle).".into());
            return None;
        }
        let loaded: anyhow::Result<Vec<_>> = picked
            .iter()
            .map(|p| crate::report::load_run(p, None))
            .collect();
        match loaded {
            Ok(runs) => Some(ComparedScreen::new(runs, self.results_root.clone())),
            Err(e) => {
                self.message = Some(format!("Failed to load runs: {:#}", e));
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// True if (col, row) falls inside the given Rect.
pub fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x && col < r.x.saturating_add(r.width) && row >= r.y && row < r.y.saturating_add(r.height)
}

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

// ---------------------------------------------------------------------------
// Report viewer (renders a per-spec result natively in the TUI; replaces the
// xdg-open-the-html-file path that doesn't work over SSH)
// ---------------------------------------------------------------------------

/// One sibling spec in the same run directory. Used for the
/// throughput-across-sweep bar chart at the bottom of the Report screen.
struct SiblingSpec {
    label: String,
    result_path: PathBuf,
    metrics: SpecMetrics,
}

pub struct ReportScreen {
    /// Result currently being shown.
    result: Option<crate::results::schema::Result>,
    /// All sibling specs in the same run dir, ordered by index. The
    /// `selected` field is an index into this vec.
    siblings: Vec<SiblingSpec>,
    selected: usize,
    /// Path to the original result.json (used to figure out the run dir
    /// for sibling discovery and to construct the report.html path for
    /// the 'b' shortcut).
    source_path: PathBuf,
    message: Option<String>,
    /// When true, the flex panel shows the client systems table instead
    /// of the throughput-across-sweep chart. Toggled with 's'.
    show_systems: bool,
}

impl ReportScreen {
    pub fn new(result_path: PathBuf) -> Self {
        let mut s = Self {
            result: None,
            siblings: Vec::new(),
            selected: 0,
            source_path: result_path.clone(),
            message: None,
            show_systems: false,
        };
        s.load(&result_path);
        s.scan_siblings(&result_path);
        s
    }

    pub fn toggle_systems(&mut self) {
        self.show_systems = !self.show_systems;
    }

    fn load(&mut self, p: &Path) {
        match std::fs::read_to_string(p)
            .map_err(|e| anyhow::anyhow!("{}", e))
            .and_then(|t| serde_json::from_str::<crate::results::schema::Result>(&t).map_err(Into::into))
        {
            Ok(r) => self.result = Some(r),
            Err(e) => self.message = Some(format!("Failed to load {}: {:#}", p.display(), e)),
        }
    }

    fn scan_siblings(&mut self, current: &Path) {
        // Sibling specs live as sibling directories of `current`'s parent
        // (i.e. the run dir contains 0001_*/, 0002_*/, ..., each with a
        // result.json). Order by directory name so indices match the spec
        // ordering the user saw in the Run screen.
        let Some(spec_dir) = current.parent() else { return };
        let Some(run_dir) = spec_dir.parent() else { return };
        let mut entries: Vec<PathBuf> = match std::fs::read_dir(run_dir) {
            Ok(it) => it
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect(),
            Err(_) => return,
        };
        entries.sort();
        for dir in entries {
            let rp = dir.join("result.json");
            if !rp.is_file() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&rp) else {
                continue;
            };
            let Ok(r) = serde_json::from_str::<crate::results::schema::Result>(&text) else {
                continue;
            };
            let metrics = SpecMetrics::from_result(&r);
            // Use the sweep-axis suffix from the directory name as the
            // chart label ("0003_local-tmp_seq-read-base_bs-1MiB" → "bs-1MiB").
            // Falls back to the directory name.
            let dname = dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let label = dname
                .splitn(4, '_')
                .nth(3)
                .map(|s| s.to_string())
                .unwrap_or_else(|| dname.clone());
            if dir == spec_dir {
                self.selected = self.siblings.len();
            }
            self.siblings.push(SiblingSpec {
                label,
                result_path: rp,
                metrics,
            });
        }
    }

    /// Jump the viewer to the next sibling spec without leaving the screen.
    pub fn select_next(&mut self) {
        if self.siblings.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.siblings.len() - 1);
        let p = self.siblings[self.selected].result_path.clone();
        self.source_path = p.clone();
        self.load(&p);
    }

    pub fn select_prev(&mut self) {
        if self.siblings.is_empty() {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        }
        let p = self.siblings[self.selected].result_path.clone();
        self.source_path = p.clone();
        self.load(&p);
    }

    /// Open the matching report.html in a browser. Best-effort; on SSH
    /// without DISPLAY this silently fails (which is fine because the
    /// in-TUI view already shows everything that matters).
    pub fn open_html(&mut self) {
        let html = self.source_path.with_file_name("report.html");
        if html.is_file() {
            open_in_browser(&html);
            self.message = Some(format!("opened {}", html.display()));
        } else {
            self.message = Some(format!("no report.html next to {}", self.source_path.display()));
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let outer = Block::default()
            .borders(Borders::ALL)
            .title(self.title_line())
            .padding(Padding::horizontal(1));
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        let result = match &self.result {
            Some(r) => r.clone(),
            None => {
                let msg = self.message.as_deref().unwrap_or("(no result loaded)");
                frame.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }), inner);
                return;
            }
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7), // metrics row
                Constraint::Length(9), // workload + target row
                Constraint::Min(5),    // sweep chart (flex)
                Constraint::Length(1), // footer
            ])
            .split(inner);

        self.render_metrics_row(frame, chunks[0], &result);
        self.render_meta_row(frame, chunks[1], &result);
        if self.show_systems {
            self.render_systems_panel(frame, chunks[2], &result);
        } else {
            self.render_sweep_chart(frame, chunks[2]);
        }
        self.render_footer(frame, chunks[3]);
    }

    /// Client systems table: per host, CPU / cores / RAM / mem / NICs /
    /// OS. Toggled with 's'. Falls back to a hint when no hardware facts
    /// were gathered (e.g. all clients local with probe failure).
    fn render_systems_panel(
        &self,
        frame: &mut Frame,
        area: Rect,
        result: &crate::results::schema::Result,
    ) {
        use crate::engine::sysinfo::{fmt_mem, fmt_nic_speed};
        let header = Row::new(vec![
            "host", "CPU", "cores", "RAM", "memory", "NICs", "OS",
        ])
        .style(
            Style::default()
                .fg(Color::Rgb(0x8b, 0x94, 0x9e))
                .add_modifier(Modifier::BOLD),
        );
        let dash = || "—".to_string();
        let rows: Vec<Row> = result
            .clients
            .iter()
            .map(|c| {
                let sys = c.system.as_ref();
                let cpu = sys.and_then(|s| s.cpu_model.clone()).unwrap_or_else(dash);
                let cores = sys
                    .and_then(|s| s.cpu_count)
                    .map(|n| n.to_string())
                    .unwrap_or_else(dash);
                let ram = sys
                    .and_then(|s| s.mem_total_bytes)
                    .map(fmt_mem)
                    .unwrap_or_else(dash);
                let mem = match sys {
                    Some(s) => match (&s.mem_type, &s.mem_speed) {
                        (Some(t), Some(sp)) => format!("{} {}", t, sp),
                        (Some(t), None) => t.clone(),
                        (None, Some(sp)) => sp.clone(),
                        (None, None) => dash(),
                    },
                    None => dash(),
                };
                let nics = match sys {
                    Some(s) if !s.nics.is_empty() => s
                        .nics
                        .iter()
                        .map(|n| format!("{} {}", n.name, fmt_nic_speed(n.speed_mbps)))
                        .collect::<Vec<_>>()
                        .join(", "),
                    _ => dash(),
                };
                let os = sys.and_then(|s| s.os.clone()).unwrap_or_else(dash);
                Row::new(vec![
                    Cell::from(c.host.clone()),
                    Cell::from(cpu),
                    Cell::from(cores),
                    Cell::from(ram),
                    Cell::from(mem),
                    Cell::from(nics),
                    Cell::from(os),
                ])
            })
            .collect();
        let widths = [
            Constraint::Length(18),
            Constraint::Min(22),
            Constraint::Length(6),
            Constraint::Length(11),
            Constraint::Length(16),
            Constraint::Min(16),
            Constraint::Min(14),
        ];
        let table = Table::new(rows, widths).header(header).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Client systems  ('s' back to chart) "),
        );
        frame.render_widget(table, area);
    }

    fn title_line(&self) -> String {
        let idx_part = if !self.siblings.is_empty() {
            format!(" {}/{}", self.selected + 1, self.siblings.len())
        } else {
            String::new()
        };
        let label = self
            .siblings
            .get(self.selected)
            .map(|s| s.label.as_str())
            .unwrap_or("");
        format!(" Report{} · {} ", idx_part, label)
    }

    fn render_metrics_row(&self, frame: &mut Frame, area: Rect, r: &crate::results::schema::Result) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(33),
                Constraint::Percentage(33),
                Constraint::Percentage(34),
            ])
            .split(area);

        let m = self
            .siblings
            .get(self.selected)
            .map(|s| s.metrics.clone())
            .unwrap_or_else(|| SpecMetrics::from_result(r));

        let tput_block = Block::default()
            .borders(Borders::ALL)
            .title(" Throughput ")
            .padding(Padding::horizontal(1));
        let tput_inner = tput_block.inner(cols[0]);
        frame.render_widget(tput_block, cols[0]);
        let tput_lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format_throughput(m.throughput_mib_s),
                Style::default()
                    .fg(Color::Rgb(0x3f, 0xb9, 0x50))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("({})", m.primary_phase),
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
        ];
        frame.render_widget(Paragraph::new(tput_lines), tput_inner);

        let iops_block = Block::default()
            .borders(Borders::ALL)
            .title(" IOPS ")
            .padding(Padding::horizontal(1));
        let iops_inner = iops_block.inner(cols[1]);
        frame.render_widget(iops_block, cols[1]);
        let iops_lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format_iops(m.iops),
                Style::default()
                    .fg(Color::Rgb(0x58, 0xa6, 0xff))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                if m.errors > 0 {
                    format!("{} errors", m.errors)
                } else {
                    "no errors".into()
                },
                Style::default().fg(if m.errors > 0 {
                    Color::Rgb(0xf8, 0x51, 0x49)
                } else {
                    Color::Rgb(0x8b, 0x94, 0x9e)
                }),
            )),
        ];
        frame.render_widget(Paragraph::new(iops_lines), iops_inner);

        let lat_block = Block::default()
            .borders(Borders::ALL)
            .title(" Latency ")
            .padding(Padding::horizontal(1));
        let lat_inner = lat_block.inner(cols[2]);
        frame.render_widget(lat_block, cols[2]);
        let lat_lines = vec![
            Line::from(vec![
                Span::styled("avg  ", Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))),
                Span::raw(format_latency(m.lat_avg_us)),
            ]),
            Line::from(vec![
                Span::styled("p50  ", Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))),
                Span::raw(format_latency(m.lat_p50_us)),
            ]),
            Line::from(vec![
                Span::styled("p95  ", Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))),
                Span::raw(format_latency(m.lat_p95_us)),
            ]),
            Line::from(vec![
                Span::styled("p99  ", Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))),
                Span::raw(format_latency(m.lat_p99_us)),
            ]),
        ];
        frame.render_widget(Paragraph::new(lat_lines), lat_inner);
    }

    fn render_meta_row(&self, frame: &mut Frame, area: Rect, r: &crate::results::schema::Result) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let wl_block = Block::default()
            .borders(Borders::ALL)
            .title(" Workload ")
            .padding(Padding::horizontal(1));
        let wl_inner = wl_block.inner(cols[0]);
        frame.render_widget(wl_block, cols[0]);
        let bs_label = human_bytes_u64(r.workload.block_size);
        let fsz = r
            .workload
            .file_size
            .map(human_bytes_u64)
            .unwrap_or_else(|| "—".into());
        let fcount = r
            .workload
            .file_count
            .map(|c| c.to_string())
            .unwrap_or_else(|| "—".into());
        let dur = r
            .workload
            .duration_s
            .map(|d| format!("{}s", d))
            .unwrap_or_else(|| "—".into());
        let wl_lines = vec![
            Line::from(meta_line("pattern", &r.workload.pattern)),
            Line::from(meta_line("block", &bs_label)),
            Line::from(meta_line(
                "rw mix",
                &format!("{}% read", r.workload.rw_mix_pct_read),
            )),
            Line::from(meta_line(
                "threads",
                &r.workload.threads_per_client.to_string(),
            )),
            Line::from(meta_line("iodepth", &r.workload.io_depth.to_string())),
            Line::from(meta_line("files", &format!("{} × {}", fcount, fsz))),
            Line::from(meta_line("duration", &dur)),
        ];
        frame.render_widget(Paragraph::new(wl_lines), wl_inner);

        let tg_block = Block::default()
            .borders(Borders::ALL)
            .title(" Target ")
            .padding(Padding::horizontal(1));
        let tg_inner = tg_block.inner(cols[1]);
        frame.render_widget(tg_block, cols[1]);
        let mut tg_lines = vec![
            Line::from(meta_line("kind", &r.target.kind)),
            Line::from(meta_line("name", &r.target.name)),
        ];
        for (k, v) in r.target.detail.iter() {
            let val = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            tg_lines.push(Line::from(meta_line(k, &val)));
        }
        let hosts: Vec<String> = r.clients.iter().map(|c| c.host.clone()).collect();
        tg_lines.push(Line::from(meta_line("hosts", &hosts.join(", "))));
        frame.render_widget(Paragraph::new(tg_lines), tg_inner);
    }

    fn render_sweep_chart(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Throughput across run  (▶ current spec) ")
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if self.siblings.is_empty() {
            frame.render_widget(
                Paragraph::new("(no sibling specs)").style(
                    Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
                ),
                inner,
            );
            return;
        }

        // Horizontal text-bar chart. ratatui's BarChart is column-only and
        // doesn't show the value next to each bar in a horizontal layout
        // the way we want, so render manually.
        let max = self
            .siblings
            .iter()
            .filter_map(|s| s.metrics.throughput_mib_s)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        let label_w = self
            .siblings
            .iter()
            .map(|s| s.label.chars().count())
            .max()
            .unwrap_or(8) as u16;
        let bar_max = inner.width.saturating_sub(label_w + 4 + 14);
        let mut lines = Vec::new();
        for (i, s) in self.siblings.iter().enumerate() {
            let v = s.metrics.throughput_mib_s.unwrap_or(0.0);
            let frac = (v / max).clamp(0.0, 1.0);
            let bar_len = (frac * bar_max as f64).round() as usize;
            let bar: String = "█".repeat(bar_len);
            let marker = if i == self.selected { "▶ " } else { "  " };
            let value = format_throughput(s.metrics.throughput_mib_s);
            let style = if i == self.selected {
                Style::default()
                    .fg(Color::Rgb(0x3f, 0xb9, 0x50))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff))
            };
            lines.push(Line::from(vec![
                Span::styled(marker.to_string(), style),
                Span::styled(format!("{:>width$}  ", s.label, width = label_w as usize), style),
                Span::styled(bar, style),
                Span::raw(format!("  {}", value)),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let msg = self.message.clone().unwrap_or_else(|| {
            "[↑/↓] cycle specs · [s] systems · [b] open report.html · [esc] back".into()
        });
        frame.render_widget(
            Paragraph::new(Span::styled(
                msg,
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
            area,
        );
    }
}

fn meta_line(k: &str, v: impl Into<String>) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("{:<10}", k),
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
        ),
        Span::raw(v.into()),
    ]
}

fn human_bytes_u64(n: u64) -> String {
    const U: &[(u64, &str)] = &[
        (1 << 40, "TiB"),
        (1 << 30, "GiB"),
        (1 << 20, "MiB"),
        (1 << 10, "KiB"),
    ];
    for (sz, unit) in U {
        if n >= *sz {
            let v = n as f64 / *sz as f64;
            if v.fract() < 0.05 {
                return format!("{:.0} {}", v, unit);
            }
            return format!("{:.2} {}", v, unit);
        }
    }
    format!("{} B", n)
}

// ---------------------------------------------------------------------------
// Compared (in-TUI side-by-side comparison of N picked runs; replaces the
// pre-v1.4.1 open-the-HTML-in-a-browser path that crashed over SSH)
// ---------------------------------------------------------------------------

/// One aligned spec row in the compared view. `per_run[i]` is the metrics
/// from `runs[i]` for this (target, workload, axes) combination — `None`
/// if that run has no matching spec.
struct ComparedRow {
    target: String,
    workload: String,
    axis_label: String,
    per_run: Vec<Option<SpecMetrics>>,
}

pub struct ComparedScreen {
    runs: Vec<crate::report::LoadedRun>,
    rows: Vec<ComparedRow>,
    state: ListState,
    list_rect: Rect,
    /// Where exported HTML lands. Set to the picked runs' parent dir so
    /// the file ends up next to the runs it compares.
    out_root: PathBuf,
    message: Option<String>,
}

impl ComparedScreen {
    pub fn new(runs: Vec<crate::report::LoadedRun>, out_root: PathBuf) -> Self {
        let rows = build_compared_rows(&runs);
        let mut state = ListState::default();
        if !rows.is_empty() {
            state.select(Some(0));
        }
        Self {
            runs,
            rows,
            state,
            list_rect: Rect::default(),
            out_root,
            message: None,
        }
    }

    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + 1) % self.rows.len()));
    }

    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = self.state.selected().unwrap_or(0);
        self.state.select(Some((i + self.rows.len() - 1) % self.rows.len()));
    }

    pub fn click_at(&mut self, col: u16, row: u16) {
        if !rect_contains(self.list_rect, col, row) {
            return;
        }
        let idx = (row - self.list_rect.y) as usize;
        if idx < self.rows.len() {
            self.state.select(Some(idx));
        }
    }

    /// Write a compare HTML file (same content as the old `compare`
    /// subcommand) next to the run dirs. No browser opens — just shows
    /// the destination path in the footer so the user can scp it back to
    /// their laptop if they want the Plotly version.
    pub fn export_html(&mut self) {
        if self.runs.len() < 2 {
            self.message = Some("Need at least 2 runs to export.".into());
            return;
        }
        let first = &self.runs[0].label;
        let last = &self.runs[self.runs.len() - 1].label;
        let out = self.out_root.join(format!("compare-{}-vs-{}.html", first, last));
        match crate::report::render_compare(&self.runs, &out, None, "elmaestro compare") {
            Ok(_) => self.message = Some(format!("Wrote {}", out.display())),
            Err(e) => self.message = Some(format!("Export failed: {:#}", e)),
        }
    }

    /// Optional escape hatch: try to open the last exported HTML in a
    /// browser. No-op on SSH hosts without DISPLAY (which is exactly why
    /// the in-TUI view exists in the first place).
    pub fn open_html(&mut self) {
        if self.runs.len() < 2 {
            self.message = Some("Need at least 2 runs to open.".into());
            return;
        }
        let first = &self.runs[0].label;
        let last = &self.runs[self.runs.len() - 1].label;
        let out = self.out_root.join(format!("compare-{}-vs-{}.html", first, last));
        if !out.is_file() {
            // Render on the fly so 'b' before 'e' still works.
            if let Err(e) =
                crate::report::render_compare(&self.runs, &out, None, "elmaestro compare")
            {
                self.message = Some(format!("Render failed: {:#}", e));
                return;
            }
        }
        open_in_browser(&out);
        self.message = Some(format!("Tried to open {} (no-op on SSH).", out.display()));
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let outer = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " Compare  ({} runs × {} specs) ",
                self.runs.len(),
                self.rows.len()
            ))
            .padding(Padding::horizontal(1));
        let inner = outer.inner(area);
        frame.render_widget(outer, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(self.runs.len() as u16 + 2), // run legend
                Constraint::Min(0),                              // body
                Constraint::Length(1),                           // footer
            ])
            .split(inner);

        self.render_legend(frame, chunks[0]);
        self.render_body(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);
    }

    fn render_legend(&self, frame: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from(Span::styled(
            "Runs being compared:",
            Style::default().fg(Color::Rgb(0x58, 0xa6, 0xff)),
        ))];
        for (i, r) in self.runs.iter().enumerate() {
            let tag = format!("  [{}] ", run_tag(i));
            lines.push(Line::from(vec![
                Span::styled(tag, Style::default().fg(run_color(i))),
                Span::raw(r.label.clone()),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_body(&mut self, frame: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(area);

        // Left: list of aligned specs.
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let label = if r.axis_label.is_empty() {
                    format!("{} · {}", r.target, r.workload)
                } else {
                    format!("{} · {} · {}", r.target, r.workload, r.axis_label)
                };
                ListItem::new(label)
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Specs (↑/↓ to cycle) "),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(0x21, 0x26, 0x2d))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        self.list_rect = cols[0];
        frame.render_stateful_widget(list, cols[0], &mut self.state);

        // Right: per-run bars for the highlighted spec.
        self.render_detail_panel(frame, cols[1]);
    }

    fn render_detail_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Selected spec across runs ")
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(sel) = self.state.selected() else { return };
        let Some(row) = self.rows.get(sel) else { return };

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(self.runs.len() as u16 + 2), // throughput block
                Constraint::Length(self.runs.len() as u16 + 2), // iops block
                Constraint::Min(self.runs.len() as u16 + 2),    // p95 block
            ])
            .split(inner);

        self.render_metric_bars(
            frame,
            sub[0],
            "Throughput",
            row,
            |m| m.throughput_mib_s,
            format_throughput,
        );
        self.render_metric_bars(frame, sub[1], "IOPS", row, |m| m.iops, format_iops);
        self.render_metric_bars(
            frame,
            sub[2],
            "p95 latency",
            row,
            |m| m.lat_p95_us,
            format_latency,
        );
    }

    fn render_metric_bars(
        &self,
        frame: &mut Frame,
        area: Rect,
        title: &str,
        row: &ComparedRow,
        pick: impl Fn(&SpecMetrics) -> Option<f64>,
        fmt: fn(Option<f64>) -> String,
    ) {
        // Find the max value across runs for this metric to scale the bars.
        let values: Vec<Option<f64>> = row
            .per_run
            .iter()
            .map(|m| m.as_ref().and_then(&pick))
            .collect();
        let max = values
            .iter()
            .filter_map(|v| *v)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        let baseline = values.first().and_then(|v| *v);
        let label_w = 5u16; // "[A] "
        let value_w = 12u16; // " 3,975 MiB/s "
        let delta_w = 12u16; // " (+12.3% )"
        let bar_max = area.width.saturating_sub(label_w + value_w + delta_w + 2) as usize;

        let mut lines: Vec<Line> = Vec::with_capacity(row.per_run.len() + 1);
        lines.push(Line::from(Span::styled(
            format!("{}", title),
            Style::default()
                .fg(Color::Rgb(0x8b, 0x94, 0x9e))
                .add_modifier(Modifier::BOLD),
        )));
        for (i, v) in values.iter().enumerate() {
            let bar_len = match v {
                Some(x) => ((x / max).clamp(0.0, 1.0) * bar_max as f64).round() as usize,
                None => 0,
            };
            let bar: String = "█".repeat(bar_len);
            let value = fmt(*v);
            let delta = match (baseline, v) {
                (Some(b), Some(x)) if i > 0 && b.abs() > f64::EPSILON => {
                    let pct = (x - b) / b * 100.0;
                    let sign = if pct >= 0.0 { "+" } else { "" };
                    format!(" ({}{:.1}%)", sign, pct)
                }
                _ => String::new(),
            };
            // Latency improves when lower; color delta accordingly.
            let delta_style = if delta.is_empty() {
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
            } else {
                let is_latency = title.contains("lat");
                let positive = delta.contains("+");
                let good = (positive && !is_latency) || (!positive && is_latency);
                Style::default().fg(if good {
                    Color::Rgb(0x3f, 0xb9, 0x50)
                } else {
                    Color::Rgb(0xf8, 0x51, 0x49)
                })
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("[{}] ", run_tag(i)),
                    Style::default().fg(run_color(i)),
                ),
                Span::styled(bar, Style::default().fg(run_color(i))),
                Span::raw(format!(" {:>10}", value)),
                Span::styled(delta, delta_style),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let msg = self.message.clone().unwrap_or_else(|| {
            "[↑/↓] cycle specs · [e] export HTML · [b] open in browser · [esc] back".into()
        });
        let style = if self.message.is_some() {
            Style::default().fg(Color::Rgb(0x3f, 0xb9, 0x50))
        } else {
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, style)),
            area,
        );
    }
}

/// Align specs across N runs by (target, workload, axes). One ComparedRow
/// per unique spec key; per_run[i] is None if that run has no matching
/// spec. Order is determined by the first run that mentioned the key, so
/// the display order matches the baseline run.
fn build_compared_rows(runs: &[crate::report::LoadedRun]) -> Vec<ComparedRow> {
    use std::collections::HashMap;
    // We need ordered insertion. IndexMap would be ideal here, but reaching
    // for it isn't necessary — track first-seen index manually.
    let mut order: Vec<(String, String, String)> = Vec::new();
    let mut by_key: HashMap<(String, String, String), Vec<Option<SpecMetrics>>> =
        HashMap::new();

    for (run_idx, run) in runs.iter().enumerate() {
        for rwa in &run.results {
            let axis_label = match &rwa.axes {
                Some(m) if !m.is_empty() => {
                    let mut parts: Vec<(String, &serde_json::Value)> =
                        m.iter().map(|(k, v)| (k.clone(), v)).collect();
                    parts.sort_by(|a, b| a.0.cmp(&b.0));
                    parts
                        .into_iter()
                        .map(|(k, v)| match v {
                            serde_json::Value::String(s) => format!("{}={}", k, s),
                            other => format!("{}={}", k, other),
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
                _ => String::new(),
            };
            let key = (
                rwa.result.target.name.clone(),
                rwa.result.workload.name.clone(),
                axis_label.clone(),
            );
            let slot = by_key.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                vec![None; runs.len()]
            });
            slot[run_idx] = Some(SpecMetrics::from_result(&rwa.result));
        }
    }

    order
        .into_iter()
        .map(|k| {
            let per_run = by_key.remove(&k).unwrap_or_else(|| vec![None; runs.len()]);
            ComparedRow {
                target: k.0,
                workload: k.1,
                axis_label: k.2,
                per_run,
            }
        })
        .collect()
}

/// Two-letter tags ([A], [B], ... [Z], then [AA]+) for the run legend.
fn run_tag(i: usize) -> String {
    if i < 26 {
        ((b'A' + i as u8) as char).to_string()
    } else {
        format!("{}", i + 1)
    }
}

/// Distinct colors for up to ~6 runs. Colors past that wrap.
fn run_color(i: usize) -> Color {
    const PALETTE: &[(u8, u8, u8)] = &[
        (0x58, 0xa6, 0xff), // blue
        (0x3f, 0xb9, 0x50), // green
        (0xd2, 0x99, 0x22), // amber
        (0xb1, 0x86, 0xff), // purple
        (0xf8, 0x51, 0x49), // red
        (0x39, 0xc5, 0xcf), // cyan
    ];
    let (r, g, b) = PALETTE[i % PALETTE.len()];
    Color::Rgb(r, g, b)
}
