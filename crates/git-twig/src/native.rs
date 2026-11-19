use anyhow::Result;
#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
use anyhow::bail;
use std::{ffi::OsString, path::Path};

#[cfg(target_os = "macos")]
pub fn clone_path(source: &Path, destination: &Path) -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // Darwin sys/clonefile.h; libc exposes clonefile but not these flag constants.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOFOLLOW_ANY: u32 = 0x0008;
    // SAFETY: paths are live NUL-terminated strings; flags reject symlink traversal.
    let result = unsafe {
        libc::clonefile(
            source.as_ptr(),
            destination.as_ptr(),
            CLONE_NOFOLLOW | CLONE_NOFOLLOW_ANY,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn clone_path(source: &Path, destination: &Path) -> Result<()> {
    use std::{
        fs::OpenOptions,
        os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    };
    // Never follow a leaf symlink or overwrite an existing destination.
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)?;
    anyhow::ensure!(source.metadata()?.is_file(), "source is not a regular file");
    let destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    // SAFETY: both descriptors are live; FICLONE takes a source fd, not a pointer.
    let result = unsafe { libc::ioctl(destination.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn clone_path(source: &Path, destination: &Path) -> Result<()> {
    crate::windows::clone_file(source, destination)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
pub fn clone_path(_: &Path, _: &Path) -> Result<()> {
    bail!("native cloning requires macOS, Linux or Windows")
}

/// A canonical path Git accepts. On Windows, not the verbatim `\\?\` form.
pub fn canonicalize(path: &Path) -> Result<std::path::PathBuf> {
    #[cfg(windows)]
    return crate::windows::canonicalize(path);
    #[cfg(not(windows))]
    Ok(std::fs::canonicalize(path)?)
}

/// clonefile(2) copies BSD flags and extended attributes along with the bytes.
/// A Git checkout has neither, and immutable flags would also make the private
/// clone impossible to chmod, replace or remove.
#[cfg(target_os = "macos")]
pub fn discard_donor_metadata(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    clear_flags(path, metadata)?;
    let c_path = c_path(path)?;
    let mut names = vec![0u8; 4096];
    let length = loop {
        // SAFETY: the buffer is live and its exact capacity is passed alongside.
        let length = unsafe {
            libc::listxattr(
                c_path.as_ptr(),
                names.as_mut_ptr().cast(),
                names.len(),
                libc::XATTR_NOFOLLOW,
            )
        };
        if length >= 0 {
            break length as usize;
        }
        let error = std::io::Error::last_os_error();
        anyhow::ensure!(error.raw_os_error() == Some(libc::ERANGE), error);
        names.resize(names.len() * 4, 0);
    };
    for name in names[..length].split_inclusive(|byte| *byte == 0) {
        // The system stamps provenance on files this process creates as well, and
        // owns it: stock checkouts carry it too.
        if name == b"com.apple.provenance\0" {
            continue;
        }
        let name = std::ffi::CStr::from_bytes_with_nul(name)?;
        // SAFETY: both strings are live and NUL-terminated; never follow a symlink.
        let result =
            unsafe { libc::removexattr(c_path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn discard_donor_metadata(_: &Path, _: &std::fs::Metadata) -> Result<()> {
    // Linux reflinks create new inodes: only the bytes come from the donor.
    Ok(())
}

/// Clear the flags an owner may set (immutable, append-only, hidden, nodump,
/// opaque). System flags and filesystem-managed ones are left for the kernel.
#[cfg(target_os = "macos")]
pub fn clear_user_flags(path: &Path) -> Result<()> {
    clear_flags(path, &std::fs::symlink_metadata(path)?)
}

#[cfg(target_os = "macos")]
fn clear_flags(path: &Path, metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::macos::fs::MetadataExt;
    const DONOR_FLAGS: u32 =
        libc::UF_NODUMP | libc::UF_IMMUTABLE | libc::UF_APPEND | libc::UF_OPAQUE | libc::UF_HIDDEN;
    let flags = metadata.st_flags();
    if flags & DONOR_FLAGS == 0 {
        return Ok(());
    }
    let c_path = c_path(path)?;
    // SAFETY: the path is a live NUL-terminated string; lchflags never follows links.
    if unsafe { libc::lchflags(c_path.as_ptr(), flags & !DONOR_FLAGS) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(windows)]
pub fn clear_user_flags(path: &Path) -> Result<()> {
    crate::windows::clear_readonly(path)
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn clear_user_flags(_: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    Ok(std::ffi::CString::new(path.as_os_str().as_bytes())?)
}

/// Discover effective creation permissions without changing the process-global umask.
pub struct FileModes {
    regular: std::fs::Permissions,
    executable: std::fs::Permissions,
}

impl FileModes {
    pub fn new(stage: &Path) -> Result<Self> {
        Ok(Self {
            regular: creation_permissions(&stage.join("regular-mode"), 0o666)?,
            executable: creation_permissions(&stage.join("executable-mode"), 0o777)?,
        })
    }

    /// Apply checkout permissions, unless the clone already has them: most do,
    /// and every skipped chmod is a system call saved per file.
    pub fn ensure(&self, path: &Path, current: &std::fs::Metadata, executable: bool) -> Result<()> {
        let wanted = if executable {
            &self.executable
        } else {
            &self.regular
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if current.permissions().mode() & 0o7777 == wanted.mode() & 0o7777 {
                return Ok(());
            }
        }
        // Windows keeps only a read-only attribute, and new clones never have it.
        #[cfg(not(unix))]
        if current.permissions().readonly() == wanted.readonly() {
            return Ok(());
        }
        std::fs::set_permissions(path, wanted.clone())?;
        Ok(())
    }

    #[cfg(any(target_os = "linux", windows))]
    pub fn apply(&self, path: &Path, executable: bool) -> Result<()> {
        let permissions = if executable {
            &self.executable
        } else {
            &self.regular
        };
        std::fs::set_permissions(path, permissions.clone())?;
        Ok(())
    }
}

fn creation_permissions(path: &Path, mode: u32) -> Result<std::fs::Permissions> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(options.open(path)?.metadata()?.permissions())
}

pub fn symlink(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (source, destination);
        anyhow::bail!("symlinks require Unix")
    }
}

pub fn path_bytes(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

/// A path Git printed. Git for Windows prints `C:/a/b`; show it as `C:\\a\\b`,
/// which names the same file, so messages and Git's own output agree.
pub fn git_path(bytes: Vec<u8>) -> std::path::PathBuf {
    #[cfg(windows)]
    let bytes: Vec<u8> = bytes
        .into_iter()
        .map(|byte| if byte == b'/' { b'\\' } else { byte })
        .collect();
    std::path::PathBuf::from(os_string(bytes))
}

pub fn os_string(bytes: Vec<u8>) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes)
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn reflink_rejects_links_special_files_and_existing_destinations() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-fixtures")
            .join(format!(
                "native-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
        fs::create_dir_all(&root)?;
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, "source bytes")?;
        fs::write(&destination, "keep destination")?;
        assert!(clone_path(&source, &destination).is_err());
        assert_eq!(fs::read(&destination)?, b"keep destination");
        symlink(&source, root.join("link"))?;
        assert!(clone_path(&root.join("link"), &root.join("link-copy")).is_err());
        assert!(!root.join("link-copy").exists());
        assert!(clone_path(&root, &root.join("directory-copy")).is_err());
        assert!(!root.join("directory-copy").exists());
        // A tracked regular file replaced by a FIFO must fail without blocking.
        let fifo = std::ffi::CString::new(path_bytes(&root.join("fifo")))?;
        // SAFETY: fifo is a live NUL-terminated path; mode is a valid permission mask.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(clone_path(&root.join("fifo"), &root.join("fifo-copy")).is_err());
        assert!(!root.join("fifo-copy").exists());
        assert_eq!(fs::read(&source)?, b"source bytes");
        eprintln!("fixture retained: {}", root.display());
        Ok(())
    }
}
