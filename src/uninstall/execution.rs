use super::{
    inventory,
    plan::{Plan, build},
};
use crate::{
    cleanup, filesystem,
    model::{Action, Item, Risk, Target},
};
use anyhow::{Context, Result, ensure};
use std::{collections::HashSet, sync::atomic::AtomicBool};

pub fn validate(plan: &Plan, selected: &HashSet<usize>) -> Result<Vec<Target>> {
    ensure!(selected.contains(&0), "Application bundle must be selected");
    ensure!(
        selected.iter().all(|&i| i < plan.entries.len()),
        "Invalid selection"
    );
    let live = inventory::scan(&plan.home, &plan.roots)?;
    let app = live
        .apps
        .iter()
        .find(|a| a.bundle.path == plan.app.bundle.path)
        .context("Application disappeared; rescan")?;
    filesystem::validate(&plan.app.bundle)?;
    filesystem::validate(&plan.app.metadata)?;
    ensure!(
        app.bundle_id == plan.app.bundle_id
            && app.name == plan.app.name
            && app.executable == plan.app.executable
            && app.brew == plan.app.brew
            && app.system_extensions == plan.app.system_extensions,
        "Application identity changed; rescan"
    );
    let fresh = build(app, &live)?;
    if app.brew.is_none() {
        super::helpers::validate(app, &plan.helpers)?;
    }
    let mut targets = vec![];
    let mut indices: Vec<_> = selected.iter().copied().collect();
    indices.sort_unstable();
    for i in indices {
        let target = &plan.entries[i].target;
        ensure!(
            fresh.entries.iter().any(|e| e.target.path == target.path),
            "Removal scope changed for {}; rescan",
            target.path.display()
        );
        filesystem::validate(target)?;
        filesystem::measure(std::slice::from_ref(target), &[], &AtomicBool::new(false))?;
        targets.push(target.clone());
    }
    Ok(targets)
}

pub fn execute(plan: &Plan, selected: &HashSet<usize>, dry: bool) -> Result<()> {
    validate(plan, selected)?;
    if dry {
        return Ok(());
    }
    let app = &plan.app;
    super::managed::request_quit(app)?;
    if app.brew.is_none() {
        super::helpers::stop(&plan.helpers)?;
    }
    super::managed::wait_for_exit(app)?;
    let mut targets = validate(plan, selected)?;
    if let Some(token) = &app.brew {
        super::managed::uninstall(token)
            .context("Homebrew uninstall failed; may be partial. Rescan before retrying")?;
        ensure!(
            !app.bundle.path.exists(),
            "Homebrew left the app bundle; data preserved"
        );
        targets.remove(0);
    }
    for target in targets {
        ensure!(
            !inventory::running(app, &inventory::process_paths()?),
            "Application restarted; removal stopped and may be partial"
        );
        let item = Item {
            id: "app-uninstall".into(),
            category: "Uninstall".into(),
            name: app.name.clone(),
            detail: String::new(),
            risk: Risk::Destructive,
            bytes: None,
            estimated: false,
            files: 0,
            action: Action::DeletePaths {
                targets: vec![target.clone()],
            },
            excludes: vec![],
        };
        cleanup::execute(&item, false).with_context(|| {
            format!(
                "Failed at {}; removal may be partial. Rescan before retrying.",
                target.path.display()
            )
        })?;
    }
    Ok(())
}
