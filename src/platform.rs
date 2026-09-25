use std::path::{Path, PathBuf};

pub fn cache_home(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Caches")
    }
    #[cfg(target_os = "linux")]
    {
        xdg(home, "XDG_CACHE_HOME", ".cache")
    }
}

pub fn xdg(home: &Path, key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(fallback))
}

#[cfg(target_os = "linux")]
pub fn discover(c: &crate::config::Config) -> (Vec<crate::model::Item>, Vec<String>) {
    use crate::{filesystem, model::Risk, providers::path_item, stores};
    let mut items = vec![];
    let mut notes = vec![];
    let cache = cache_home(&c.home);
    let config = xdg(&c.home, "XDG_CONFIG_HOME", ".config");
    let mut paths = vec![];
    match filesystem::directories(&cache) {
        Ok(children) => {
            for path in children {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if [
                    "uv",
                    "huggingface",
                    "torch",
                    "pip",
                    "yarn",
                    "ms-playwright",
                    "go-build",
                    "deno",
                    "Homebrew",
                    "org.swift.swiftpm",
                ]
                .contains(&name.as_ref())
                {
                    continue;
                }
                if name == "codex-runtimes" {
                    for runtime in filesystem::directories(&path).unwrap_or_default() {
                        if runtime.join("runtime.json").is_file()
                            && let Some(item) = path_item(
                                "Developer caches",
                                "Codex runtime dependencies",
                                "Downloaded runtime dependencies only. Quit Codex first; chat history and configuration are preserved.",
                                Risk::Review,
                                vec![runtime.join("dependencies")],
                                &mut notes,
                            )
                        {
                            items.push(item);
                        }
                    }
                } else if stores::cache_has_user_state(&path) {
                    notes.push(format!(
                        "Cache discovery excluded possible user/session data: {}",
                        path.display()
                    ));
                } else {
                    paths.push(path);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => notes.push(format!("Cannot read {}: {e}", cache.display())),
    }
    for app in ["Code", "Cursor", "Slack", "discord", "Helium"] {
        for folder in ["Cache", "Code Cache", "GPUCache", "CachedData"] {
            paths.push(config.join(app).join(folder));
        }
    }
    paths.retain(|p| !c.ignored(p));
    if let Some(item) = path_item(
        "App caches",
        "App caches",
        "Combined XDG application caches and known editor caches. Quit apps first. Offline cache content will be lost; settings and conversation state are excluded.",
        Risk::Review,
        paths,
        &mut notes,
    ) {
        items.push(item);
    }
    (items, notes)
}

#[cfg(target_os = "linux")]
pub fn check_flags(fd: std::os::fd::RawFd) -> anyhow::Result<()> {
    let mut flags: libc::c_long = 0;
    if unsafe { libc::ioctl(fd, libc::FS_IOC_GETFLAGS, &mut flags) } == -1 {
        let e = std::io::Error::last_os_error();
        if matches!(
            e.raw_os_error(),
            Some(libc::ENOTTY) | Some(libc::EOPNOTSUPP)
        ) {
            return Ok(());
        }
        return Err(e.into());
    }
    const FS_IMMUTABLE_FL: libc::c_long = 0x10;
    const FS_APPEND_FL: libc::c_long = 0x20;
    if flags & (FS_IMMUTABLE_FL | FS_APPEND_FL) != 0 {
        anyhow::bail!("Immutable or append-only entry excluded");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn check_path(path: &Path, metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    use std::os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    };
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if !metadata.is_file() && !metadata.is_dir() {
        anyhow::bail!("Special filesystem entry excluded");
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let opened = file.metadata()?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        anyhow::bail!("Entry changed while checking protection");
    }
    check_flags(file.as_raw_fd())
}

pub fn disk_access_warning(_home: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    for relative in [".Trash", "Library/Mail", "Library/Safari"] {
        let path = _home.join(relative);
        if let Err(e) = std::fs::read_dir(&path)
            && (e.kind() == std::io::ErrorKind::PermissionDenied
                || e.raw_os_error() == Some(libc::EPERM))
        {
            return Some(format!(
                "Limited disk access: {} could not be read ({e}). Enable Full Disk Access for the app running cleanix in System Settings > Privacy & Security > Full Disk Access, then restart that app and rescan. Protected paths are excluded from the cleanup total.",
                path.display()
            ));
        }
    }
    None
}
