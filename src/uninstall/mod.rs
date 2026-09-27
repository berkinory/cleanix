mod execution;
mod helpers;
pub mod inventory;
mod managed;
mod metrics;
pub mod plan;
mod ui;
mod view;

use crate::config::Config;
use anyhow::Result;
use std::path::PathBuf;

pub fn roots(home: &std::path::Path) -> Vec<PathBuf> {
    vec![PathBuf::from("/Applications"), home.join("Applications")]
}

pub fn run(config: Config, scan: bool, json: bool) -> Result<()> {
    if scan {
        let inventory = metrics::scan(&config.home, &roots(&config.home))?;
        if json {
            println!("{}", serde_json::to_string_pretty(&inventory)?);
        } else {
            for app in inventory.apps {
                println!(
                    "{}  {}  {}",
                    crate::model::safe_text(&app.name),
                    app.bundle_id,
                    app.blocked.as_deref().unwrap_or("available")
                );
            }
            for note in inventory.notes {
                println!("{}", crate::model::safe_text(&note));
            }
        }
        Ok(())
    } else {
        ui::run(config)
    }
}
