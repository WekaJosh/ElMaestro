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
    /// Set when the user activates the Run button. The app drains this to
    /// transition to a RunScreen.
    pub built_plan: Option<(RunPlan, String, usize)>,
    pub cancelled: bool,
}

impl ConfigureScreen {
    pub fn new() -> Self {
        Self {
            fields: default_fields(),
            focused: 0,
            error: None,
            built_plan: None,
            cancelled: false,
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
        frame.render_widget(body, inner_area);

        let footer_text = match &self.error {
            Some(e) => Line::from(Span::styled(
                e.clone(),
                Style::default().fg(Color::Rgb(0xf8, 0x51, 0x49)),
            )),
            None => Line::from(Span::styled(
                "Comma-separate sweep values, e.g. block_size = 64k,256k,1m,4m",
                Style::default().fg(Color::Rgb(0x8b, 0x94, 0x9e)),
            )),
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
            }
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
        Field::Button {
            label: "Run benchmark",
            action: ButtonAction::Run,
        },
        Field::Button {
            label: "Cancel",
            action: ButtonAction::Cancel,
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

    let mut plan = RunPlan {
        version: 1,
        engine,
        output_dir: PathBuf::from("./results"),
        clients: vec![ClientHost::default()],
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
