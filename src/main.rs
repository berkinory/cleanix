use anyhow::{Result, bail};
use cleanix::{
    config::Config,
    model::{safe_text, size},
    scan, ui,
};
use std::path::PathBuf;
fn main() {
    if let Err(e) = run() {
        eprintln!("cleanix: {e:#}");
        std::process::exit(1)
    }
}
fn run() -> Result<()> {
    let mut roots = vec![];
    let mut scan_only = false;
    let mut json = false;
    let mut dry = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--internal-delete" => {
                return cleanix::cleanup::elevated_target(
                    &args
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Missing internal request"))?,
                );
            }
            "--root" => roots.push(PathBuf::from(
                args.next()
                    .ok_or_else(|| anyhow::anyhow!("--root needs a path"))?,
            )),
            "--scan" => scan_only = true,
            "--json" => {
                json = true;
                scan_only = true
            }
            "--dry" => dry = true,
            "--version" => {
                println!("cleanix {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "cleanix [--dry] [--root PATH ...] [--scan | --json]\n\nInteractive cleanup permanently deletes selected data after confirmation.\n--dry validates only: no deletion, reset, prune or administrator prompt.\n--scan and --json are always read-only inventories.\n--root PATH overrides the project discovery root; repeat for multiple roots.\n--version shows the version; --help / -h shows this help.\nConfig: $XDG_CONFIG_HOME/cleanix/config.toml or ~/.config/cleanix/config.toml\nDefault discovery root: home. Global tool caches remain in scope.\nKeys: space select, enter expand, / search, d delete, i details, ? help."
                );
                return Ok(());
            }
            _ => bail!("Unknown option {arg}; use --help"),
        }
    }
    let mut config = Config::load(roots)?;
    config.dry = dry;
    if scan_only {
        let report = scan::collect(config);
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?)
        } else {
            for item in &report.items {
                println!(
                    "{:>12}  {:20}  {}",
                    item.bytes.map(size).unwrap_or_else(|| "unknown".into()),
                    item.category,
                    safe_text(&item.name)
                );
            }
            println!(
                "\n{} actions in {:.2}s. Read-only. {} scan notes.",
                report.items.len(),
                report.elapsed_seconds,
                report.notes.len()
            );
            for n in report.notes {
                println!("  {}", safe_text(&n))
            }
        }
    } else {
        ui::run(config)?
    }
    Ok(())
}
