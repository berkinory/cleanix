use crate::{
    config::Config,
    filesystem,
    model::{Action, Item, Risk},
    process,
};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub fn path_item(
    category: &str,
    name: &str,
    detail: &str,
    risk: Risk,
    paths: Vec<PathBuf>,
    notes: &mut Vec<String>,
) -> Option<Item> {
    let mut targets = vec![];
    for path in paths {
        if let Err(e) = filesystem::safe_cleanup_path(&path) {
            notes.push(format!("Skipped {}: {e}", path.display()));
            continue;
        }
        match filesystem::target(&path) {
            Ok(t) => targets.push(t),
            Err(e) => {
                if path.symlink_metadata().is_ok() || path.try_exists().is_err() {
                    notes.push(format!("Skipped {}: {e}", path.display()));
                }
            }
        }
    }
    if targets.is_empty() {
        return None;
    }
    let id = format!("{category}:{name}:{}", targets[0].path.display());
    Some(Item {
        id,
        category: category.into(),
        name: name.into(),
        detail: detail.into(),
        risk,
        bytes: None,
        estimated: false,
        files: 0,
        action: Action::DeletePaths { targets },
        excludes: vec![],
    })
}
#[cfg(target_os = "macos")]
fn children(path: &Path, notes: &mut Vec<String>) -> Vec<PathBuf> {
    match filesystem::directories(path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => {
            notes.push(format!("{}: {e}", path.display()));
            vec![]
        }
    }
}
pub fn discover(c: &Config, cancel: &AtomicBool) -> (Vec<Item>, Vec<String>) {
    let mut items = vec![];
    let mut notes = vec![];
    let h = &c.home;
    let gradle = env::var_os("GRADLE_USER_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| h.join(".gradle"));
    let bun = env::var_os("BUN_INSTALL_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| h.join(".bun/install/cache"));
    let mut caches: Vec<(&str, PathBuf)> = vec![
        ("Gradle cache", gradle.join("caches")),
        ("Gradle distributions", gradle.join("wrapper/dists")),
        ("CocoaPods", h.join("Library/Caches/CocoaPods")),
        ("Bun", bun),
        ("Yarn", h.join("Library/Caches/Yarn")),
        (
            "Playwright browsers",
            h.join("Library/Caches/ms-playwright"),
        ),
        ("SwiftPM", h.join("Library/Caches/org.swift.swiftpm")),
        ("Homebrew downloads", h.join("Library/Caches/Homebrew")),
        ("pip", h.join("Library/Caches/pip")),
        (
            "uv",
            env::var_os("UV_CACHE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| h.join(".cache/uv")),
        ),
        (
            "Cargo registry downloads",
            env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| h.join(".cargo"))
                .join("registry/cache"),
        ),
        ("Expo", h.join(".expo/cache")),
    ];
    #[cfg(target_os = "linux")]
    {
        caches.retain(|(name, _)| {
            ![
                "CocoaPods",
                "Yarn",
                "Playwright browsers",
                "SwiftPM",
                "Homebrew downloads",
                "pip",
            ]
            .contains(name)
        });
        let cache = crate::platform::cache_home(h);
        caches.extend([
            ("Yarn", cache.join("yarn")),
            ("Playwright browsers", cache.join("ms-playwright")),
            (
                "pip",
                env::var_os("PIP_CACHE_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| cache.join("pip")),
            ),
            ("SwiftPM", cache.join("org.swift.swiftpm")),
            ("Homebrew downloads", cache.join("Homebrew")),
        ]);
        if env::var_os("UV_CACHE_DIR").is_none() {
            for (name, path) in &mut caches {
                if *name == "uv" {
                    *path = cache.join("uv");
                }
            }
        }
    }
    let npm = if let Some(exe) = process::which("npm") {
        match process::query(&exe, &["config", "get", "cache"]) {
            Ok(s) if Path::new(&s).is_absolute() => PathBuf::from(s),
            _ => {
                notes
                    .push("npm cache location unavailable; using documented default ~/.npm".into());
                h.join(".npm")
            }
        }
    } else {
        h.join(".npm")
    };
    caches.extend([("npm", npm.join("_cacache")), ("npx", npm.join("_npx"))]);
    if let Some(exe) = process::which("pnpm") {
        match process::query(&exe, &["store", "path"]) {
            Ok(s) if Path::new(&s).is_absolute() => caches.push(("pnpm store", PathBuf::from(s))),
            _ => notes.push("pnpm did not report its store; skipped".into()),
        }
    } else {
        for p in [
            "Library/pnpm/store",
            ".local/share/pnpm/store",
            ".pnpm-store",
        ] {
            let p = h.join(p);
            if p.exists() {
                caches.push(("pnpm store", p));
                break;
            }
        }
    }
    if let Some(exe) = process::which("go") {
        match process::query(&exe, &["env", "GOCACHE"]) {
            Ok(s) if Path::new(&s).is_absolute() => {
                caches.push(("Go build cache", PathBuf::from(s)))
            }
            _ => notes.push("Go cache location unavailable".into()),
        }
    } else {
        caches.push((
            "Go build cache",
            crate::platform::cache_home(h).join("go-build"),
        ))
    }
    #[cfg(target_os = "linux")]
    if process::which("pnpm").is_none() {
        caches.retain(|(name, _)| *name != "pnpm store");
        caches.push((
            "pnpm store",
            crate::platform::xdg(h, "XDG_DATA_HOME", ".local/share").join("pnpm/store"),
        ));
    }
    for (name, path) in caches {
        if name == "uv" && process::running(&["uv", "uvx"]).unwrap_or(true) {
            notes.push("uv cache excluded while uv tools are running".into());
            continue;
        }
        if let Some(i) = path_item(
            "Package managers",
            name,
            "Downloaded packages or generated cache. Stop package managers first; the next install/build may need network access.",
            Risk::Rebuild,
            vec![path],
            &mut notes,
        ) {
            items.push(i)
        }
    }
    #[cfg(target_os = "macos")]
    {
        for (folder, label, risk, detail) in [
            (
                "DerivedData",
                "DerivedData",
                Risk::Rebuild,
                "Generated Xcode build products and indexes. Close Xcode before cleaning.",
            ),
            (
                "iOS DeviceSupport",
                "iOS symbols",
                Risk::Rebuild,
                "Device support symbols. Xcode may download or copy these again.",
            ),
            (
                "watchOS DeviceSupport",
                "watchOS symbols",
                Risk::Rebuild,
                "Watch device support symbols.",
            ),
            (
                "Archives",
                "Archive",
                Risk::Review,
                "Release archive and dSYMs. Keep builds needed for distribution or crash symbolication.",
            ),
        ] {
            for path in children(&h.join("Library/Developer/Xcode").join(folder), &mut notes) {
                let paths = if folder == "Archives" {
                    children(&path, &mut notes)
                } else {
                    vec![path]
                };
                for path in paths {
                    let name = format!(
                        "{label} · {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    );
                    if let Some(i) = path_item(
                        if folder == "Archives" {
                            "Archives"
                        } else {
                            "Developer caches"
                        },
                        &name,
                        detail,
                        risk,
                        vec![path],
                        &mut notes,
                    ) {
                        items.push(i)
                    }
                }
            }
        }
        if let Some(i) = path_item(
            "Developer caches",
            "Xcode previews",
            "Generated preview simulator data. Close Xcode first.",
            Risk::Rebuild,
            vec![h.join("Library/Developer/Xcode/UserData/Previews/Simulator Devices")],
            &mut notes,
        ) {
            items.push(i)
        }
    }
    for path in &c.build_outputs {
        if c.ignored(path) {
            continue;
        }
        if let Some(i) = path_item(
            "Build outputs",
            &format!(
                "Custom · {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            ),
            "Explicitly configured build output. Review distributable artifacts before permanent deletion.",
            Risk::Review,
            vec![path.clone()],
            &mut notes,
        ) {
            items.push(i)
        }
    }
    let (project_items, project_notes) = discover_projects(c, cancel);
    items.extend(project_items);
    notes.extend(project_notes);
    let (stores, store_notes) = crate::stores::discover(c);
    items.extend(stores);
    notes.extend(store_notes);
    let (extra, extra_notes) = crate::extra::discover(c, cancel);
    items.extend(extra);
    notes.extend(extra_notes);
    #[cfg(target_os = "macos")]
    {
        let owned: Vec<PathBuf> = items.iter().flat_map(|i| i.action.paths()).collect();
        let mut app_paths = crate::extra::app_paths(h);
        for path in children(&h.join("Library/Caches"), &mut notes) {
            if !owned
                .iter()
                .any(|p| path.starts_with(p) || p.starts_with(&path))
                && !c.ignored(&path)
            {
                app_paths.push(path)
            }
        }
        for app in ["Code", "Cursor", "Helium"] {
            for folder in ["Cache", "Code Cache", "GPUCache", "CachedData"] {
                app_paths.push(h.join(format!("Library/Application Support/{app}/{folder}")))
            }
        }
        let helium = h.join("Library/Application Support/net.imput.helium");
        for folder in [
            "GrShaderCache",
            "ShaderCache",
            "GraphiteDawnCache",
            "GPUPersistentCache",
            "component_crx_cache",
        ] {
            app_paths.push(helium.join(folder));
        }
        for profile in children(&helium, &mut notes) {
            let name = profile.file_name().unwrap_or_default().to_string_lossy();
            if name == "Default" || name.starts_with("Profile ") {
                for folder in ["Cache", "Code Cache", "GPUCache"] {
                    app_paths.push(profile.join(folder));
                }
            }
        }
        app_paths.push(h.join("Library/Application Support/Spotify/PersistentCache"));
        app_paths.retain(|p| !c.ignored(p));
        if let Some(i) = path_item(
            "App caches",
            "App caches",
            "Combined application caches and Mail attachment downloads. Quit apps first. Offline content and edits to downloaded attachments may be lost; app settings are excluded.",
            Risk::Review,
            app_paths,
            &mut notes,
        ) {
            items.push(i)
        }
        if let Some(i) = path_item(
            "Logs",
            "User application logs",
            "Diagnostic logs from user applications. Close apps first; old diagnostics will be lost.",
            Risk::Review,
            vec![h.join("Library/Logs")],
            &mut notes,
        ) {
            items.push(i)
        }
    }
    #[cfg(target_os = "linux")]
    {
        let (linux, linux_notes) = crate::platform::discover(c);
        items.extend(linux);
        notes.extend(linux_notes);
    }
    normalize(&mut items, &c.ignore);
    for item in &mut items {
        item.category = crate::model::category(&item.category).into();
    }
    (items, notes)
}

pub fn discover_projects(c: &Config, cancel: &AtomicBool) -> (Vec<Item>, Vec<String>) {
    let mut items = vec![];
    let mut notes = vec![];
    let mut visited = HashSet::new();
    let mut boundaries = 0;
    use std::os::unix::fs::MetadataExt;
    for root in &c.roots {
        if !root.is_absolute() || filesystem::target(root).is_err() {
            notes.push(format!(
                "Unavailable or redirected project root: {}",
                root.display()
            ));
            continue;
        }
        let root_device = match fs::metadata(root) {
            Ok(m) => m.dev(),
            Err(_) => continue,
        };
        let mut pending = vec![(root.clone(), 0usize)];
        while let Some((dir, depth)) = pending.pop() {
            if cancel.load(Ordering::Relaxed) {
                return (items, notes);
            }
            if c.ignored(&dir)
                || !visited.insert(dir.clone())
                || !fs::symlink_metadata(&dir)
                    .is_ok_and(|m| m.dev() == root_device && !m.file_type().is_symlink())
            {
                continue;
            }
            if depth > c.max_depth {
                boundaries += 1;
                continue;
            }
            if dir.join("pyvenv.cfg").is_file() {
                let active = env::var_os("VIRTUAL_ENV")
                    .map(PathBuf::from)
                    .is_some_and(|p| p == dir)
                    || env::var_os("CONDA_PREFIX")
                        .map(PathBuf::from)
                        .is_some_and(|p| p == dir);
                if !active
                    && let Some(i) = path_item(
                        "Projects",
                        &format!(
                            "Python environment · {}",
                            dir.strip_prefix(root).unwrap_or(&dir).display()
                        ),
                        "Installed Python virtual environment, not just cache. Review dependencies and local edits first; recreate using the project's lockfile. Currently activated environments are excluded.",
                        Risk::Review,
                        vec![dir.clone()],
                        &mut notes,
                    )
                {
                    items.push(i);
                }
                continue;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    notes.push(format!("{}: {e}", dir.display()));
                    continue;
                }
            };
            let mut dirs = vec![];
            let mut names = HashSet::new();
            for e in entries {
                match e {
                    Ok(e) => {
                        names.insert(e.file_name().to_string_lossy().into_owned());
                        if e.file_type().is_ok_and(|t| t.is_dir()) {
                            dirs.push(e.path())
                        }
                    }
                    Err(e) => notes.push(format!("{}: {e}", dir.display())),
                }
            }
            let unity = dir.join("ProjectSettings/ProjectVersion.txt").is_file()
                && names.contains("Assets");
            let gradle = names.contains("build.gradle") || names.contains("build.gradle.kts");
            let node = names.contains("package.json");
            let mut found: Vec<(&str, &str, Risk, &str)> = vec![];
            if !node && names.contains("node_modules") {
                found.push(("Dependencies","node_modules",Risk::Review,"Dependency directory without a package.json alongside it. Review carefully: reinstall instructions may be missing."));
            }
            if names.contains("__pycache__") {
                found.push((
                    "Developer caches",
                    "__pycache__",
                    Risk::Rebuild,
                    "Generated Python bytecode. Rebuilt by Python; source files are kept.",
                ));
            }
            if node {
                found.push(("Dependencies","node_modules",Risk::Review,"Installed packages. Local patches can be lost; lockfiles and registry access are needed to reinstall."));
                for f in [
                    ".next",
                    ".nuxt",
                    ".svelte-kit",
                    ".astro",
                    ".angular",
                    ".turbo",
                    ".parcel-cache",
                ] {
                    found.push((
                        "Build outputs",
                        f,
                        Risk::Rebuild,
                        "Generated web build output. Stop the dev server before cleaning.",
                    ))
                }
                found.push((
                    "Developer caches",
                    ".expo",
                    Risk::Review,
                    "Local Expo state. Stop Expo first.",
                ));
            }
            if names.contains("Podfile") {
                found.push(("Dependencies","Pods",Risk::Review,"Installed CocoaPods dependencies. Local changes can be lost; pod install restores dependencies."))
            }
            if unity {
                for f in ["Library", "Temp", "obj"] {
                    found.push(("Developer caches",f,Risk::Rebuild,"Generated Unity imports, artifacts or intermediate files. Close Unity first; regeneration can take time."))
                }
                found.push((
                    "Build outputs",
                    "Builds",
                    Risk::Review,
                    "Unity exports. May contain releases worth keeping.",
                ));
            }
            if gradle {
                found.push((
                    "Build outputs",
                    "build",
                    Risk::Rebuild,
                    "Generated Gradle build output. Stop builds and daemons first.",
                ));
                found.push((
                    "Developer caches",
                    ".gradle",
                    Risk::Rebuild,
                    "Project Gradle cache. Stop Gradle daemons first.",
                ));
            }
            if names.contains("Package.swift") {
                found.push((
                    "Build outputs",
                    ".build",
                    Risk::Review,
                    "SwiftPM output and dependency checkouts. Local checkout changes can be lost.",
                ))
            }
            if names
                .iter()
                .any(|s| s.ends_with(".xcodeproj") || s.ends_with(".xcworkspace"))
            {
                found.push((
                    "Build outputs",
                    "build",
                    Risk::Review,
                    "Local Xcode output. Review exported artifacts before cleaning.",
                ))
            }
            let project = dir
                .strip_prefix(root)
                .ok()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| dir.file_name().map(Path::new).unwrap_or(&dir))
                .display()
                .to_string();
            for (cat, folder, risk, detail) in found {
                let path = dir.join(folder);
                if c.ignored(&path) {
                    continue;
                }
                if let Some(i) = path_item(
                    cat,
                    &format!("{project} / {folder}"),
                    detail,
                    risk,
                    vec![path],
                    &mut notes,
                ) {
                    items.push(i)
                }
            }
            for path in dirs {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if (name.starts_with('.') && !path.join("pyvenv.cfg").is_file())
                    || [
                        "node_modules",
                        "Pods",
                        "__pycache__",
                        "build",
                        "dist",
                        "Temp",
                        "obj",
                        "target",
                    ]
                    .contains(&name.as_ref())
                    || name.ends_with(".app")
                    || name.ends_with(".xcarchive")
                    || name.ends_with(".photoslibrary")
                    || name.ends_with(".fcpbundle")
                    || (dir == c.home && name == "Library")
                    || (unity && name == "Library")
                {
                    continue;
                }
                if unity
                    && [
                        "Assets",
                        "Packages",
                        "ProjectSettings",
                        "Builds",
                        "UserSettings",
                        "Logs",
                    ]
                    .contains(&name.as_ref())
                {
                    continue;
                }
                if gradle && name == "src" {
                    continue;
                }
                pending.push((path, depth + 1));
            }
        }
    }
    if boundaries > 0 {
        notes.push(format!(
            "{boundaries} project branches exceeded depth {}; add a deeper root to cover them",
            c.max_depth
        ))
    }
    (items, notes)
}

pub fn normalize(items: &mut Vec<Item>, ignored: &[PathBuf]) {
    let mut paths: Vec<_> = items
        .iter()
        .enumerate()
        .flat_map(|(index, item)| {
            item.action
                .paths()
                .into_iter()
                .map(move |path| (index, path))
        })
        .collect();
    paths.sort_by_key(|(index, path)| (path.components().count(), *index));
    let mut claimed = HashSet::<PathBuf>::new();
    let mut keep = HashSet::new();
    for (index, path) in paths {
        if ignored
            .iter()
            .any(|p| path.starts_with(p) || p.starts_with(&path))
            || path.ancestors().any(|p| claimed.contains(p))
        {
            continue;
        }
        claimed.insert(path.clone());
        keep.insert((index, path));
    }
    for (index, item) in items.iter_mut().enumerate() {
        if let Action::AgedFiles { targets, .. }
        | Action::DeletePaths { targets }
        | Action::UnifiedLogs { targets }
        | Action::InspectStorage { targets, .. } = &mut item.action
        {
            targets.retain(|t| keep.remove(&(index, t.path.clone())));
        }
    }
    items.retain(|i|!matches!(&i.action,Action::AgedFiles{targets,..} | Action::DeletePaths{targets} | Action::UnifiedLogs{targets} | Action::InspectStorage{targets,..} if targets.is_empty()));
}
