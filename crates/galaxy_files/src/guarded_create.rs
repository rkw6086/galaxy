//! Creates a new file without following changed project-scope paths or replacing existing data.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path};

pub(super) fn write_new_file(path: &Path, scope_root: &Path, content: &[u8]) -> io::Result<()> {
    let components = scoped_components(path, scope_root)?;
    let (name, parents) = components.split_last().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "A new file name is required.")
    })?;
    #[cfg(unix)]
    return unix::write(scope_root, parents, name, content);
    #[cfg(not(unix))]
    return portable::write(scope_root, parents, name, content);
}

pub(super) fn remove_created_file(
    path: &Path,
    scope_root: &Path,
    expected_content: &[u8],
) -> io::Result<()> {
    let components = scoped_components(path, scope_root)?;
    let (name, parents) = components.split_last().ok_or_else(changed_file)?;
    #[cfg(unix)]
    return unix::remove(scope_root, parents, name, expected_content);
    #[cfg(not(unix))]
    return portable::remove(scope_root, parents, name, expected_content);
}

fn scoped_components<'a>(path: &'a Path, scope_root: &Path) -> io::Result<Vec<&'a OsStr>> {
    // The caller captured a canonical scope before permission review. Refuse a root that has
    // subsequently become a symlink to another location, rather than approving that new scope.
    if fs::canonicalize(scope_root)? != scope_root {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The file creation scope changed after this action was prepared.",
        ));
    }
    let relative = path.strip_prefix(scope_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The new file must remain inside the selected scope.",
        )
    })?;
    relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The new file path must not traverse outside its selected scope.",
            )),
        })
        .collect()
}

#[cfg(unix)]
mod unix {
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "File names must not contain NUL.",
            )
        })
    }

    fn open_directory(parent: RawFd, name: &CStr) -> io::Result<OwnedFd> {
        // Each lookup is relative to a directory we already opened. O_NOFOLLOW prevents a
        // symlink swapped in during permission review or concurrent directory creation from
        // redirecting this traversal outside the selected directory tree.
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: openat returned a new descriptor owned by this call.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    fn open_parent(scope_root: &Path, parents: &[&OsStr], create: bool) -> io::Result<OwnedFd> {
        // Walk the canonical scope itself without following symlinks, including its ancestors.
        // A root replaced between canonicalization and open must not select a different tree.
        let mut directory: OwnedFd = fs::File::open("/")?.into();
        for component in scope_root.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    directory = open_directory(directory.as_raw_fd(), &c_name(name)?)?;
                }
                Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "The selected scope must be a canonical absolute directory.",
                    ));
                }
            }
        }
        for name in parents {
            let name = c_name(name)?;
            directory = match open_directory(directory.as_raw_fd(), &name) {
                Ok(child) => child,
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    let created =
                        unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o777) };
                    if created < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                    open_directory(directory.as_raw_fd(), &name)?
                }
                Err(error) => return Err(error),
            };
        }
        Ok(directory)
    }

    pub(super) fn write(
        scope_root: &Path,
        parents: &[&OsStr],
        name: &OsStr,
        content: &[u8],
    ) -> io::Result<()> {
        let directory = open_parent(scope_root, parents, true)?;
        let name = c_name(name)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o666,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a new descriptor owned by this call. O_EXCL ensures an existing
        // file, including a dangling leaf symlink, is never opened for truncation or writing.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        file.write_all(content)
    }

    fn open_leaf(parent: RawFd, name: &CStr) -> io::Result<fs::File> {
        let fd = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            // SAFETY: openat returned a new descriptor owned by this call.
            Ok(unsafe { fs::File::from_raw_fd(fd) })
        }
    }

    pub(super) fn remove(
        scope_root: &Path,
        parents: &[&OsStr],
        name: &OsStr,
        expected_content: &[u8],
    ) -> io::Result<()> {
        let directory = open_parent(scope_root, parents, false)?;
        let name = c_name(name)?;
        let mut file = open_leaf(directory.as_raw_fd(), &name)?;
        let metadata = check_expected_content(&mut file, expected_content)?;
        // Reopen the current leaf without following links and compare its identity/metadata to
        // the file just read. Revert must preserve a replacement or a subsequently edited file.
        let current = open_leaf(directory.as_raw_fd(), &name)?;
        if !same_file(&metadata, &current.metadata()?) {
            return Err(changed_file());
        }
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(not(unix))]
mod portable {
    use std::path::PathBuf;

    use super::*;

    fn check_directory(path: &Path, scope_root: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || redirects(&metadata)
            || fs::canonicalize(path)? != path
            || !path.starts_with(scope_root)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The file creation directory is no longer inside its selected scope.",
            ));
        }
        Ok(())
    }

    fn parent_path(scope_root: &Path, parents: &[&OsStr], create: bool) -> io::Result<PathBuf> {
        check_directory(scope_root, scope_root)?;
        let mut parent = scope_root.to_path_buf();
        for name in parents {
            parent.push(name);
            if create {
                match fs::create_dir(&parent) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
            }
            check_directory(&parent, scope_root)?;
        }
        // Windows does not expose directory-relative open through std. Recheck the parent at the
        // write boundary; create_new still guarantees that an existing leaf is never overwritten.
        check_directory(&parent, scope_root)?;
        Ok(parent)
    }

    pub(super) fn write(
        scope_root: &Path,
        parents: &[&OsStr],
        name: &OsStr,
        content: &[u8],
    ) -> io::Result<()> {
        let parent = parent_path(scope_root, parents, true)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(parent.join(name))?;
        file.write_all(content)
    }

    pub(super) fn remove(
        scope_root: &Path,
        parents: &[&OsStr],
        name: &OsStr,
        expected_content: &[u8],
    ) -> io::Result<()> {
        let parent = parent_path(scope_root, parents, false)?;
        let path = parent.join(name);
        if redirects(&fs::symlink_metadata(&path)?) {
            return Err(changed_file());
        }
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        let mut file = options.open(&path)?;
        let metadata = check_expected_content(&mut file, expected_content)?;
        check_directory(&parent, scope_root)?;
        if !same_file(&metadata, &fs::symlink_metadata(&path)?) {
            return Err(changed_file());
        }
        // Close before removing on Windows, where an open file may not share delete access.
        drop(file);
        fs::remove_file(path)
    }
}

fn changed_file() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "The created file changed; preserve it and review its current contents before deletion.",
    )
}

fn check_expected_content(file: &mut fs::File, expected: &[u8]) -> io::Result<fs::Metadata> {
    let before = file.metadata()?;
    if !before.is_file() || redirects(&before) || before.len() != expected.len() as u64 {
        return Err(changed_file());
    }
    let mut contents = Vec::with_capacity(expected.len());
    Read::by_ref(file)
        .take((expected.len() as u64).saturating_add(1))
        .read_to_end(&mut contents)?;
    let after = file.metadata()?;
    if contents != expected || !same_file(&before, &after) {
        return Err(changed_file());
    }
    Ok(after)
}

fn redirects(metadata: &fs::Metadata) -> bool {
    let redirects = metadata.file_type().is_symlink();
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return redirects || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    redirects
}

fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    if !after.is_file() || redirects(after) || before.len() != after.len() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.mode() == after.mode()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
    }
    #[cfg(not(unix))]
    {
        before.modified().ok() == after.modified().ok()
            && before.created().ok() == after.created().ok()
    }
}

#[cfg(test)]
#[path = "guarded_create_tests.rs"]
mod tests;
