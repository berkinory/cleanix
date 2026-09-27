use super::{inventory, managed};
use crate::{filesystem, model::Target, process};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{fs, path::Path};

#[derive(Clone, Debug, Serialize)]
pub struct Helper {
    pub bundle_id: String,
    pub parent_bundle_id: String,
    pub bundle: Target,
    pub metadata: Target,
}

pub fn discover(app: &inventory::Application) -> Result<Vec<Helper>> {
    let root = app.bundle.path.join("Contents/Library/LoginItems");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut helpers = vec![];
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "app") {
            continue;
        }
        let helper = inventory::read_app(&path)?;
        ensure!(
            !helper
                .bundle_id
                .to_ascii_lowercase()
                .starts_with("com.apple."),
            "Protected login helper identifier"
        );
        helpers.push(Helper {
            bundle_id: helper.bundle_id,
            parent_bundle_id: app.bundle_id.clone(),
            bundle: helper.bundle,
            metadata: helper.metadata,
        });
    }
    helpers.sort_by(|a, b| a.bundle.path.cmp(&b.bundle.path));
    Ok(helpers)
}

fn loaded(label: &str) -> Result<bool> {
    let output = process::query(Path::new("/bin/launchctl"), &["list"])?;
    Ok(output
        .lines()
        .any(|line| line.split_whitespace().nth(2) == Some(label)))
}

fn service(helper: &Helper) -> Result<Option<String>> {
    if !loaded(&helper.bundle_id)? {
        return Ok(None);
    }
    let name = format!("gui/{}/{}", unsafe { libc::getuid() }, helper.bundle_id);
    let output = process::query(Path::new("/bin/launchctl"), &["print", &name])?;
    let field = |key: &str| {
        output
            .lines()
            .find_map(|line| line.trim().strip_prefix(key))
    };
    let program = field("program = ");
    let path_matches =
        program.is_some_and(|p| Path::new(p.trim_matches('"')).starts_with(&helper.bundle.path));
    let managed_matches = program.is_none()
        && field("managed_by = ") == Some("com.apple.xpc.ServiceManagement")
        && field("parent bundle identifier = ") == Some(helper.parent_bundle_id.as_str())
        && field("program identifier = ").and_then(|s| s.split_whitespace().next())
            == Some(helper.bundle_id.as_str());
    ensure!(
        path_matches || managed_matches,
        "Login helper service ownership changed: {}",
        helper.bundle_id
    );
    Ok(Some(name))
}

pub fn validate(app: &inventory::Application, helpers: &[Helper]) -> Result<()> {
    let fresh = discover(app)?;
    ensure!(
        fresh.len() == helpers.len(),
        "Login helpers changed; rescan"
    );
    for (old, new) in helpers.iter().zip(&fresh) {
        ensure!(
            old.bundle_id == new.bundle_id && old.bundle.path == new.bundle.path,
            "Login helper identity changed; rescan"
        );
        filesystem::validate(&old.bundle)?;
        filesystem::validate(&old.metadata)?;
        service(old)?;
    }
    Ok(())
}

pub fn stop(helpers: &[Helper]) -> Result<()> {
    for helper in helpers {
        filesystem::validate(&helper.bundle)?;
        filesystem::validate(&helper.metadata)?;
        let app = inventory::read_app(&helper.bundle.path)?;
        ensure!(
            app.bundle_id == helper.bundle_id,
            "Login helper identity changed"
        );
        if let Some(name) = service(helper)? {
            process::query(Path::new("/bin/launchctl"), &["bootout", &name])?;
            ensure!(
                !loaded(&helper.bundle_id)?,
                "Login helper is still registered; removal stopped"
            );
        } else {
            managed::request_quit(&app)?;
        }
    }
    Ok(())
}

pub fn daemon_block(bundle: &Path) -> Result<Option<String>> {
    let root = bundle.join("Contents/Library/LaunchDaemons");
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut labels = vec![];
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "plist") {
            continue;
        }
        filesystem::target(&path)?;
        let data = process::json(
            Path::new("/usr/bin/plutil"),
            &["-convert", "json", "-o", "-", path.to_str().unwrap_or("")],
        )?;
        let label = data["Label"].as_str().unwrap_or("");
        ensure!(
            inventory::valid_id(label) && !label.to_ascii_lowercase().starts_with("com.apple."),
            "Invalid embedded daemon identity"
        );
        labels.push(label.to_owned());
    }
    if labels.is_empty() {
        return Ok(None);
    }
    for domain in [
        "system".to_owned(),
        format!("gui/{}", unsafe { libc::getuid() }),
    ] {
        let output = process::query(Path::new("/bin/launchctl"), &["print", &domain])?;
        if output
            .split_whitespace()
            .any(|word| labels.iter().any(|label| label == word))
        {
            return Ok(Some(
                "Privileged daemon is registered; stop it through the app’s uninstaller first"
                    .into(),
            ));
        }
    }
    Ok(None)
}
