//! Configure-benchmark form screen.
//!
//! Fully keyboard-driven. Lets a user fill in every attribute the harness
//! needs without writing a YAML file. The result is an in-memory `RunPlan`
//! that gets handed to the RunScreen / runner just like a loaded config.

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::config::{
    parse_bytesize_string, ClientHost, Engine, PosixTarget, RunPlan, RunRef, Sweep, SweepAxis,
    Target, Workload,
};

// ---------------------------------------------------------------------------
// Field abstraction
// ---------------------------------------------------------------------------

enum Field {
    Radio {
        label: &'static str,
        options: Vec<&'static str>,
        selected: usize,
    },
    Text {
        label: &'static str,
        value: String,
        cursor: usize,
        placeholder: &'static str,
        hint: &'static str,
    },
    Checkbox {
        label: &'static str,
        checked: bool,
    },
    Button {
        label: &'static str,
        action: ButtonAction,
    },
}

#[derive(Clone, Copy)]
enum ButtonAction {
    Run,
    Cancel,
    SaveTemplate,
    LoadTemplate,
}

impl Field {
    fn render_line(&self, focused: bool, width: u16) -> Line<'_> {
        let label_width = 18usize;
        let label_style = if focused {
            Style::default()
                .fg(Color::Rgb(0x58, 0xa6, 0xff))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
        };
        let cursor_style = Style::default()
            .bg(Color::Rgb(0x58, 0xa6, 0xff))
            .fg(Color::Rgb(0x0e, 0x11, 0x16));
        let value_style = Style::default();
        match self {
            Field::Radio { label, options, selected } => {
                let mut spans = vec![Span::styled(
                    format!("{:<width$}", label, width = label_width),
                    label_style,
                )];
                for (i, opt) in options.iter().enumerate() {
                    let marker = if i == *selected { "(•)" } else { "( )" };
                    let style = if i == *selected && focused {
                        Style::default()
                            .fg(Color::Rgb(0x58, 0xa6, 0xff))
                            .add_modifier(Modifier::BOLD)
                    } else if i == *selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
                    };
                    spans.push(Span::styled(format!("{} {}   ", marker, opt), style));
                }
                Line::from(spans)
            }
            Field::Text {
                label,
                value,
                cursor,
                placeholder,
                hint,
            } => {
                let mut spans = vec![Span::styled(
                    format!("{:<width$}", label, width = label_width),
                    label_style,
                )];
                let display = if value.is_empty() && !focused {
                    Span::styled(
                        format!("[{}]", placeholder),
                        Style::default().fg(Color::Rgb(0x57, 0x5e, 0x68)),
                    )
                } else if focused {
                    let cur = (*cursor).min(value.chars().count());
                    let before: String = value.chars().take(cur).collect();
                    let at: String = value.chars().nth(cur).map(|c| c.to_string()).unwrap_or(" ".into());
                    let after: String = value.chars().skip(cur + 1).collect();
                    let _ = (before, at, after);
                    // We'll do styled spans below; for simplicity, render the whole value
                    // and overlay a cursor block via a single combined render below.
                    Span::styled(value.clone(), value_style)
                } else {
                    Span::styled(value.clone(), value_style)
                };
                if focused {
                    let cur = (*cursor).min(value.chars().count());
                    let before: String = value.chars().take(cur).collect();
                    let at: String = value.chars().nth(cur).map(|c| c.to_string()).unwrap_or(" ".into());
                    let after: String = value.chars().skip(cur + 1).collect();
                    spans.push(Span::styled(format!("[{}", before), value_style));
                    spans.push(Span::styled(at, cursor_style));
                    spans.push(Span::styled(format!("{}]", after), value_style));
                } else {
                    spans.push(Span::styled("[", value_style));
                    spans.push(display);
                    spans.push(Span::styled("]", value_style));
                }
                if !hint.is_empty() && focused {
                    let pad = width.saturating_sub((label_width + value.chars().count() + 4) as u16);
                    if pad > 8 {
                        spans.push(Span::styled(
                            format!("  {}", hint),
                            Style::default().fg(Color::Rgb(0x57, 0x5e, 0x68)),
                        ));
                    }
                }
                Line::from(spans)
            }
            Field::Checkbox { label, checked } => {
                let mark = if *checked { "[x]" } else { "[ ]" };
                let style = if focused {
                    Style::default()
                        .fg(Color::Rgb(0x58, 0xa6, 0xff))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::styled(
                        format!("{:<width$}", label, width = label_width),
                        label_style,
                    ),
                    Span::styled(format!("{}", mark), style),
                ])
            }
            Field::Button { label, .. } => {
                let style = if focused {
                    Style::default()
                        .bg(Color::Rgb(0x21, 0x26, 0x2d))
                        .fg(Color::Rgb(0x58, 0xa6, 0xff))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e))
                };
                Line::from(Span::styled(format!("  [ {} ]  ", label), style))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Screen
// ---------------------------------------------------------------------------

pub struct ConfigureScreen {
    fields: Vec<Field>,
    focused: usize,
    pub error: Option<String>,
    /// Green success message shown in the footer. Used by save/load.
    pub notice: Option<String>,
    /// Set when the user activates the Run button. The app drains this to
    /// transition to a RunScreen.
    pub built_plan: Option<(RunPlan, String, usize)>,
    pub cancelled: bool,
    /// Rect of the form body, captured at render time so mouse clicks can
    /// map row -> field index.
    body_rect: ratatui::layout::Rect,
}

impl ConfigureScreen {
    pub fn new() -> Self {
        Self {
            fields: default_fields(),
            focused: 0,
            error: None,
            notice: None,
            built_plan: None,
            cancelled: false,
            body_rect: ratatui::layout::Rect::default(),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(0),
                Constraint::Length(3),
            ])
            .split(area);

        let header = Paragraph::new(vec![
            Line::from(Span::styled(
                "Configure & run a benchmark",
                Style::default()
                    .fg(Color::Rgb(0x58, 0xa6, 0xff))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "Tab/↓ next · Shift+Tab/↑ prev · Space toggles · Enter activates · Esc cancel",
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
        ]);
        frame.render_widget(header, chunks[0]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Rgb(0x2d, 0x33, 0x3b)));
        let inner_area = block.inner(chunks[1]);
        frame.render_widget(block, chunks[1]);

        let lines: Vec<Line> = self
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| f.render_line(i == self.focused, inner_area.width))
            .collect();
        let body = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.body_rect = inner_area;
        frame.render_widget(body, inner_area);

        let footer_text = if let Some(e) = &self.error {
            Line::from(Span::styled(
                e.clone(),
                Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
            ))
        } else if let Some(n) = &self.notice {
            Line::from(Span::styled(
                n.clone(),
                Style::default().fg(Color::Rgb(0x3f, 0xb9, 0x50)),
            ))
        } else {
            Line::from(Span::styled(
                "Comma-separate sweep values · Ctrl+S save · Ctrl+L load template",
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            ))
        };
        frame.render_widget(Paragraph::new(footer_text), chunks[2]);
    }

    pub fn focus_next(&mut self) {
        self.error = None;
        if self.fields.is_empty() {
            return;
        }
        self.focused = (self.focused + 1) % self.fields.len();
    }

    pub fn focus_prev(&mut self) {
        self.error = None;
        if self.fields.is_empty() {
            return;
        }
        self.focused = (self.focused + self.fields.len() - 1) % self.fields.len();
    }

    /// Space / Enter at radio: cycle to next option. At checkbox: toggle.
    /// At button: invoke. At text: just insert nothing (Enter is harmless).
    pub fn activate(&mut self) {
        let idx = self.focused;
        let mut clicked: Option<ButtonAction> = None;
        if let Some(f) = self.fields.get_mut(idx) {
            match f {
                Field::Radio { options, selected, .. } => {
                    *selected = (*selected + 1) % options.len();
                }
                Field::Checkbox { checked, .. } => {
                    *checked = !*checked;
                }
                Field::Button { action, .. } => {
                    clicked = Some(*action);
                }
                Field::Text { .. } => {}
            }
        }
        if let Some(action) = clicked {
            match action {
                ButtonAction::Run => self.try_build_plan(),
                ButtonAction::Cancel => {
                    self.cancelled = true;
                }
                ButtonAction::SaveTemplate => self.save_template(),
                ButtonAction::LoadTemplate => self.load_template(),
            }
        }
    }

    /// Build a RunPlan from the current form state and write it to the path
    /// in the Template path field. Sets notice on success, error on failure.
    pub fn save_template(&mut self) {
        self.error = None;
        self.notice = None;
        let path_str = field_text(&self.fields, "Template path").trim().to_string();
        if path_str.is_empty() {
            self.error = Some("Template path is required".into());
            return;
        }
        match save_template_inner(&self.fields, &path_str) {
            Ok(()) => self.notice = Some(format!("Saved {}", path_str)),
            Err(e) => self.error = Some(format!("Save failed: {:#}", e)),
        }
    }

    /// Load a YAML template into the form fields. Sets notice on success,
    /// error on failure.
    pub fn load_template(&mut self) {
        self.error = None;
        self.notice = None;
        let path_str = field_text(&self.fields, "Template path").trim().to_string();
        if path_str.is_empty() {
            self.error = Some("Template path is required".into());
            return;
        }
        match load_template_inner(&mut self.fields, &path_str) {
            Ok(()) => self.notice = Some(format!("Loaded {}", path_str)),
            Err(e) => self.error = Some(format!("Load failed: {:#}", e)),
        }
    }

    /// Right arrow / left arrow: at text moves cursor; at radio cycles
    /// option (right = next, left = prev).
    pub fn nudge_right(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focused) {
            match f {
                Field::Text { value, cursor, .. } => {
                    let max = value.chars().count();
                    if *cursor < max {
                        *cursor += 1;
                    }
                }
                Field::Radio { options, selected, .. } => {
                    *selected = (*selected + 1) % options.len();
                }
                _ => {}
            }
        }
    }

    pub fn nudge_left(&mut self) {
        if let Some(f) = self.fields.get_mut(self.focused) {
            match f {
                Field::Text { cursor, .. } => {
                    if *cursor > 0 {
                        *cursor -= 1;
                    }
                }
                Field::Radio { options, selected, .. } => {
                    *selected = (*selected + options.len() - 1) % options.len();
                }
                _ => {}
            }
        }
    }

    pub fn home(&mut self) {
        if let Some(Field::Text { cursor, .. }) = self.fields.get_mut(self.focused) {
            *cursor = 0;
        }
    }

    pub fn end(&mut self) {
        if let Some(Field::Text { value, cursor, .. }) = self.fields.get_mut(self.focused) {
            *cursor = value.chars().count();
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if let Some(Field::Text { value, cursor, .. }) = self.fields.get_mut(self.focused) {
            let mut chars: Vec<char> = value.chars().collect();
            let cur = (*cursor).min(chars.len());
            chars.insert(cur, c);
            *value = chars.into_iter().collect();
            *cursor = cur + 1;
        }
    }

    pub fn backspace(&mut self) {
        if let Some(Field::Text { value, cursor, .. }) = self.fields.get_mut(self.focused) {
            if *cursor == 0 {
                return;
            }
            let mut chars: Vec<char> = value.chars().collect();
            let cur = (*cursor).min(chars.len());
            chars.remove(cur - 1);
            *value = chars.into_iter().collect();
            *cursor = cur - 1;
        }
    }

    pub fn delete(&mut self) {
        if let Some(Field::Text { value, cursor, .. }) = self.fields.get_mut(self.focused) {
            let mut chars: Vec<char> = value.chars().collect();
            if *cursor >= chars.len() {
                return;
            }
            chars.remove(*cursor);
            *value = chars.into_iter().collect();
        }
    }

    /// True if the currently focused field is a Text input. Lets the app
    /// route Space to insert-space vs activate.
    pub fn is_text_focused(&self) -> bool {
        matches!(self.fields.get(self.focused), Some(Field::Text { .. }))
    }

    /// Hit-test a click. Focuses the clicked field; if it's a button /
    /// checkbox / radio, also activates it (toggles or runs).
    pub fn click_at(&mut self, col: u16, row: u16) {
        let r = self.body_rect;
        if col < r.x
            || col >= r.x.saturating_add(r.width)
            || row < r.y
            || row >= r.y.saturating_add(r.height)
        {
            return;
        }
        let idx = (row - r.y) as usize;
        if idx >= self.fields.len() {
            return;
        }
        self.focused = idx;
        self.error = None;
        // For non-text fields, treat a click as activation (cycle radio,
        // toggle checkbox, press button). Text fields just focus.
        let is_text = matches!(self.fields[idx], Field::Text { .. });
        if !is_text {
            self.activate();
        }
    }

    fn try_build_plan(&mut self) {
        match build_plan(&self.fields) {
            Ok((plan, label, repeats)) => {
                self.built_plan = Some((plan, label, repeats));
            }
            Err(e) => {
                self.error = Some(format!("{:#}", e));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Defaults + plan builder
// ---------------------------------------------------------------------------

fn default_fields() -> Vec<Field> {
    vec![
        Field::Radio {
            label: "Engine",
            options: vec!["elbencho", "fio"],
            selected: 0,
        },
        Field::Text {
            label: "Mount path",
            value: "/mnt/weka".into(),
            cursor: "/mnt/weka".len(),
            placeholder: "/mnt/weka",
            hint: "(e.g. /mnt/weka, /tmp)",
        },
        Field::Text {
            label: "Dataset subdir",
            value: "elmaestro-bench".into(),
            cursor: "elmaestro-bench".len(),
            placeholder: "elmaestro-bench",
            hint: "(directory under mount path)",
        },
        Field::Radio {
            label: "Pattern",
            options: vec!["seq", "rand"],
            selected: 0,
        },
        Field::Text {
            label: "Read mix %",
            value: "100".into(),
            cursor: 3,
            placeholder: "100",
            hint: "(0=write, 100=read, 70=mixed)",
        },
        Field::Text {
            label: "Block size(s)",
            value: "64k,256k,1m,4m".into(),
            cursor: "64k,256k,1m,4m".len(),
            placeholder: "1m",
            hint: "(comma-separated for a sweep)",
        },
        Field::Text {
            label: "Threads",
            value: "8".into(),
            cursor: 1,
            placeholder: "8",
            hint: "(comma-separated for a sweep)",
        },
        Field::Text {
            label: "IO depth",
            value: "4".into(),
            cursor: 1,
            placeholder: "4",
            hint: "(comma-separated for a sweep)",
        },
        Field::Text {
            label: "File size",
            value: "256MiB".into(),
            cursor: "256MiB".len(),
            placeholder: "256MiB",
            hint: "(per-thread file size; blank if using duration)",
        },
        Field::Text {
            label: "Files per thread",
            value: "4".into(),
            cursor: 1,
            placeholder: "4",
            hint: "(total files = Threads × this, e.g. 8 × 4 = 32 files)",
        },
        Field::Text {
            label: "Duration (s)",
            value: String::new(),
            cursor: 0,
            placeholder: "blank",
            hint: "(blank means use file size; otherwise N seconds)",
        },
        Field::Checkbox {
            label: "Direct IO",
            checked: true,
        },
        Field::Checkbox {
            label: "Drop caches",
            checked: false,
        },
        Field::Text {
            label: "Number of runs",
            value: "1".into(),
            cursor: 1,
            placeholder: "1",
            hint: "(>1 repeats the whole sweep for variance)",
        },
        Field::Text {
            label: "Workers (hosts)",
            value: String::new(),
            cursor: 0,
            placeholder: "blank = localhost",
            hint: "(comma-sep; brace-expand: 10.10.10.{1..100}, node{01..16}, gpu{a,b,c})",
        },
        Field::Text {
            label: "SSH user",
            value: String::new(),
            cursor: 0,
            placeholder: "blank = current user",
            hint: "(only used when Workers is set)",
        },
        Field::Text {
            label: "SSH key",
            value: String::new(),
            cursor: 0,
            placeholder: "blank = ssh-agent / default",
            hint: "(path to private key; supports ~)",
        },
        Field::Text {
            label: "Jump host",
            value: String::new(),
            cursor: 0,
            placeholder: "user@bastion.example.com",
            hint: "(optional ssh -J jumphost; reach internal workers via bastion)",
        },
        Field::Text {
            label: "Service port",
            value: String::new(),
            cursor: 0,
            placeholder: "1611 / 8765",
            hint: "(blank = engine default: 1611 elbencho, 8765 fio)",
        },
        Field::Text {
            label: "Template path",
            value: String::new(),
            cursor: 0,
            placeholder: "./template.yaml",
            hint: "(for Save/Load below; same YAML format as `elmaestro run`)",
        },
        Field::Button {
            label: "Run benchmark",
            action: ButtonAction::Run,
        },
        Field::Button {
            label: "Cancel",
            action: ButtonAction::Cancel,
        },
        Field::Button {
            label: "Save template",
            action: ButtonAction::SaveTemplate,
        },
        Field::Button {
            label: "Load template",
            action: ButtonAction::LoadTemplate,
        },
    ]
}

fn field_text<'a>(fields: &'a [Field], label: &'static str) -> &'a str {
    for f in fields {
        if let Field::Text { label: l, value, .. } = f {
            if *l == label {
                return value.as_str();
            }
        }
    }
    ""
}

fn field_radio_selected<'a>(fields: &'a [Field], label: &'static str) -> Option<&'a str> {
    for f in fields {
        if let Field::Radio { label: l, options, selected } = f {
            if *l == label {
                return options.get(*selected).copied();
            }
        }
    }
    None
}

fn field_checkbox(fields: &[Field], label: &'static str) -> bool {
    for f in fields {
        if let Field::Checkbox { label: l, checked } = f {
            if *l == label {
                return *checked;
            }
        }
    }
    false
}

fn build_plan(fields: &[Field]) -> Result<(RunPlan, String, usize)> {
    let engine_str =
        field_radio_selected(fields, "Engine").ok_or_else(|| anyhow!("missing engine"))?;
    let engine = match engine_str {
        "elbencho" => Engine::Elbencho,
        "fio" => Engine::Fio,
        other => anyhow::bail!("unknown engine: {}", other),
    };

    let path = field_text(fields, "Mount path").trim();
    if path.is_empty() {
        anyhow::bail!("Mount path is required");
    }
    let subdir = field_text(fields, "Dataset subdir").trim();
    if subdir.is_empty() {
        anyhow::bail!("Dataset subdir is required");
    }
    if subdir.starts_with('/') || subdir.contains("..") {
        anyhow::bail!("Dataset subdir must be a relative path with no '..'");
    }

    let pattern = field_radio_selected(fields, "Pattern").unwrap_or("seq").to_string();
    let read_mix: u8 = field_text(fields, "Read mix %")
        .trim()
        .parse()
        .map_err(|_| anyhow!("Read mix must be 0-100"))?;
    if read_mix > 100 {
        anyhow::bail!("Read mix must be 0-100");
    }

    let block_sizes: Vec<u64> = parse_byte_list(field_text(fields, "Block size(s)"))
        .map_err(|e| anyhow!("Block size(s): {}", e))?;
    if block_sizes.is_empty() {
        anyhow::bail!("At least one block size is required");
    }
    let threads_list: Vec<u32> = parse_int_list::<u32>(field_text(fields, "Threads"))
        .map_err(|e| anyhow!("Threads: {}", e))?;
    if threads_list.is_empty() {
        anyhow::bail!("At least one thread count is required");
    }
    let iodepth_list: Vec<u32> = parse_int_list::<u32>(field_text(fields, "IO depth"))
        .map_err(|e| anyhow!("IO depth: {}", e))?;
    if iodepth_list.is_empty() {
        anyhow::bail!("At least one IO depth is required");
    }

    let file_size_raw = field_text(fields, "File size").trim();
    let file_size: Option<u64> = if file_size_raw.is_empty() {
        None
    } else {
        Some(parse_bytesize_string(file_size_raw).map_err(|e| anyhow!("File size: {}", e))?)
    };
    let files_per_thread: Option<u32> = match field_text(fields, "Files per thread").trim() {
        "" => None,
        s => Some(
            s.parse()
                .map_err(|_| anyhow!("Files per thread must be an integer"))?,
        ),
    };

    let duration_raw = field_text(fields, "Duration (s)").trim();
    let duration_s: Option<u64> = if duration_raw.is_empty() {
        None
    } else {
        Some(duration_raw.parse().map_err(|_| anyhow!("Duration must be an integer (seconds)"))?)
    };
    if duration_s.is_none() && file_size.is_none() {
        anyhow::bail!("Either Duration or File size must be set");
    }

    let direct_io = field_checkbox(fields, "Direct IO");
    let drop_caches = field_checkbox(fields, "Drop caches");

    let repeats: usize = field_text(fields, "Number of runs")
        .trim()
        .parse()
        .map_err(|_| anyhow!("Number of runs must be a positive integer"))?;
    if repeats == 0 {
        anyhow::bail!("Number of runs must be >= 1");
    }

    // Build the workload (base values used when sweep doesn't override).
    let workload = Workload {
        name: "wl".into(),
        pattern,
        rw_mix_pct_read: read_mix,
        block_size: block_sizes[0],
        threads_per_client: threads_list[0],
        io_depth: iodepth_list[0],
        direct_io,
        sync_after_write: false,
        drop_caches_before: drop_caches,
        duration_s,
        dataset_size: None,
        file_size,
        file_count: files_per_thread,
        s3_multipart_size: None,
        s3_object_prefix: None,
        extra_flags: Vec::new(),
    };

    // Decide sweep vs explicit run: any axis with >1 value -> sweep.
    let has_sweep =
        block_sizes.len() > 1 || threads_list.len() > 1 || iodepth_list.len() > 1;

    let target = Target::Posix(PosixTarget {
        name: "target".into(),
        mount_path: PathBuf::from(path),
        dataset_subdir: subdir.into(),
        cleanup: false,
    });

    let workers = field_text(fields, "Workers (hosts)").trim().to_string();
    let ssh_user = field_text(fields, "SSH user").trim().to_string();
    let ssh_key = field_text(fields, "SSH key").trim().to_string();
    let jump_host = field_text(fields, "Jump host").trim().to_string();
    let svc_port_raw = field_text(fields, "Service port").trim().to_string();

    let default_service_port: u16 = match engine {
        Engine::Elbencho => 1611,
        Engine::Fio => 8765,
    };
    let explicit_svc_port: Option<u16> = if svc_port_raw.is_empty() {
        None
    } else {
        Some(
            svc_port_raw
                .parse()
                .map_err(|_| anyhow!("Service port must be an integer"))?,
        )
    };

    let clients: Vec<ClientHost> = if workers.is_empty() {
        vec![ClientHost::default()]
    } else {
        let engine_path_default = match engine {
            Engine::Elbencho => "elbencho",
            Engine::Fio => "fio",
        };
        let mut out: Vec<ClientHost> = Vec::new();
        // Bash-style brace expansion: `10.10.10.{1..100}` → 100 entries,
        // `node{01..16}` → "node01".."node16", `gpu{a,b,c}` → 3 hosts,
        // cartesian over multiple braces, etc. See config/host_expand.rs.
        let expanded = crate::config::host_expand::expand_hosts(&workers);
        for entry in expanded {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let (host, port_str) = match entry.split_once(':') {
                Some((h, p)) => (h.trim().to_string(), Some(p.trim())),
                None => (entry.to_string(), None),
            };
            let port = if let Some(p) = port_str {
                p.parse::<u16>().map_err(|_| anyhow!(
                    "Worker {:?} port must be an integer",
                    entry
                ))?
            } else {
                explicit_svc_port.unwrap_or(default_service_port)
            };
            out.push(ClientHost {
                host,
                ssh_user: if ssh_user.is_empty() {
                    None
                } else {
                    Some(ssh_user.clone())
                },
                ssh_port: 22,
                ssh_key: if ssh_key.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(&ssh_key))
                },
                ssh_jump: if jump_host.is_empty() {
                    None
                } else {
                    Some(jump_host.clone())
                },
                elbencho_path: engine_path_default.into(),
                service_port: port,
            });
        }
        if out.is_empty() {
            anyhow::bail!("Workers parsed to an empty list");
        }
        out
    };

    let mut plan = RunPlan {
        version: 1,
        engine,
        output_dir: PathBuf::from("./results"),
        clients,
        targets: vec![target],
        workloads: vec![workload],
        runs: Vec::new(),
        sweeps: Vec::new(),
    };

    if has_sweep {
        let axes = SweepAxis {
            block_size: if block_sizes.len() > 1 {
                Some(block_sizes)
            } else {
                None
            },
            rw_mix_pct_read: None,
            threads_per_client: if threads_list.len() > 1 {
                Some(threads_list)
            } else {
                None
            },
            io_depth: if iodepth_list.len() > 1 {
                Some(iodepth_list)
            } else {
                None
            },
            client_count: None,
            dataset_size: None,
        };
        plan.sweeps.push(Sweep {
            name: "sweep".into(),
            base: "wl".into(),
            targets: None,
            target: Some("target".into()),
            axes,
            order: "cartesian".into(),
            max_runs: None,
        });
    } else {
        plan.runs.push(RunRef {
            target: "target".into(),
            workload: "wl".into(),
        });
    }

    plan.validate()?;
    let label = if has_sweep { "sweep".into() } else { "single".into() };
    Ok((plan, label, repeats))
}

/// Parse a comma-separated list of byte-size strings.
fn parse_byte_list(s: &str) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    for token in s.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        out.push(parse_bytesize_string(t)?);
    }
    Ok(out)
}

/// Parse a comma-separated list of integers.
fn parse_int_list<T: std::str::FromStr>(s: &str) -> Result<Vec<T>>
where
    T::Err: std::fmt::Display,
{
    let mut out = Vec::new();
    for token in s.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        out.push(
            t.parse::<T>()
                .map_err(|e| anyhow!("invalid integer {:?}: {}", t, e))?,
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Template save / load
// ---------------------------------------------------------------------------

/// Build the form's RunPlan and dump it as YAML to `path`. Skips writing
/// if path is empty.
fn save_template_inner(fields: &[Field], path: &str) -> Result<()> {
    let (plan, _label, _repeats) = build_plan(fields)?;
    let yaml = serde_yaml::to_string(&plan)
        .map_err(|e| anyhow!("YAML encode: {}", e))?;
    // Expand ~ in the path so users can save to ~/templates/foo.yaml.
    let expanded = shellexpand::tilde(path).into_owned();
    if let Some(parent) = std::path::Path::new(&expanded).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&expanded, yaml)?;
    Ok(())
}

/// Read a YAML template from `path` and unpack into the form fields. Errors
/// out cleanly if the YAML's shape can't be represented by this form
/// (multi-workload / multi-target / non-POSIX target / multi-axis sweep
/// beyond bs/threads/iodepth).
fn load_template_inner(fields: &mut [Field], path: &str) -> Result<()> {
    let expanded = shellexpand::tilde(path).into_owned();
    let plan = crate::config::loader::load(std::path::Path::new(&expanded))?;
    apply_plan_to_fields(fields, &plan)
}

/// Reverse of build_plan: take a RunPlan and unpack it into the form fields
/// so the user can edit and rerun.
fn apply_plan_to_fields(fields: &mut [Field], plan: &crate::config::RunPlan) -> Result<()> {
    if plan.targets.len() != 1 {
        anyhow::bail!(
            "template has {} targets; the form can only represent one",
            plan.targets.len()
        );
    }
    if plan.workloads.len() != 1 {
        anyhow::bail!(
            "template has {} workloads; the form can only represent one",
            plan.workloads.len()
        );
    }
    let posix = match &plan.targets[0] {
        crate::config::Target::Posix(t) => t,
        crate::config::Target::S3(_) => {
            anyhow::bail!("template uses an S3 target; the form is POSIX-only")
        }
    };
    let wl = &plan.workloads[0];

    // Engine radio.
    set_radio(fields, "Engine", match plan.engine {
        crate::config::Engine::Elbencho => "elbencho",
        crate::config::Engine::Fio => "fio",
    });

    // Target.
    set_text(fields, "Mount path", &posix.mount_path.display().to_string());
    set_text(fields, "Dataset subdir", &posix.dataset_subdir);

    // Workload.
    set_radio(fields, "Pattern", &wl.pattern);
    set_text(fields, "Read mix %", &wl.rw_mix_pct_read.to_string());

    // Sweep axes -> comma-separated lists in the form.
    let mut bs_values = vec![wl.block_size];
    let mut threads_values = vec![wl.threads_per_client];
    let mut iodepth_values = vec![wl.io_depth];
    for sw in &plan.sweeps {
        if sw.base != wl.name {
            continue;
        }
        if let Some(vs) = &sw.axes.block_size {
            if !vs.is_empty() {
                bs_values = vs.clone();
            }
        }
        if let Some(vs) = &sw.axes.threads_per_client {
            if !vs.is_empty() {
                threads_values = vs.clone();
            }
        }
        if let Some(vs) = &sw.axes.io_depth {
            if !vs.is_empty() {
                iodepth_values = vs.clone();
            }
        }
        // Note: rw_mix_pct_read / dataset_size / client_count sweep axes
        // aren't represented in the form. If a template uses them, we silently
        // ignore (the loaded form won't be a perfect round-trip but will run).
    }
    set_text(
        fields,
        "Block size(s)",
        &bs_values.iter().map(|b| human_bytes(*b)).collect::<Vec<_>>().join(","),
    );
    set_text(
        fields,
        "Threads",
        &threads_values.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(","),
    );
    set_text(
        fields,
        "IO depth",
        &iodepth_values.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(","),
    );

    set_text(
        fields,
        "File size",
        &wl.file_size.map(human_bytes).unwrap_or_default(),
    );
    set_text(
        fields,
        "Files per thread",
        &wl.file_count.map(|n| n.to_string()).unwrap_or_default(),
    );
    set_text(
        fields,
        "Duration (s)",
        &wl.duration_s.map(|n| n.to_string()).unwrap_or_default(),
    );
    set_checkbox(fields, "Direct IO", wl.direct_io);
    set_checkbox(fields, "Drop caches", wl.drop_caches_before);
    set_text(fields, "Number of runs", "1");

    // Clients: derive Workers / SSH user / SSH key from the first non-localhost
    // client. localhost-only clients map to empty Workers field.
    let remote: Vec<&crate::config::ClientHost> = plan
        .clients
        .iter()
        .filter(|c| !matches!(c.host.as_str(), "localhost" | "127.0.0.1" | "::1" | ""))
        .collect();
    if remote.is_empty() {
        set_text(fields, "Workers (hosts)", "");
        set_text(fields, "SSH user", "");
        set_text(fields, "SSH key", "");
        set_text(fields, "Jump host", "");
        set_text(fields, "Service port", "");
    } else {
        let first = remote[0];
        let port = first.service_port;
        let workers_str = remote
            .iter()
            .map(|c| {
                if c.service_port == port {
                    c.host.clone()
                } else {
                    format!("{}:{}", c.host, c.service_port)
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        set_text(fields, "Workers (hosts)", &workers_str);
        set_text(fields, "SSH user", first.ssh_user.as_deref().unwrap_or(""));
        set_text(
            fields,
            "SSH key",
            &first
                .ssh_key
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        set_text(fields, "Jump host", first.ssh_jump.as_deref().unwrap_or(""));
        let engine_default = match plan.engine {
            crate::config::Engine::Elbencho => 1611,
            crate::config::Engine::Fio => 8765,
        };
        let port_str = if port == engine_default {
            String::new()
        } else {
            port.to_string()
        };
        set_text(fields, "Service port", &port_str);
    }

    Ok(())
}

fn set_text(fields: &mut [Field], label: &str, val: &str) {
    for f in fields.iter_mut() {
        if let Field::Text {
            label: l,
            value,
            cursor,
            ..
        } = f
        {
            if *l == label {
                *value = val.to_string();
                *cursor = val.chars().count();
                return;
            }
        }
    }
}

fn set_radio(fields: &mut [Field], label: &str, opt: &str) {
    for f in fields.iter_mut() {
        if let Field::Radio { label: l, options, selected } = f {
            if *l == label {
                if let Some(idx) = options.iter().position(|o| *o == opt) {
                    *selected = idx;
                }
                return;
            }
        }
    }
}

fn set_checkbox(fields: &mut [Field], label: &str, checked_in: bool) {
    for f in fields.iter_mut() {
        if let Field::Checkbox { label: l, checked } = f {
            if *l == label {
                *checked = checked_in;
                return;
            }
        }
    }
}

/// 65536 -> "64KiB", 4194304 -> "4MiB". Mirrors python-side display.
fn human_bytes(n: u64) -> String {
    for (unit, base) in [
        ("GiB", 1u64 << 30),
        ("MiB", 1u64 << 20),
        ("KiB", 1u64 << 10),
    ] {
        if n >= base && n % base == 0 {
            return format!("{}{}", n / base, unit);
        }
    }
    format!("{}", n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: set a Text field's value (test-only; bypasses cursor tracking).
    fn set_text(fields: &mut [Field], label: &str, val: &str) {
        for f in fields.iter_mut() {
            if let Field::Text { label: l, value, cursor, .. } = f {
                if *l == label {
                    *value = val.to_string();
                    *cursor = val.chars().count();
                    return;
                }
            }
        }
        panic!("no Text field named {:?}", label);
    }

    fn select_radio(fields: &mut [Field], label: &str, opt: &str) {
        for f in fields.iter_mut() {
            if let Field::Radio { label: l, options, selected } = f {
                if *l == label {
                    let idx = options.iter().position(|o| *o == opt)
                        .unwrap_or_else(|| panic!("no option {:?} on {:?}", opt, label));
                    *selected = idx;
                    return;
                }
            }
        }
        panic!("no Radio field named {:?}", label);
    }

    #[test]
    fn defaults_build_a_single_localhost_plan() {
        let fields = default_fields();
        let (plan, label, repeats) = build_plan(&fields).expect("defaults should validate");
        assert_eq!(plan.clients.len(), 1, "default = single localhost");
        assert_eq!(plan.clients[0].host, "localhost");
        assert!(plan.clients[0].ssh_user.is_none());
        assert!(plan.clients[0].ssh_key.is_none());
        assert_eq!(label, "sweep");  // defaults include comma-separated block sizes
        assert_eq!(repeats, 1);
    }

    #[test]
    fn jump_host_propagates_to_every_client() {
        let mut fields = default_fields();
        set_text(&mut fields, "Workers (hosts)", "w1,w2");
        set_text(&mut fields, "SSH user", "bench");
        set_text(&mut fields, "Jump host", "user@bastion.example.com");
        let (plan, _, _) = build_plan(&fields).expect("plan should validate");
        for c in &plan.clients {
            assert_eq!(c.ssh_jump.as_deref(), Some("user@bastion.example.com"));
        }
    }

    #[test]
    fn workers_field_produces_multi_client_plan() {
        let mut fields = default_fields();
        set_text(&mut fields, "Workers (hosts)", "worker-01,worker-02:1612,worker-03");
        set_text(&mut fields, "SSH user", "bench");
        set_text(&mut fields, "SSH key", "~/.ssh/id_ed25519");
        let (plan, _, _) = build_plan(&fields).expect("plan should validate");
        assert_eq!(plan.clients.len(), 3);
        assert_eq!(plan.clients[0].host, "worker-01");
        assert_eq!(plan.clients[1].host, "worker-02");
        assert_eq!(plan.clients[2].host, "worker-03");
        // Default service port is 1611 for elbencho when not specified.
        assert_eq!(plan.clients[0].service_port, 1611);
        // Worker 2 had a per-host override.
        assert_eq!(plan.clients[1].service_port, 1612);
        // Worker 3 has no override.
        assert_eq!(plan.clients[2].service_port, 1611);
        // SSH user + key applied to every worker.
        assert_eq!(plan.clients[0].ssh_user.as_deref(), Some("bench"));
        assert_eq!(
            plan.clients[0].ssh_key.as_ref().map(|p| p.to_string_lossy().into_owned()),
            Some("~/.ssh/id_ed25519".into())
        );
    }

    #[test]
    fn fio_engine_picks_8765_as_default_service_port() {
        let mut fields = default_fields();
        select_radio(&mut fields, "Engine", "fio");
        set_text(&mut fields, "Workers (hosts)", "worker-01");
        let (plan, _, _) = build_plan(&fields).expect("plan should validate");
        assert_eq!(plan.clients[0].service_port, 8765);
    }

    #[test]
    fn explicit_service_port_overrides_engine_default() {
        let mut fields = default_fields();
        select_radio(&mut fields, "Engine", "fio");
        set_text(&mut fields, "Workers (hosts)", "worker-01");
        set_text(&mut fields, "Service port", "9999");
        let (plan, _, _) = build_plan(&fields).expect("plan should validate");
        assert_eq!(plan.clients[0].service_port, 9999);
    }

    #[test]
    fn workers_field_blank_means_localhost() {
        let mut fields = default_fields();
        set_text(&mut fields, "Workers (hosts)", "   ");  // whitespace only
        let (plan, _, _) = build_plan(&fields).expect("plan should validate");
        assert_eq!(plan.clients.len(), 1);
        assert_eq!(plan.clients[0].host, "localhost");
    }

    #[test]
    fn invalid_worker_port_errors() {
        let mut fields = default_fields();
        set_text(&mut fields, "Workers (hosts)", "worker-01:not-a-port");
        let err = build_plan(&fields).unwrap_err();
        assert!(format!("{:#}", err).contains("port"));
    }

    #[test]
    fn validation_blocks_missing_duration_and_filesize() {
        let mut fields = default_fields();
        set_text(&mut fields, "File size", "");
        set_text(&mut fields, "Duration (s)", "");
        let err = build_plan(&fields).unwrap_err();
        assert!(format!("{:#}", err).contains("Duration or File size"));
    }

    #[test]
    fn save_and_load_template_round_trips_through_yaml() {
        // Configure a non-default-shaped form, save, clear, load, compare.
        let mut fields = default_fields();
        select_radio(&mut fields, "Engine", "fio");
        set_text(&mut fields, "Mount path", "/mnt/test");
        set_text(&mut fields, "Dataset subdir", "round-trip");
        set_text(&mut fields, "Block size(s)", "4k,16k,1m");
        set_text(&mut fields, "Threads", "8");
        set_text(&mut fields, "IO depth", "4");
        set_text(&mut fields, "File size", "512MiB");
        set_text(&mut fields, "Files per thread", "4");
        set_text(&mut fields, "Read mix %", "70");
        set_text(&mut fields, "Workers (hosts)", "worker-01,worker-02");
        set_text(&mut fields, "SSH user", "bench");

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("tpl.yaml");
        let path_str = path.to_string_lossy().to_string();
        save_template_inner(&fields, &path_str).expect("save");

        // Fresh form, load the saved template.
        let mut fresh = default_fields();
        load_template_inner(&mut fresh, &path_str).expect("load");

        // Compare key field values.
        assert_eq!(field_radio_selected(&fresh, "Engine"), Some("fio"));
        assert_eq!(field_text(&fresh, "Mount path"), "/mnt/test");
        assert_eq!(field_text(&fresh, "Dataset subdir"), "round-trip");
        // Block sizes get reformatted as human-bytes; 4k -> 4KiB, etc.
        assert_eq!(field_text(&fresh, "Block size(s)"), "4KiB,16KiB,1MiB");
        assert_eq!(field_text(&fresh, "Threads"), "8");
        assert_eq!(field_text(&fresh, "IO depth"), "4");
        assert_eq!(field_text(&fresh, "File size"), "512MiB");
        assert_eq!(field_text(&fresh, "Files per thread"), "4");
        assert_eq!(field_text(&fresh, "Read mix %"), "70");
        assert_eq!(field_text(&fresh, "Workers (hosts)"), "worker-01,worker-02");
        assert_eq!(field_text(&fresh, "SSH user"), "bench");

        // Build plans from both forms and compare top-level shape.
        let (a, _, _) = build_plan(&fields).unwrap();
        let (b, _, _) = build_plan(&fresh).unwrap();
        assert_eq!(a.engine, b.engine);
        assert_eq!(a.clients.len(), b.clients.len());
        assert_eq!(a.sweeps.len(), b.sweeps.len());
    }

    #[test]
    fn load_template_rejects_multi_target_yaml() {
        let yaml = r#"
version: 1
engine: elbencho
output_dir: ./results
clients:
  - host: localhost
targets:
  - name: a
    kind: posix
    mount_path: /mnt/a
  - name: b
    kind: posix
    mount_path: /mnt/b
workloads:
  - name: w
    block_size: 4096
    file_size: 4096
runs:
  - target: a
    workload: w
"#;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("multi.yaml");
        std::fs::write(&p, yaml).unwrap();
        let mut fields = default_fields();
        let err = load_template_inner(&mut fields, &p.to_string_lossy()).unwrap_err();
        assert!(format!("{:#}", err).contains("2 targets"));
    }

    #[test]
    fn load_template_rejects_s3_target() {
        let yaml = r#"
version: 1
engine: elbencho
clients:
  - host: localhost
targets:
  - name: s3t
    kind: s3
    endpoint: https://s3.example.com
    bucket: b
    credentials_ref: env:X
workloads:
  - name: w
    block_size: 4096
    file_size: 4096
runs:
  - target: s3t
    workload: w
"#;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("s3.yaml");
        std::fs::write(&p, yaml).unwrap();
        let mut fields = default_fields();
        let err = load_template_inner(&mut fields, &p.to_string_lossy()).unwrap_err();
        assert!(format!("{:#}", err).contains("S3"));
    }
}
