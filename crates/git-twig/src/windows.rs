//! Windows primitives: ReFS block cloning, volume identity and Git-compatible paths.
//!
//! Block cloning (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`) is the Windows counterpart of
//! APFS clonefile and Linux FICLONE. Only ReFS, including Dev Drive, supports it.
//! It shares clusters between two files on one volume or fails: it never copies.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    mem::{size_of, zeroed},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_SPARSE_FILE, FILE_END_OF_FILE_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FileEndOfFileInfo, GetFileInformationByHandle,
        GetVolumeInformationByHandleW, SetFileInformationByHandle,
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            CHECKSUM_TYPE_NONE, DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            FSCTL_GET_INTEGRITY_INFORMATION, FSCTL_GET_INTEGRITY_INFORMATION_BUFFER,
            FSCTL_SET_INTEGRITY_INFORMATION, FSCTL_SET_INTEGRITY_INFORMATION_BUFFER,
            FSCTL_SET_SPARSE,
        },
        SystemServices::FILE_SUPPORTS_BLOCK_REFCOUNTING,
    },
};

/// One duplicate-extents request must stay below 4 GiB.
const MAX_CLONE_BYTES: u64 = (4 << 30) - (1 << 20);

/// Clone a regular file into a new destination, sharing its clusters.
///
/// The source is opened without following a reparse point (the Windows form of
/// a symlink or junction), so a donor path swapped for one is refused. The
/// destination is created exclusively and never replaces an existing entry.
pub fn clone_file(source: &Path, destination: &Path) -> Result<()> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(source)?;
    let metadata = source.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "source is not a regular file"
    );
    // Refuse before creating anything: an unsupported volume must not leave an
    // empty private file behind, and NTFS is the common case.
    ensure!(
        supports_block_cloning(&source)?,
        "volume does not support block cloning (ReFS or a Dev Drive is required)"
    );
    // SAFETY: an all-zero buffer is a valid output buffer for this control code.
    let mut integrity: FSCTL_GET_INTEGRITY_INFORMATION_BUFFER = unsafe { zeroed() };
    control::<(), _>(
        &source,
        FSCTL_GET_INTEGRITY_INFORMATION,
        None,
        Some(&mut integrity),
    )?;
    let destination = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(destination)?;
    // ReFS clones only between files with the same sparse and integrity settings.
    if metadata.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE != 0 {
        control::<(), ()>(&destination, FSCTL_SET_SPARSE, None, None)?;
    }
    if integrity.ChecksumAlgorithm != CHECKSUM_TYPE_NONE as u16 {
        let settings = FSCTL_SET_INTEGRITY_INFORMATION_BUFFER {
            ChecksumAlgorithm: integrity.ChecksumAlgorithm,
            Reserved: integrity.Reserved,
            Flags: integrity.Flags,
        };
        control::<_, ()>(
            &destination,
            FSCTL_SET_INTEGRITY_INFORMATION,
            Some(&settings),
            None,
        )?;
    }
    let length = metadata.file_size();
    if length == 0 {
        return Ok(());
    }
    // The target must already be as long as the source. Regions are cluster
    // aligned; the last one rounds up past the end of both files, as documented.
    set_length(&destination, length)?;
    let cluster = u64::from(integrity.ClusterSizeInBytes);
    ensure!(
        cluster.is_power_of_two(),
        "invalid ReFS cluster size {cluster}"
    );
    let rounded = length.div_ceil(cluster) * cluster;
    let step = MAX_CLONE_BYTES / cluster * cluster;
    let mut offset = 0;
    while offset < rounded {
        let count = (rounded - offset).min(step);
        let request = DUPLICATE_EXTENTS_DATA {
            FileHandle: source.as_raw_handle(),
            SourceFileOffset: i64::try_from(offset)?,
            TargetFileOffset: i64::try_from(offset)?,
            ByteCount: i64::try_from(count)?,
        };
        control::<_, ()>(
            &destination,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            Some(&request),
            None,
        )
        .with_context(|| format!("block clone failed after {offset} bytes"))?;
        offset += count;
    }
    Ok(())
}

/// Refuse early on a volume without block cloning, before staging anything: NTFS
/// is the common Windows case and delegating it should cost almost nothing.
pub fn ensure_block_cloning(directory: &Path) -> Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(directory)?;
    ensure!(
        supports_block_cloning(&directory)?,
        "volume does not support block cloning (ReFS or a Dev Drive is required)"
    );
    Ok(())
}

fn supports_block_cloning(file: &File) -> Result<bool> {
    let mut flags = 0;
    // SAFETY: the handle is live; zero-length name buffers are never written.
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(flags & FILE_SUPPORTS_BLOCK_REFCOUNTING != 0)
}

fn set_length(file: &File, length: u64) -> Result<()> {
    let info = FILE_END_OF_FILE_INFO {
        EndOfFile: i64::try_from(length)?,
    };
    // SAFETY: the handle is live and the buffer is exactly this information class.
    let result = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileEndOfFileInfo,
            (&raw const info).cast(),
            size_of::<FILE_END_OF_FILE_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// Issue one file-system control. Each caller passes the input and output types
/// the control code documents; `None` passes no buffer.
fn control<I, O>(file: &File, code: u32, input: Option<&I>, output: Option<&mut O>) -> Result<()> {
    let (input, input_size) = input.map_or((null(), 0), |input| {
        ((input as *const I).cast::<c_void>(), size_of::<I>() as u32)
    });
    let (output, output_size) = output.map_or((null_mut(), 0), |output| {
        ((output as *mut O).cast::<c_void>(), size_of::<O>() as u32)
    });
    let mut returned = 0;
    // SAFETY: the handle is live; each buffer is valid for its stated size for
    // the duration of this synchronous call; no OVERLAPPED is used.
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            input,
            input_size,
            output,
            output_size,
            &mut returned,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

/// A volume's serial number: equal numbers mean a rename or clone can stay on
/// one volume. Directories are opened too, without following reparse points.
pub fn volume_identity(path: &Path) -> Result<u32> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    // SAFETY: an all-zero structure is a valid output buffer.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    // SAFETY: the handle is live and `info` is a valid output buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(info.dwVolumeSerialNumber)
}

/// `fs::canonicalize` returns verbatim `\\?\C:\...` paths, which Git for Windows
/// cannot use. Resolve the path, then drop the verbatim prefix wherever an
/// ordinary path names the same location.
pub fn canonicalize(path: &Path) -> Result<PathBuf> {
    let resolved = std::fs::canonicalize(path)?;
    let mut components = resolved.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Ok(resolved);
    };
    let rest: PathBuf = components.collect();
    Ok(match prefix.kind() {
        Prefix::VerbatimDisk(letter) => {
            let ordinary = PathBuf::from(format!("{}:\\", char::from(letter))).join(rest);
            // A verbatim path may hold names an ordinary path cannot (trailing
            // dots, reserved names). Keep it rather than name something else.
            if ordinary.as_os_str().len() < 260 && std::fs::canonicalize(&ordinary)? == resolved {
                ordinary
            } else {
                bail!("path {resolved:?} has no ordinary Windows form")
            }
        }
        Prefix::VerbatimUNC(server, share) => {
            let mut ordinary = PathBuf::from(r"\\");
            ordinary.push(server);
            ordinary.push(share);
            ordinary.push(rest);
            ordinary
        }
        _ => resolved,
    })
}

/// Clear the read-only attribute, so an owned private entry can be replaced or
/// removed. Windows has no other owner-settable flag that blocks removal.
pub fn clear_readonly(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_READONLY != 0 {
        let mut permissions = metadata.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// Seconds and nanoseconds since the Unix epoch, from a Windows FILETIME.
pub fn unix_time(filetime: u64) -> (u32, u32) {
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let ticks = filetime.saturating_sub(UNIX_EPOCH_AS_FILETIME);
    // Git keeps the low 32 bits of the seconds, as it does on every platform.
    (
        (ticks / 10_000_000) as u32,
        (ticks % 10_000_000 * 100) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::UNIX_EPOCH};

    fn fixture(name: &str) -> Result<PathBuf> {
        let root = std::env::var_os("TWIG_TEST_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-fixtures")
            })
            .join(format!(
                "windows-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_nanos()
            ));
        fs::create_dir_all(&root)?;
        eprintln!("fixture retained: {}", root.display());
        canonicalize(&root)
    }

    #[test]
    fn block_clones_partial_clusters_and_never_replaces_destinations() -> Result<()> {
        let root = fixture("clone")?;
        let source = root.join("source");
        // Longer than a 64 KiB cluster, and ends inside one.
        let bytes: Vec<u8> = (0..70_001u32).map(|index| (index % 251) as u8).collect();
        fs::write(&source, &bytes)?;
        fs::write(root.join("empty"), b"")?;
        let existing = root.join("existing");
        fs::write(&existing, "keep")?;
        assert!(clone_file(&source, &existing).is_err());
        assert_eq!(fs::read(&existing)?, b"keep");
        assert!(clone_file(&root, &root.join("directory-copy")).is_err());
        assert!(!root.join("directory-copy").exists());
        let copy = root.join("copy");
        match clone_file(&source, &copy) {
            Ok(()) => {
                assert_eq!(fs::read(&copy)?, bytes);
                clone_file(&root.join("empty"), &root.join("empty-copy"))?;
                assert_eq!(fs::read(root.join("empty-copy"))?, b"");
                // A private write never reaches the source.
                fs::write(&copy, "private")?;
                assert_eq!(fs::read(&source)?, bytes);
            }
            Err(error) => {
                assert!(
                    std::env::var_os("TWIG_TEST_REQUIRE_NATIVE").is_none(),
                    "{error:#}"
                );
                assert!(
                    error.to_string().contains("does not support block cloning"),
                    "{error:#}"
                );
                assert!(!copy.exists());
            }
        }
        Ok(())
    }

    #[test]
    fn canonical_paths_are_ordinary_and_volumes_compare() -> Result<()> {
        let root = fixture("paths")?;
        let canonical = canonicalize(&root)?;
        assert!(
            !canonical.to_string_lossy().starts_with(r"\\?\"),
            "{canonical:?}"
        );
        assert_eq!(volume_identity(&canonical)?, volume_identity(&root)?);
        Ok(())
    }

    #[test]
    fn filetimes_convert_like_git_for_windows() {
        assert_eq!(unix_time(116_444_736_000_000_000), (0, 0));
        assert_eq!(
            unix_time(116_444_736_000_000_000 + 12_345_678),
            (1, 234_567_800)
        );
    }
}
