use crate::{
    config::Config,
    model::{Item, Risk},
    process,
    providers::path_item,
};
#[cfg(target_os = "macos")]
use std::fs;
use std::{
    env,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime},
};
use walkdir::WalkDir;

#[cfg(target_os = "macos")]
fn entries(root: &Path, notes: &mut Vec<String>) -> Vec<PathBuf> {
    match fs::read_dir(root) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(e) => {
            notes.push(format!("Skipped {}: {e}", root.display()));
            vec![]
        }
    }
}
fn add(
    items: &mut Vec<Item>,
    notes: &mut Vec<String>,
    category: &str,
    name: &str,
    detail: &str,
    risk: Risk,
    paths: Vec<PathBuf>,
) {
    if let Some(item) = path_item(category, name, detail, risk, paths, notes) {
        items.push(item);
    }
}
fn located(home: &Path, key: &str, fallback: &str) -> PathBuf {
    env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(fallback))
}

pub fn aged_files(
    root: &Path,
    now: SystemTime,
    cancel: &AtomicBool,
    notes: &mut Vec<String>,
) -> Vec<PathBuf> {
    let mut files = vec![];
    let mut skipped = 0usize;
    if !root.exists() {
        return files;
    }
    for entry in WalkDir::new(root)
        .follow_links(false)
        .same_file_system(true)
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        match entry {
            Ok(e) if e.file_type().is_file() => {
                if let Ok(m) = e.metadata() {
                    let old = |time: std::io::Result<SystemTime>| {
                        time.ok()
                            .and_then(|t| now.duration_since(t).ok())
                            .is_some_and(|age| age > Duration::from_secs(3 * 86400))
                    };
                    if old(m.modified()) && old(m.accessed()) {
                        files.push(e.into_path());
                    }
                }
            }
            Err(_) => skipped += 1,
            _ => {}
        }
    }
    if skipped > 0 {
        notes.push(format!(
            "{skipped} inaccessible temporary branches excluded under {}",
            root.display()
        ));
    }
    files
}

pub fn discover(c: &Config, _cancel: &AtomicBool) -> (Vec<Item>, Vec<String>) {
    let mut items = vec![];
    let mut notes = vec![];
    let h = &c.home;
    for (name, paths) in [
        (
            "Maven & Ivy",
            vec![h.join(".m2/repository"), h.join(".ivy2/cache")],
        ),
        ("CocoaPods specs", vec![h.join(".cocoapods/repos")]),
        (
            "Android cache",
            vec![located(h, "ANDROID_USER_HOME", ".android").join("cache")],
        ),
        (
            "Conda packages",
            vec![
                h.join(".conda/pkgs"),
                h.join("miniconda3/pkgs"),
                h.join("anaconda3/pkgs"),
                h.join("miniforge3/pkgs"),
            ],
        ),
        (
            "Deno cache",
            vec![
                env::var_os("DENO_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| crate::platform::cache_home(h).join("deno")),
                h.join(".deno/deps"),
                h.join(".deno/gen"),
            ],
        ),
        (
            "Cargo registry",
            vec![located(h, "CARGO_HOME", ".cargo").join("registry")],
        ),
    ] {
        if cfg!(target_os = "linux") && name == "CocoaPods specs" {
            continue;
        }
        add(
            &mut items,
            &mut notes,
            "Developer caches",
            name,
            "Downloaded packages and indexes. Reinstallation may need network access. Local modifications will be lost.",
            Risk::Review,
            paths,
        );
    }
    let go = process::which("go")
        .and_then(|p| process::query(&p, &["env", "GOMODCACHE"]).ok())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| located(h, "GOPATH", "go").join("pkg/mod"));
    add(
        &mut items,
        &mut notes,
        "Developer caches",
        "Go modules",
        "Downloaded module sources; local changes will be lost.",
        Risk::Review,
        vec![go],
    );
    #[cfg(target_os = "macos")]
    {
        for (relative, name) in [
            ("tvOS DeviceSupport", "tvOS symbols"),
            ("visionOS DeviceSupport", "visionOS symbols"),
            ("iOS Device Logs", "iOS device logs"),
        ] {
            add(
                &mut items,
                &mut notes,
                "Developer caches",
                name,
                "Xcode device support or diagnostics. Close Xcode first.",
                Risk::Rebuild,
                vec![h.join("Library/Developer/Xcode").join(relative)],
            );
        }
        add(
            &mut items,
            &mut notes,
            "Simulators",
            "Shared dyld cache",
            "User-level simulator shared cache; regenerated on demand. Stop simulators first.",
            Risk::Rebuild,
            vec![h.join("Library/Developer/CoreSimulator/Caches/dyld")],
        );

        let system_caches = entries(Path::new("/Library/Caches"), &mut notes);
        add(
            &mut items,
            &mut notes,
            "System cleanup",
            "System caches",
            "Accessible third-party system cache entries. Some require administrator privileges for permanent deletion.",
            Risk::Review,
            system_caches,
        );
        add(
            &mut items,
            &mut notes,
            "System cleanup",
            "System crash reports",
            "System diagnostic reports. Administrator privileges may be required.",
            Risk::Review,
            vec![PathBuf::from("/Library/Logs/DiagnosticReports")],
        );
        if let Some(mut item) = path_item(
            "System cleanup",
            "Unified log store",
            "Erase the macOS unified log store through log erase, never raw database removal. Requires administrator authentication.",
            Risk::Review,
            vec![
                PathBuf::from("/private/var/db/diagnostics"),
                PathBuf::from("/private/var/db/uuidtext"),
            ],
            &mut notes,
        ) {
            if let crate::model::Action::DeletePaths { targets } = item.action {
                item.action = crate::model::Action::UnifiedLogs { targets };
            }
            items.push(item);
        }
        let old_logs = aged_files(
            Path::new("/private/var/log/asl"),
            SystemTime::now(),
            _cancel,
            &mut notes,
        );
        add(
            &mut items,
            &mut notes,
            "System cleanup",
            "Old ASL logs",
            "Only regular logs with access and modification times older than three days.",
            Risk::Review,
            old_logs,
        );
        let mut temp = vec![];
        for key in ["DARWIN_USER_TEMP_DIR", "DARWIN_USER_CACHE_DIR"] {
            if let Ok(dir) = process::query(Path::new("/usr/bin/getconf"), &[key])
                && let Ok(root) = Path::new(&dir).canonicalize()
                && root.starts_with("/private/var/folders")
            {
                temp.extend(aged_files(&root, SystemTime::now(), _cancel, &mut notes));
            }
        }
        add(
            &mut items,
            &mut notes,
            "System cleanup",
            "Temporary files",
            "Current user's macOS temp/cache files only; recently accessed or modified files and non-regular entries are excluded.",
            Risk::Review,
            temp,
        );
        let trash = entries(&h.join(".Trash"), &mut notes);
        add(
            &mut items,
            &mut notes,
            "System cleanup",
            "Empty Trash",
            "Permanently delete existing Trash contents. No recovery through Finder.",
            Risk::Destructive,
            trash,
        );
    }
    for (name, path) in [
        (
            "Ollama models",
            located(h, "OLLAMA_MODELS", ".ollama/models"),
        ),
        (
            "Hugging Face downloads",
            env::var_os("HF_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    crate::platform::xdg(h, "XDG_CACHE_HOME", ".cache").join("huggingface")
                })
                .join("hub"),
        ),
        (
            "PyTorch models",
            env::var_os("TORCH_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    crate::platform::xdg(h, "XDG_CACHE_HOME", ".cache").join("torch")
                }),
        ),
        ("LM Studio models", h.join(".lmstudio/models")),
        (
            "Chrome on-device model",
            h.join("Library/Application Support/Google/Chrome/OptGuideOnDeviceModel"),
        ),
        (
            "Claude sandbox bundles",
            h.join("Library/Application Support/Claude/vm_bundles"),
        ),
    ] {
        if cfg!(target_os = "linux")
            && ["Chrome on-device model", "Claude sandbox bundles"].contains(&name)
        {
            continue;
        }
        add(
            &mut items,
            &mut notes,
            "Models & virtual machines",
            name,
            "Downloaded models or sandbox images. Local custom assets may be lost; downloading again can be expensive. Stop the owning application first.",
            Risk::Review,
            vec![path],
        );
    }
    let hf = env::var_os("HF_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::platform::xdg(h, "XDG_CACHE_HOME", ".cache").join("huggingface"));
    add(
        &mut items,
        &mut notes,
        "Models & virtual machines",
        "Hugging Face datasets & transfer cache",
        "Downloaded datasets and transfer cache; credentials and settings are preserved.",
        Risk::Review,
        vec![hf.join("datasets"), hf.join("xet"), hf.join("assets")],
    );
    #[cfg(target_os = "macos")]
    {
        for (root, suffix, product, marker) in [
            ("Parallels", "pvm", "Parallels", ""),
            ("Documents/Parallels", "pvm", "Parallels", ""),
            (
                "Library/Containers/com.utmapp.UTM/Data/Documents",
                "utm",
                "UTM",
                "",
            ),
            ("Virtual Machines.localized", "vmwarevm", "VMware", ""),
            (
                "Documents/Virtual Machines.localized",
                "vmwarevm",
                "VMware",
                "",
            ),
            ("VirtualBox VMs", "", "VirtualBox", ""),
            (".tart/vms", "", "Tart", "config.json"),
            (".lima", "", "Lima", "lima.yaml"),
            (".colima/_lima", "", "Colima", "lima.yaml"),
        ] {
            for path in entries(&h.join(root), &mut notes) {
                if !path.is_dir()
                    || path
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
                {
                    continue;
                }
                if !suffix.is_empty() && path.extension().is_none_or(|x| x != suffix) {
                    continue;
                }
                if !marker.is_empty() && !path.join(marker).is_file() {
                    continue;
                }
                let name = format!(
                    "{product} · {}",
                    path.file_stem().unwrap_or_default().to_string_lossy()
                );
                add(
                    &mut items,
                    &mut notes,
                    "Models & virtual machines",
                    &name,
                    "Entire virtual machine, including its disk and personal data. Shut down and unregister in its manager before permanent removal.",
                    Risk::Destructive,
                    vec![path],
                );
            }
        }
        if let Some(mut i) = path_item(
            "Models & virtual machines",
            "Docker Desktop backing storage",
            "Docker Desktop VM disk, including live images, containers and volumes. Its total size is not all disposable cache.",
            Risk::Review,
            vec![h.join("Library/Containers/com.docker.docker/Data/vms")],
            &mut notes,
        ) {
            if let crate::model::Action::DeletePaths { targets } = i.action {
                i.action=crate::model::Action::InspectStorage{targets,guidance:"Start Docker Desktop and rescan to discover unused images, containers, volumes and build cache.".into(),allocated_bytes:0};
            }
            items.push(i);
        }
        for path in entries(
            &h.join("Library/Application Support/MobileSync/Backup"),
            &mut notes,
        ) {
            let name = format!(
                "iPhone/iPad backup · {}",
                path.file_name().unwrap_or_default().to_string_lossy()
            );
            add(
                &mut items,
                &mut notes,
                "Archives & backups",
                &name,
                "Device backup, potentially your only restore point. Permanent removal loses it.",
                Risk::Destructive,
                vec![path],
            );
        }
        for root in [
            "iPhone Software Updates",
            "iPad Software Updates",
            "iPod Software Updates",
        ] {
            let paths = entries(&h.join("Library/iTunes").join(root), &mut notes)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|x| x == "ipsw"))
                .collect();
            add(
                &mut items,
                &mut notes,
                "Archives & backups",
                root,
                "Downloaded Apple restore firmware. Can be downloaded again if still available.",
                Risk::Review,
                paths,
            );
        }
        let selected = process::query(Path::new("/usr/bin/xcode-select"), &["-p"])
            .ok()
            .and_then(|p| Path::new(&p).canonicalize().ok());
        let xcode_running = process::running(&["Xcode"]).unwrap_or(true);
        for root in [PathBuf::from("/Applications"), h.join("Applications")] {
            for path in entries(&root, &mut notes) {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if path.extension().is_none_or(|x| x != "app") {
                    continue;
                }
                if name.starts_with("Install macOS") && path.join("Contents/SharedSupport").is_dir()
                {
                    add(
                        &mut items,
                        &mut notes,
                        "Archives & backups",
                        &name,
                        "Downloaded macOS installer application. Does not remove the installed operating system.",
                        Risk::Review,
                        vec![path],
                    );
                } else if name.starts_with("Xcode")
                    && path.join("Contents/Developer/usr/bin/xcodebuild").is_file()
                    && selected.as_ref().is_some_and(|s| !s.starts_with(&path))
                    && !xcode_running
                {
                    add(
                        &mut items,
                        &mut notes,
                        "Archives & backups",
                        &name,
                        "Nonselected Xcode installation. Other projects or DEVELOPER_DIR may still rely on it. All Xcode processes must be closed.",
                        Risk::Review,
                        vec![path],
                    );
                }
            }
        }
        for root in [h.join("Movies"), h.join("Documents")] {
            for library in entries(&root, &mut notes)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|x| x == "fcpbundle"))
            {
                let paths = entries(&library, &mut notes)
                    .into_iter()
                    .map(|p| p.join("Render Files"))
                    .collect();
                let name = format!(
                    "Final Cut renders · {}",
                    library.file_stem().unwrap_or_default().to_string_lossy()
                );
                add(
                    &mut items,
                    &mut notes,
                    "Projects",
                    &name,
                    "Generated render files only; original media stays. Close Final Cut first.",
                    Risk::Rebuild,
                    paths,
                );
            }
        }
    }
    for i in &mut items {
        if ["Temporary files", "Old ASL logs"].contains(&i.name.as_str())
            && let crate::model::Action::DeletePaths { targets } = &i.action
        {
            i.action = crate::model::Action::AgedFiles {
                targets: targets.clone(),
                seconds: 3 * 86400,
            };
        }
    }
    (items, notes)
}

pub fn app_paths(home: &Path) -> Vec<PathBuf> {
    [
        "Library/Application Support/Slack/Cache",
        "Library/Application Support/Slack/Service Worker/CacheStorage",
        "Library/Application Support/discord/Cache",
        "Library/Containers/com.microsoft.teams2/Data/Library/Caches",
        "Library/Containers/com.apple.Safari/Data/Library/Caches",
        "Library/Containers/com.apple.photolibraryd/Data/Library/Caches",
        "Library/Containers/com.apple.mail/Data/Library/Mail Downloads",
        "Library/Mail Downloads",
        "Library/Application Support/Adobe/Common/Media Cache Files",
        "Library/Application Support/Adobe/Common/Media Cache",
        "Library/Application Support/Adobe/Adobe Desktop Common/HDBox/Download",
    ]
    .into_iter()
    .map(|p| home.join(p))
    .collect()
}
