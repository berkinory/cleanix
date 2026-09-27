use crate::{filesystem, model::Target, process};
use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

#[derive(Clone, Debug, Serialize)]
pub struct Application {
    pub name: String,
    pub bundle_id: String,
    pub executable: String,
    pub bundle: Target,
    pub metadata: Target,
    pub blocked: Option<String>,
    pub brew: Option<String>,
    pub running: bool,
    pub system_extensions: bool,
    pub bytes: Option<u64>,
    pub last_used: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    pub apps: Vec<Application>,
    pub notes: Vec<String>,
    pub complete: bool,
    pub home: PathBuf,
    pub roots: Vec<PathBuf>,
}

pub fn valid_id(value: &str) -> bool {
    value.len() <= 255
        && value.split('.').count() >= 3
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        })
}
fn component(value: &str) -> bool {
    !value.is_empty()
        && ![".", ".."].contains(&value)
        && !value.contains(['/', '\\'])
        && !value.chars().any(char::is_control)
}
pub fn read_app(path: &Path) -> Result<Application> {
    let bundle = filesystem::target(path)?;
    let plist = path.join("Contents/Info.plist");
    let metadata = filesystem::target(&plist)?;
    let data = process::json(
        Path::new("/usr/bin/plutil"),
        &["-convert", "json", "-o", "-", plist.to_str().unwrap()],
    )?;
    let id = data["CFBundleIdentifier"].as_str().unwrap_or("");
    let executable = data["CFBundleExecutable"].as_str().unwrap_or("");
    if !valid_id(id) || !component(executable) || data["CFBundlePackageType"] != "APPL" {
        bail!("Missing or invalid application metadata");
    }
    let name = data["CFBundleDisplayName"]
        .as_str()
        .or(data["CFBundleName"].as_str())
        .filter(|s| component(s))
        .unwrap_or_else(|| path.file_stem().unwrap().to_str().unwrap());
    let blocked = if id.to_ascii_lowercase().starts_with("com.apple.") {
        Some("Apple application; protected".into())
    } else if path.join("Contents/Library/LaunchServices").exists() {
        Some("Privileged background helper; use the vendor’s uninstaller".into())
    } else {
        super::helpers::daemon_block(path)
            .unwrap_or_else(|e| Some(format!("Cannot verify embedded services: {e:#}")))
    };
    Ok(Application {
        name: name.into(),
        bundle_id: id.into(),
        executable: executable.into(),
        bundle,
        metadata,
        blocked,
        brew: None,
        running: false,
        system_extensions: path.join("Contents/Library/SystemExtensions").exists(),
        bytes: None,
        last_used: None,
    })
}
pub fn process_paths() -> Result<Vec<PathBuf>> {
    let output = process::query(Path::new("/bin/ps"), &["-A", "-o", "comm="])?;
    Ok(output
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect())
}
pub fn running(app: &Application, paths: &[PathBuf]) -> bool {
    paths.iter().any(|p| {
        p.starts_with(&app.bundle.path)
            || (!p.is_absolute() && p.file_name().is_some_and(|n| n == app.executable.as_str()))
    })
}
fn service_references(home: &Path) -> Result<Vec<(String, Vec<PathBuf>)>> {
    let mut services = vec![];
    for root in [
        home.join("Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchAgents"),
        PathBuf::from("/Library/LaunchDaemons"),
    ] {
        let entries = match fs::read_dir(&root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().is_none_or(|e| e != "plist") {
                continue;
            }
            let data = process::json(
                Path::new("/usr/bin/plutil"),
                &["-convert", "json", "-o", "-", path.to_str().unwrap_or("")],
            )?;
            let paths = data["Program"]
                .as_str()
                .into_iter()
                .chain(
                    data["ProgramArguments"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str()),
                )
                .filter(|s| Path::new(s).is_absolute())
                .map(PathBuf::from)
                .collect();
            services.push((data["Label"].as_str().unwrap_or("").into(), paths));
        }
    }
    Ok(services)
}
pub fn scan(home: &Path, roots: &[PathBuf]) -> Result<Inventory> {
    let processes = process_paths()?;
    let managed = super::managed::ownership();
    let hosts = super::managed::hosting_apps()?;
    let services = service_references(home);
    let mut result = Inventory {
        apps: vec![],
        notes: vec![],
        complete: true,
        home: home.into(),
        roots: roots.into(),
    };
    if let Some(warning) = crate::platform::disk_access_warning(home) {
        result.notes.push(warning);
    }
    if let Err(e) = &managed {
        result.notes.push(format!("Homebrew inventory: {e:#}"));
    }
    if let Err(e) = &services {
        result.notes.push(format!("Service inventory: {e:#}"));
    }
    for root in roots {
        if !root.try_exists()? {
            continue;
        }
        for entry in WalkDir::new(root)
            .max_depth(3)
            .follow_links(false)
            .same_file_system(true)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || e.path()
                        .parent()
                        .is_none_or(|p| p.extension().is_none_or(|x| x != "app"))
            })
        {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    result.complete = false;
                    result.notes.push(e.to_string());
                    continue;
                }
            };
            if entry.path().extension().is_none_or(|s| s != "app") {
                continue;
            }
            let linked = entry.file_type().is_symlink();
            let resolved = if linked {
                entry.path().canonicalize().ok()
            } else {
                None
            };
            if resolved.as_ref().is_some_and(|p| p.starts_with("/System")) {
                continue;
            }
            let mut app = match read_app(resolved.as_deref().unwrap_or(entry.path())) {
                Ok(a) => a,
                Err(e) => {
                    result.complete = false;
                    result
                        .notes
                        .push(format!("Skipped {}: {e}", entry.path().display()));
                    continue;
                }
            };
            if app.bundle_id.to_ascii_lowercase().starts_with("com.apple.") {
                continue;
            }
            if linked {
                app.blocked =
                    Some("Linked application; remove it through its owning installer".into());
            }
            match &managed {
                Ok(owners) => {
                    if let Some((token, single_app)) = owners.get(&app.bundle.path) {
                        app.brew = Some(token.clone());
                        app.blocked = if *single_app {
                            None
                        } else {
                            Some(
                                "Cask owns multiple apps; uninstall the full cask through Homebrew"
                                    .into(),
                            )
                        };
                    }
                }
                Err(_) => app.blocked = Some("Homebrew ownership could not be checked".into()),
            }
            if app.blocked.is_none() && app.brew.is_none() {
                app.blocked = match &services {
                    Err(_) => Some(
                        "Background service inventory unavailable; cannot safely uninstall".into(),
                    ),
                    Ok(services)
                        if services.iter().any(|(id, paths)| {
                            id == &app.bundle_id
                                || id.starts_with(&format!("{}.", app.bundle_id))
                                || paths.iter().any(|p| p.starts_with(&app.bundle.path))
                        }) =>
                    {
                        Some("Registered background service; use the vendor's uninstaller".into())
                    }
                    _ => None,
                };
            }
            app.running = running(&app, &processes);
            if running(&app, &hosts) {
                app.blocked = Some(
                    "Hosts this cleanix session; run from another terminal to uninstall".into(),
                );
            }
            result.apps.push(app);
        }
    }
    result.apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(result)
}
