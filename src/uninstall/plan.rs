use super::inventory::{self, Application, Inventory};
use crate::{filesystem, model::Target};
use anyhow::{Result, bail, ensure};
use serde::Serialize;
use std::{collections::HashSet, fs, path::PathBuf, sync::atomic::AtomicBool};

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub target: Target,
    pub bytes: u64,
    pub label: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub app: Application,
    pub entries: Vec<Entry>,
    pub helpers: Vec<super::helpers::Helper>,
    pub notes: Vec<String>,
    pub home: PathBuf,
    pub roots: Vec<PathBuf>,
}
fn exact_paths(app: &Application, inventory: &Inventory) -> Vec<(String, PathBuf)> {
    let library = inventory.home.join("Library");
    let mut paths = vec![];
    for folder in [
        "Application Support",
        "Containers",
        "Caches",
        "Logs",
        "HTTPStorages",
        "WebKit",
    ] {
        paths.push((folder.into(), library.join(folder).join(&app.bundle_id)));
    }
    paths.extend([
        (
            "Preferences".into(),
            library
                .join("Preferences")
                .join(format!("{}.plist", app.bundle_id)),
        ),
        (
            "Saved state".into(),
            library
                .join("Saved Application State")
                .join(format!("{}.savedState", app.bundle_id)),
        ),
        (
            "Cookies".into(),
            library
                .join("Cookies")
                .join(format!("{}.binarycookies", app.bundle_id)),
        ),
    ]);
    let stem = app
        .bundle
        .path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let unique_name = inventory
        .apps
        .iter()
        .filter(|other| {
            other.name.eq_ignore_ascii_case(&app.name)
                || other
                    .bundle
                    .path
                    .file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(&stem))
        })
        .count()
        == 1;
    if unique_name {
        for name in [&app.name, stem.as_ref()] {
            if name.len() < 4
                || !name.chars().any(char::is_alphabetic)
                || name.contains(['/', '\\'])
            {
                continue;
            }
            paths.push((
                "Application data (unique exact name)".into(),
                library.join("Application Support").join(name),
            ));
        }
    }
    paths
}
pub fn build(app: &Application, inventory: &Inventory) -> Result<Plan> {
    if let Some(reason) = &app.blocked {
        bail!("{reason}");
    }
    filesystem::validate(&app.bundle)?;
    filesystem::validate(&app.metadata)?;
    let now = inventory::read_app(&app.bundle.path)?;
    ensure!(
        now.bundle_id == app.bundle_id && now.name == app.name && now.executable == app.executable,
        "Application metadata changed; rescan"
    );
    let mut plan = Plan {
        app: app.clone(),
        entries: vec![],
        helpers: if app.brew.is_none() {
            super::helpers::discover(app)?
        } else {
            vec![]
        },
        notes: vec![],
        home: inventory.home.clone(),
        roots: inventory.roots.clone(),
    };
    let mut paths = vec![("Application".into(), app.bundle.path.clone())];
    let siblings = inventory
        .apps
        .iter()
        .filter(|other| other.bundle_id.eq_ignore_ascii_case(&app.bundle_id))
        .count();
    let indexed = crate::process::query(
        std::path::Path::new("/usr/bin/mdfind"),
        &[&format!("kMDItemCFBundleIdentifier == '{}'", app.bundle_id)],
    );
    let indexed_copy = match indexed {
        Ok(output) => output
            .lines()
            .map(std::path::Path::new)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .any(|p| {
                p.canonicalize()
                    .is_ok_and(|p| p != app.bundle.path && p.exists())
            }),
        Err(_) => {
            plan.notes.push(
                "Spotlight application-copy check unavailable; related data is preserved.".into(),
            );
            true
        }
    };
    ensure!(
        plan.helpers.is_empty() || (inventory.complete && siblings == 1 && !indexed_copy),
        "Login helpers may be shared with another application copy; removal stopped"
    );
    if inventory.complete && siblings == 1 && !indexed_copy {
        paths.extend(exact_paths(app, inventory));
    } else {
        plan.notes.push("Related data is preserved: another copy exists or application discovery was incomplete.".into());
    }
    plan.notes.push("Shared containers, keychains, launch services, package receipts and files outside the listed paths are preserved. Application data can contain documents and settings.".into());
    let mut seen = HashSet::new();
    for (label, path) in paths {
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Err(e) = fs::symlink_metadata(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                plan.notes.push(format!("Skipped {}: {e}", path.display()));
            }
            continue;
        }
        let measured = (|| -> Result<Entry> {
            filesystem::safe_cleanup_path_for(&path, &inventory.home)?;
            let target = filesystem::target(&path)?;
            let bytes =
                filesystem::measure(std::slice::from_ref(&target), &[], &AtomicBool::new(false))?
                    .bytes;
            Ok(Entry {
                target,
                bytes,
                label,
            })
        })();
        match measured {
            Ok(entry) => plan.entries.push(entry),
            Err(e) if path == app.bundle.path => return Err(e),
            Err(e) => plan.notes.push(format!("Skipped {}: {e}", path.display())),
        }
    }
    ensure!(
        plan.entries
            .first()
            .is_some_and(|e| e.target.path == app.bundle.path),
        "Application bundle unavailable"
    );
    Ok(plan)
}

pub use super::execution::execute;
