//! Command-line dispatch.
//!
//! `elmaestro` with no args opens the TUI; subcommands stay available for
//! scripted use (`elmaestro run config.yaml`, etc).

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "elmaestro",
    version,
    about = "Interactive IO benchmarking harness on elbencho + fio.",
    long_about = "Run with no arguments to open the TUI. \
                  Subcommands stay available for scripted use."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Print the harness version.
    Version,
    /// Load and validate a YAML config without running anything.
    Validate { config: std::path::PathBuf },
    /// Dry-run: print every spec a run would execute, without running.
    Expand { config: std::path::PathBuf },
    /// Execute the runs and sweeps in a config and render a report for each spec.
    Run { config: std::path::PathBuf },
    /// (Re-)render HTML reports from existing result.json files.
    Report { results_dir: std::path::PathBuf },
    /// Overlay N run directories into one comparison HTML report.
    Compare { run_dirs: Vec<std::path::PathBuf> },
    /// Open the interactive TUI for a given config.
    Tui { config: Option<std::path::PathBuf> },
}

/// Library entry point. Returns Result so main can print errors nicely.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => crate::tui::run_home(),
        Some(Command::Version) => {
            println!("elmaestro {}", crate::VERSION);
            Ok(())
        }
        Some(Command::Validate { config }) => {
            let plan = crate::config::loader::load(&config)?;
            print_validate(&config, &plan);
            Ok(())
        }
        Some(Command::Expand { config }) => {
            let plan = crate::config::loader::load(&config)?;
            print_expand(&plan)?;
            Ok(())
        }
        Some(Command::Run { config }) => run_command(&config),
        Some(Command::Report { results_dir }) => report_command(&results_dir),
        Some(Command::Compare { run_dirs }) => compare_command(&run_dirs),
        Some(Command::Tui { config }) => crate::tui::run_tui(config.as_deref()),
    }
}

fn report_command(results_dir: &std::path::Path) -> Result<()> {
    // Two modes: results_dir is a spec dir (has result.json directly) OR
    // a run dir (subdirs each have result.json). Detect and handle both.
    if results_dir.join("result.json").is_file() {
        let result = read_result(&results_dir.join("result.json"))?;
        let out = results_dir.join("report.html");
        crate::report::render_single(&result, &out, None)?;
        println!("rendered {}", out.display());
        return Ok(());
    }
    let mut rendered = 0;
    for entry in std::fs::read_dir(results_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let rj = entry.path().join("result.json");
        if !rj.is_file() {
            continue;
        }
        let result = read_result(&rj)?;
        let out = entry.path().join("report.html");
        crate::report::render_single(&result, &out, None)?;
        println!("  rendered {}", out.display());
        rendered += 1;
    }
    if rendered == 0 {
        anyhow::bail!("no result.json files found under {}", results_dir.display());
    }
    println!("Rendered {} report(s).", rendered);
    Ok(())
}

fn compare_command(run_dirs: &[std::path::PathBuf]) -> Result<()> {
    if run_dirs.len() < 2 {
        anyhow::bail!("compare needs at least two run directories");
    }
    let loaded: Vec<crate::report::LoadedRun> = run_dirs
        .iter()
        .map(|d| crate::report::load_run(d, None))
        .collect::<Result<Vec<_>>>()?;
    let out = std::path::PathBuf::from("compare.html");
    crate::report::render_compare(&loaded, &out, None, "elmaestro compare")?;
    println!("Wrote {}", out.display());
    Ok(())
}

fn read_result(path: &std::path::Path) -> Result<crate::results::schema::Result> {
    let text = std::fs::read_to_string(path)?;
    let r: crate::results::schema::Result = serde_json::from_str(&text)?;
    Ok(r)
}

fn run_command(config_path: &std::path::Path) -> Result<()> {
    let plan = crate::config::loader::load(config_path)?;
    let pairs = crate::config::sweep::materialize_run_refs(&plan)?;
    if pairs.is_empty() {
        anyhow::bail!("config has neither `runs:` nor `sweeps:` to execute");
    }
    let backend = crate::backends::get_backend(plan.engine);
    let base_out = plan.output_dir.clone();
    std::fs::create_dir_all(&base_out)?;

    let first_label = match &pairs[0].0 {
        Some(point) => format!("sweep_{}", point.sweep_name),
        None => format!("{}_{}", pairs[0].1.target_name(), pairs[0].1.workload.name),
    };
    let run_dir = new_run_dir(&base_out, &first_label)?;
    println!("Run directory: {}", run_dir.display());

    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut manifest_entries: Vec<serde_json::Value> = Vec::new();
    let mut manifest_statuses: serde_json::Map<String, serde_json::Value> = Default::default();
    let manifest_run_id = ulid::Ulid::new().to_string();
    let manifest_created = chrono::Utc::now();

    for (idx, (point, spec)) in pairs.iter().enumerate() {
        let sweep_label = point.as_ref().map(|p| p.short_label());
        let spec_dir = spec_dir_path(
            &run_dir,
            idx + 1,
            spec.target_name(),
            &spec.workload.name,
            sweep_label.as_deref(),
        )?;
        let label_extra = sweep_label
            .as_deref()
            .map(|s| format!(" · {}", s))
            .unwrap_or_default();
        println!(
            "→ [{:04}] {} · {}{}  (spec_hash={}…)",
            idx + 1,
            spec.target_name(),
            spec.workload.name,
            label_extra,
            &spec.spec_hash[..18.min(spec.spec_hash.len())]
        );
        let status_label;
        match crate::engine::run_spec(spec, &spec_dir, None, backend.as_ref()) {
            Ok(result) => {
                write_result_json(&spec_dir, &result)?;
                let report_path = spec_dir.join("report.html");
                if let Err(e) = crate::report::render_single(&result, &report_path, None) {
                    eprintln!("  warn: rendering report failed: {:#}", e);
                }
                if result.elbencho_exit_code == 0 {
                    println!("  ✓ result.json + report.html written");
                    completed += 1;
                    status_label = "completed".to_string();
                } else {
                    eprintln!(
                        "  ✗ {} exited {}; partial result written",
                        backend.name(),
                        result.elbencho_exit_code
                    );
                    failed += 1;
                    status_label = format!("failed:{}", result.elbencho_exit_code);
                }
            }
            Err(e) => {
                eprintln!("  ✗ coordinator error: {:#}", e);
                failed += 1;
                status_label = "error".into();
            }
        }

        // Manifest entry. axis_values format matches the Python schema so
        // result.json files between Python and Rust runs are interchangeable.
        let axis_values: Option<serde_json::Value> = point.as_ref().map(|p| {
            let mut map = serde_json::Map::new();
            for (k, v) in &p.overrides {
                map.insert(k.clone(), v.to_json_value());
            }
            serde_json::Value::Object(map)
        });
        manifest_entries.push(serde_json::json!({
            "index": idx + 1,
            "spec_hash": spec.spec_hash,
            "run_id": spec.run_id,
            "target": spec.target_name(),
            "workload": spec.workload.name,
            "sweep": point.as_ref().map(|p| p.sweep_name.clone()),
            "axis_values": axis_values,
            "spec_dir": spec_dir
                .strip_prefix(&run_dir)
                .unwrap_or(&spec_dir)
                .display()
                .to_string(),
        }));
        manifest_statuses.insert(
            spec.spec_hash.clone(),
            serde_json::Value::String(status_label),
        );
    }

    // Write manifest.json (matches python-legacy/.../results/store.py shape so
    // bench compare in either implementation can read it).
    let manifest = serde_json::json!({
        "schema_version": "1.0",
        "run_id": manifest_run_id,
        "created_at": manifest_created.to_rfc3339(),
        "run_specs": manifest_entries,
        "statuses": manifest_statuses,
    });
    std::fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;

    println!(
        "\nDone. {} completed, {} failed. {}",
        completed,
        failed,
        run_dir.display()
    );
    if failed > 0 {
        anyhow::bail!("{} spec(s) failed", failed);
    }
    Ok(())
}

fn new_run_dir(base: &std::path::Path, label: &str) -> Result<std::path::PathBuf> {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let slug = slugify(label);
    let dir = base.join(format!("{}_{}", ts, slug));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn spec_dir_path(
    run_dir: &std::path::Path,
    index: usize,
    target: &str,
    workload: &str,
    label: Option<&str>,
) -> Result<std::path::PathBuf> {
    let mut name = format!("{:04}_{}_{}", index, slugify(target), slugify(workload));
    if let Some(l) = label {
        if !l.is_empty() {
            name.push('_');
            name.push_str(&slugify(l));
        }
    }
    let dir = run_dir.join(name);
    std::fs::create_dir_all(dir.join("raw"))?;
    Ok(dir)
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
        .replace("--", "-")
}

fn write_result_json(spec_dir: &std::path::Path, result: &crate::results::schema::Result) -> Result<()> {
    let json = serde_json::to_string_pretty(result)?;
    std::fs::write(spec_dir.join("result.json"), json)?;
    Ok(())
}

fn print_validate(config_path: &std::path::Path, plan: &crate::config::RunPlan) {
    println!("✓ {} parses cleanly.", config_path.display());
    println!("engine: {}", plan.engine);
    println!(
        "  {} client(s), {} target(s), {} workload(s), {} explicit run(s), {} sweep(s)",
        plan.clients.len(),
        plan.targets.len(),
        plan.workloads.len(),
        plan.runs.len(),
        plan.sweeps.len(),
    );
}

fn print_expand(plan: &crate::config::RunPlan) -> Result<()> {
    let pairs = crate::config::sweep::materialize_run_refs(plan)?;
    if pairs.is_empty() {
        anyhow::bail!("config has neither `runs:` nor `sweeps:`");
    }
    println!(
        "{:>5}  {:<16}  {:<20}  {:<20}  {:<20}  clients",
        "#", "source", "target", "workload", "axes",
    );
    for (idx, (point, spec)) in pairs.iter().enumerate() {
        let source = point
            .as_ref()
            .map(|p| p.sweep_name.as_str())
            .unwrap_or("runs");
        let axes = point.as_ref().map(|p| p.short_label()).unwrap_or_default();
        println!(
            "{:>5}  {:<16}  {:<20}  {:<20}  {:<20}  {}",
            idx + 1,
            source,
            spec.target_name(),
            spec.workload.name,
            axes,
            spec.clients.len(),
        );
    }
    println!("\n{} spec(s) total.", pairs.len());
    Ok(())
}
