#[cfg(target_os = "macos")]
use crate::filesystem;
use crate::{
    config::Config,
    managed,
    model::{Action, Item, Risk},
    process,
    providers::path_item,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn location(c: &Config, key: &str, default: &str) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| c.home.join(default))
}
fn add(
    items: &mut Vec<Item>,
    notes: &mut Vec<String>,
    name: &str,
    detail: &str,
    risk: Risk,
    paths: Vec<PathBuf>,
) {
    if let Some(i) = path_item("Developer caches", name, detail, risk, paths, notes) {
        items.push(i);
    }
}
pub fn discover(c: &Config) -> (Vec<Item>, Vec<String>) {
    let mut items = vec![];
    let mut notes = vec![];
    let h = &c.home;
    let nvm = location(c, "NVM_DIR", ".nvm");
    let rustup = location(c, "RUSTUP_HOME", ".rustup");
    let pyenv = location(c, "PYENV_ROOT", ".pyenv");
    let rbenv = location(c, "RBENV_ROOT", ".rbenv");
    let sdkman = location(c, "SDKMAN_DIR", ".sdkman");
    for (name, paths) in [
        ("nvm downloads", vec![nvm.join(".cache")]),
        (
            "rustup downloads",
            vec![rustup.join("downloads"), rustup.join("tmp")],
        ),
        ("pyenv downloads", vec![pyenv.join("cache")]),
        ("rbenv downloads", vec![rbenv.join("cache")]),
        (
            "SDKMAN downloads",
            vec![sdkman.join("archives"), sdkman.join("tmp")],
        ),
    ] {
        add(
            &mut items,
            &mut notes,
            name,
            "Downloaded installers and temporary files only. Installed language versions and toolchains are preserved. Stop installers first.",
            Risk::Rebuild,
            paths,
        );
    }
    if let Some(exe) = process::which("conda") {
        match process::json(&exe, &["info", "--json"]) {
            Ok(info) => {
                let paths = info["pkgs_dirs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|p| p.as_str())
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .collect();
                add(
                    &mut items,
                    &mut notes,
                    "Conda configured package stores",
                    "Downloaded package stores only; installed Conda environments are preserved. Hardlinked packages can remain allocated through those environments.",
                    Risk::Review,
                    paths,
                );
            }
            Err(e) => notes.push(format!("Conda package store discovery failed: {e}")),
        }
    }
    let mut nuget = vec![
        location(c, "NUGET_PACKAGES", ".nuget/packages"),
        location(c, "NUGET_HTTP_CACHE_PATH", ".local/share/NuGet/http-cache"),
        h.join("Library/Caches/NuGet"),
    ];
    if let Some(exe) = process::which("dotnet") {
        match process::query(&exe, &["nuget", "locals", "all", "--list"]) {
            Ok(output) => nuget.extend(nuget_locations(&output)),
            Err(e) => notes.push(format!("NuGet configured cache discovery failed: {e}")),
        }
    }
    nuget.sort();
    nuget.dedup();
    add(
        &mut items,
        &mut notes,
        "NuGet packages & cache",
        "Downloaded NuGet packages and HTTP/temp cache. nuget.config and credentials are excluded; restore downloads packages again.",
        Risk::Rebuild,
        nuget,
    );
    let bun = location(c, "BUN_INSTALL_GLOBAL_DIR", ".bun/install/global");
    add(
        &mut items,
        &mut notes,
        "Bun global packages",
        "Installed global packages, not disposable cache. Removal breaks their commands until reinstalled. Review the manifest first; it is preserved.",
        Risk::Review,
        vec![bun.join("node_modules")],
    );
    #[cfg(target_os = "macos")]
    {
        let cache = location(c, "XDG_CACHE_HOME", ".cache");
        if cache != *h && cache != Path::new("/") {
            for path in filesystem::directories(&cache).unwrap_or_default() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if ["uv", "huggingface", "torch"].contains(&name.as_ref()) {
                    continue;
                }
                if name == "codex-runtimes" {
                    for runtime in filesystem::directories(&path).unwrap_or_default() {
                        if runtime.join("runtime.json").is_file() {
                            add(
                                &mut items,
                                &mut notes,
                                "Codex runtime dependencies",
                                "Downloaded Node/Python/native tool dependencies only. Chat history, plugins and runtime configuration are excluded. Quit Codex before eventual deletion; tools need restoration.",
                                Risk::Review,
                                vec![runtime.join("dependencies")],
                            );
                        }
                    }
                    continue;
                }
                if cache_has_user_state(&path) {
                    notes.push(format!(
                        "Cache discovery excluded possible user/session data: {}",
                        path.display()
                    ));
                    continue;
                }
                add(
                    &mut items,
                    &mut notes,
                    &format!("Tool cache · {name}"),
                    "Discovered inside the tool cache root. Contents are not app-specific verified; review the path and stop the owner before permanent removal.",
                    Risk::Review,
                    vec![path],
                );
            }
        }
    }
    (items, notes)
}
pub fn nuget_locations(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|l| l.split_once(':'))
        .filter(|(key, _)| {
            ["http-cache", "global-packages", "temp", "plugins-cache"].contains(&key.trim())
        })
        .map(|(_, p)| {
            let p = PathBuf::from(p.trim());
            if cfg!(target_os = "macos")
                && let Ok(rest) = p.strip_prefix("/var")
            {
                Path::new("/private/var").join(rest)
            } else {
                p
            }
        })
        .filter(|p| p.is_absolute())
        .collect()
}
pub fn cache_has_user_state(path: &Path) -> bool {
    let forbidden = |name: &str| {
        [
            "codex",
            "claude",
            "sessions",
            "session",
            "chats",
            "conversations",
            "history",
            "auth.json",
            "history.jsonl",
            "credentials",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
    };
    if forbidden(&path.file_name().unwrap_or_default().to_string_lossy()) {
        return true;
    }
    fs::read_dir(path).map_or(true, |entries| {
        entries
            .flatten()
            .any(|e| forbidden(&e.file_name().to_string_lossy()))
    })
}
pub fn valid_snapshot_date(date: &str) -> bool {
    date.len() == 17
        && date.bytes().enumerate().all(|(i, c)| {
            if [4, 7, 10].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}
pub fn snapshot_items(output: &str) -> Vec<Item> {
    let mut dates = std::collections::BTreeSet::new();
    for line in output.lines() {
        if let Some(date) = line
            .trim()
            .strip_prefix("com.apple.TimeMachine.")
            .and_then(|s| s.strip_suffix(".local"))
            && valid_snapshot_date(date)
        {
            dates.insert(date);
        }
    }
    dates.into_iter().map(|date|managed::item("System cleanup",format!("Time Machine · {date}"),"Local backup snapshot. Removing it loses this restore point. APFS does not provide an independent reclaimable size; no guessed bytes are added. Requires administrator authentication.".into(),None,Action::TimeMachineSnapshot{date:date.into()})).collect()
}
pub fn snapshots() -> (Vec<Item>, Vec<String>) {
    match process::query(Path::new("/usr/bin/tmutil"), &["listlocalsnapshots", "/"]) {
        Ok(output) => (snapshot_items(&output), vec![]),
        Err(e) => (
            vec![],
            vec![format!("Time Machine inventory unavailable: {e}")],
        ),
    }
}
