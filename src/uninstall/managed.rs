use super::inventory::Application;
use crate::process;
use anyhow::{Result, ensure};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub fn ownership() -> Result<HashMap<PathBuf, (String, bool)>> {
    let mut apps = HashMap::new();
    let Some(brew) = process::which("brew") else {
        ensure!(
            !Path::new("/opt/homebrew/Caskroom").exists()
                && !Path::new("/usr/local/Caskroom").exists(),
            "Homebrew exists but brew is unavailable"
        );
        return Ok(apps);
    };
    let data = process::json(&brew, &["info", "--json=v2", "--cask", "--installed"])?;
    let casks = data["casks"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Invalid Homebrew inventory"))?;
    for cask in casks {
        let token = cask["token"].as_str().unwrap_or("");
        ensure!(
            !token.is_empty()
                && !token.starts_with('-')
                && token
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-@+._".contains(&c)),
            "Invalid cask token"
        );
        for artifact in cask["artifacts"].as_array().into_iter().flatten() {
            if artifact.get("app").is_none() {
                continue;
            }
            let target = artifact["target"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Homebrew app target unavailable for {token}"))?;
            let path = PathBuf::from(target);
            ensure!(path.is_absolute(), "Invalid Homebrew app target");
            let path = path.canonicalize().unwrap_or(path);
            ensure!(
                apps.insert(
                    path,
                    (
                        token.to_owned(),
                        cask["artifacts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|a| a.get("app").is_some())
                            .count()
                            == 1
                    )
                )
                .is_none(),
                "Multiple casks own one application"
            );
        }
    }
    Ok(apps)
}

pub fn request_quit(app: &Application) -> Result<()> {
    if !super::inventory::running(app, &super::inventory::process_paths()?) {
        return Ok(());
    }
    let script = "on run argv\nset bundleID to item 1 of argv\nif application id bundleID is running then tell application id bundleID to quit\nend run";
    process::run(
        Path::new("/usr/bin/osascript"),
        &["-e".into(), script.into(), app.bundle_id.clone()],
        Duration::from_secs(60),
    )?;
    Ok(())
}

pub fn wait_for_exit(app: &Application) -> Result<()> {
    let start = Instant::now();
    while super::inventory::running(app, &super::inventory::process_paths()?) {
        ensure!(
            start.elapsed() < Duration::from_secs(10),
            "{} is still running; nothing removed for this app",
            app.name
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

pub fn uninstall(token: &str) -> Result<()> {
    let brew = process::which("brew").ok_or_else(|| anyhow::anyhow!("Homebrew unavailable"))?;
    process::run(
        &brew,
        &["uninstall".into(), "--cask".into(), token.into()],
        Duration::from_secs(180),
    )?;
    Ok(())
}

pub fn hosting_apps() -> Result<Vec<PathBuf>> {
    let output = process::query(Path::new("/bin/ps"), &["-A", "-o", "pid=,ppid=,comm="])?;
    let mut rows = HashMap::new();
    for line in output.lines() {
        let mut parts = line.trim().splitn(2, char::is_whitespace);
        let Some(pid) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let mut rest = parts
            .next()
            .unwrap_or("")
            .trim()
            .splitn(2, char::is_whitespace);
        let Some(parent) = rest.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        rows.insert(
            pid,
            (parent, PathBuf::from(rest.next().unwrap_or("").trim())),
        );
    }
    let mut pid = std::process::id();
    let mut paths = vec![];
    for _ in 0..128 {
        let Some((parent, path)) = rows.get(&pid) else {
            break;
        };
        paths.push(path.clone());
        if *parent == pid || *parent == 0 {
            break;
        }
        pid = *parent;
    }
    Ok(paths)
}
