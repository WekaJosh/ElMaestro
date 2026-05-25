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
        Some(Command::Run { config: _ })
        | Some(Command::Report { results_dir: _ })
        | Some(Command::Compare { run_dirs: _ }) => {
            anyhow::bail!("not yet implemented in the rewrite");
        }
        Some(Command::Tui { config }) => crate::tui::run_tui(config.as_deref()),
    }
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
