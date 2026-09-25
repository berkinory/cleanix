use crate::{filesystem, model::Target};
use anyhow::{Context, Result, bail};
use std::{
    ffi::{CStr, CString},
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::ffi::OsStrExt,
    path::{Component, Path},
};

fn open(parent: i32, name: &CStr) -> Result<OwnedFd> {
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}
fn stat(parent: i32, name: &CStr) -> Result<libc::stat> {
    let mut s = std::mem::MaybeUninit::uninit();
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            s.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { s.assume_init() })
}
fn same(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino && a.st_mode == b.st_mode
}
fn remove_at(parent: i32, name: &CStr, expected: &libc::stat, device: libc::dev_t) -> Result<()> {
    let current = stat(parent, name)?;
    if !same(&current, expected) || current.st_dev != device {
        bail!("Target changed or crossed a filesystem boundary");
    }
    #[cfg(target_os = "macos")]
    if current.st_flags
        & (libc::UF_IMMUTABLE
            | libc::SF_IMMUTABLE
            | 0x0008_0000
            | libc::UF_APPEND
            | libc::SF_APPEND)
        != 0
    {
        bail!("Protected entry");
    }
    let directory = current.st_mode & libc::S_IFMT == libc::S_IFDIR;
    if directory {
        let fd = open(parent, name)?;
        #[cfg(target_os = "linux")]
        crate::platform::check_flags(fd.as_raw_fd())?;
        let mut opened = std::mem::MaybeUninit::uninit();
        if unsafe { libc::fstat(fd.as_raw_fd(), opened.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        if !same(&current, &unsafe { opened.assume_init() }) {
            bail!("Directory changed while opening");
        }
        let duplicate = unsafe { libc::dup(fd.as_raw_fd()) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let stream = unsafe { libc::fdopendir(duplicate) };
        if stream.is_null() {
            unsafe { libc::close(duplicate) };
            return Err(io::Error::last_os_error().into());
        }
        struct Dir(*mut libc::DIR);
        impl Drop for Dir {
            fn drop(&mut self) {
                unsafe { libc::closedir(self.0) };
            }
        }
        let stream = Dir(stream);
        loop {
            unsafe {
                #[cfg(target_os = "macos")]
                {
                    *libc::__error() = 0;
                }
                #[cfg(target_os = "linux")]
                {
                    *libc::__errno_location() = 0;
                }
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(0) {
                    return Err(e.into());
                }
                break;
            }
            let child = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_owned();
            if child.to_bytes() == b"." || child.to_bytes() == b".." {
                continue;
            }
            let info = stat(fd.as_raw_fd(), &child)?;
            remove_at(fd.as_raw_fd(), &child, &info, device)?;
        }
    }
    if !same(&stat(parent, name)?, expected) {
        bail!("Target changed before unlink");
    }
    if unsafe {
        libc::unlinkat(
            parent,
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } != 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}
pub fn remove(target: &Target) -> Result<()> {
    filesystem::safe_cleanup_path(&target.path)?;
    filesystem::validate(target)?;
    let parent = target.path.parent().context("Missing parent")?;
    let mut fd = open(libc::AT_FDCWD, c"/")?;
    for component in parent.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => fd = open(fd.as_raw_fd(), &CString::new(name.as_bytes())?)?,
            _ => bail!("Invalid parent path"),
        }
    }
    let name = CString::new(
        target
            .path
            .file_name()
            .context("Missing filename")?
            .as_bytes(),
    )?;
    let current = stat(fd.as_raw_fd(), &name)?;
    if current.st_ino != target.inode
        || current.st_dev as u64 != target.device
        || current.st_mode & libc::S_IFMT == libc::S_IFLNK
    {
        bail!("Scanned target changed");
    }
    remove_at(fd.as_raw_fd(), &name, &current, current.st_dev)
}
pub fn writable_parent(path: &Path) -> bool {
    path.parent()
        .and_then(|p| CString::new(p.as_os_str().as_bytes()).ok())
        .is_some_and(|p| unsafe { libc::access(p.as_ptr(), libc::W_OK) } == 0)
}
