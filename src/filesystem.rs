// Darwin sys/stat.h: entitlement required for writing.
#[cfg(target_os = "macos")]
const SF_RESTRICTED: u32 = 0x0008_0000;
use crate::model::Target;
use anyhow::{Context, Result, bail};
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt as MacMetadataExt;
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use walkdir::WalkDir;

pub fn target(path: &Path) -> Result<Target> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("Not an absolute normalized path: {}", path.display());
    }
    if path.to_str().is_none() {
        bail!("Non-UTF8 path excluded")
    };
    let m = fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() || path.canonicalize()? != path {
        bail!("Symlink path excluded: {}", path.display());
    }
    #[cfg(target_os = "macos")]
    if m.st_flags()
        & (libc::UF_IMMUTABLE
            | libc::SF_IMMUTABLE
            | SF_RESTRICTED
            | libc::UF_APPEND
            | libc::SF_APPEND)
        != 0
    {
        bail!("Protected path excluded: {}", path.display());
    }
    #[cfg(target_os = "linux")]
    crate::platform::check_path(path, &m)?;
    Ok(Target {
        path: path.to_owned(),
        device: m.dev(),
        inode: m.ino(),
    })
}
pub fn validate(t: &Target) -> Result<()> {
    let now = target(&t.path)?;
    if now.device != t.device || now.inode != t.inode {
        bail!("Path changed since scan: {}", t.path.display());
    }
    Ok(())
}
#[derive(Default, Debug)]
pub struct Measurement {
    pub bytes: u64,
    pub files: u64,
    pub hardlink_duplicates_bytes: u64,
}
pub fn measure(
    paths: &[Target],
    excludes: &[PathBuf],
    cancelled: &AtomicBool,
) -> Result<Measurement> {
    let mut result = Measurement::default();
    let mut seen = HashSet::new();
    for t in paths {
        validate(t)?;
        for entry in WalkDir::new(&t.path)
            .follow_links(false)
            .same_file_system(true)
            .into_iter()
            .filter_entry(|e| !excludes.iter().any(|p| e.path().starts_with(p)))
        {
            if cancelled.load(Ordering::Relaxed) {
                bail!("Scan cancelled");
            }
            let e = entry.with_context(|| format!("Cannot fully read {}", t.path.display()))?;
            let m = fs::symlink_metadata(e.path())?;
            if m.file_type().is_symlink() {
                continue;
            }
            if m.is_file() && m.nlink() > 1 && !seen.insert((m.dev(), m.ino())) {
                result.hardlink_duplicates_bytes += m.blocks() * 512;
                continue;
            }
            #[cfg(target_os = "macos")]
            if m.st_flags()
                & (libc::UF_IMMUTABLE
                    | libc::SF_IMMUTABLE
                    | SF_RESTRICTED
                    | libc::UF_APPEND
                    | libc::SF_APPEND)
                != 0
            {
                bail!("Contains protected entries: {}", t.path.display());
            }
            #[cfg(target_os = "linux")]
            crate::platform::check_path(e.path(), &m)?;
            result.bytes = result.bytes.saturating_add(m.blocks() * 512);
            if m.is_file() {
                result.files += 1;
            }
        }
        validate(t)?;
    }
    Ok(result)
}
pub fn directories(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for e in fs::read_dir(path)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            paths.push(e.path());
        }
    }
    paths.sort();
    Ok(paths)
}
pub fn mounted_free(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let p = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(p.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    (u128::from(stat.f_bavail) * u128::from(stat.f_frsize))
        .try_into()
        .ok()
}

pub fn safe_cleanup_path(path: &Path) -> Result<()> {
    safe_cleanup_path_for(
        path,
        &PathBuf::from(std::env::var_os("HOME").unwrap_or_default()),
    )
}
pub fn safe_cleanup_path_for(path: &Path, home: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().count() < 3
        || [
            "/usr/bin",
            "/usr/sbin",
            "/usr/lib",
            "/usr/lib64",
            "/usr/share",
            "/etc",
            "/boot",
            "/opt",
            "/var/cache",
            "/var/lib",
            "/var/log",
            "/var/tmp",
            "/usr/local",
            "/home",
            "/run/user",
            "/private/var",
            "/private/tmp",
            "/Library/Caches",
            "/Library/Application Support",
            "/Users/Shared",
            "/opt/homebrew",
        ]
        .iter()
        .any(|p| path == Path::new(p))
    {
        bail!("Cleanup target is too broad")
    }
    if !home.as_os_str().is_empty() {
        if [".codex", ".claude"]
            .iter()
            .any(|p| path.starts_with(home.join(p)))
        {
            bail!(
                "Conversation and assistant state is excluded: {}",
                path.display()
            );
        }
        if home.starts_with(path)
            || [
                "Library",
                "Library/Caches",
                "Library/Application Support",
                "Library/Containers",
                "Library/Group Containers",
                "Desktop",
                "Documents",
                "Downloads",
                "codes",
                ".config",
                ".cache",
                ".local",
                ".local/share",
                ".local/state",
                ".Trash",
                ".ssh",
                ".gnupg",
                ".orbstack",
                "Projects",
                "Developer",
                "work",
            ]
            .iter()
            .any(|p| path == home.join(p))
        {
            bail!("Refusing broad data root: {}", path.display())
        }
    }
    for key in [
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        if let Some(root) = std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            && root.starts_with(path)
        {
            bail!("Refusing XDG data root: {}", path.display());
        }
    }
    Ok(())
}

pub fn mounted_capacity(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let p = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(p.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    (u128::from(stat.f_blocks) * u128::from(stat.f_frsize))
        .try_into()
        .ok()
}
