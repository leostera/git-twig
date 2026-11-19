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
    Foundation::FILETIME,
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE, FILE_END_OF_FILE_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileEndOfFileInfo, GetFileInformationByHandle, GetFinalPathNameByHandleW,
        GetVolumeInformationByHandleW, MoveFileExW, SetFileInformationByHandle, SetFileTime,
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
    clone_handle(&source, destination)
}

/// Clone an open regular file into a new destination. Holding the source
/// handle means a later rename or replacement of its path cannot redirect the
/// clone.
pub fn clone_handle(source: &File, destination: &Path) -> Result<()> {
    let metadata = source.metadata()?;
    ensure!(
        metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "source is not a regular file"
    );
    // Refuse before creating anything: an unsupported volume must not leave an
    // empty private file behind, and NTFS is the common case.
    ensure!(
        supports_block_cloning(source)?,
        "volume does not support block cloning (ReFS or a Dev Drive is required)"
    );
    let integrity = integrity(source)?;
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
    // The target must already be as long as the source.
    set_length(&destination, length)?;
    duplicate(source, &destination, length, integrity.ClusterSizeInBytes)
}

/// Share the donor's clusters with an existing target of equal length and
/// bytes, in place: the target keeps its identity, name and timestamps.
///
/// The caller holds both files open denying writes and deletion to everyone
/// else, and has compared every byte under those handles. No other writer can
/// change either file between that comparison and this call, and Windows
/// refuses such a handle while another process holds the file open for
/// writing or has it mapped writable.
pub fn dedupe_locked(donor: &File, target: &File, length: u64) -> Result<u64> {
    ensure!(length > 0, "nothing to share");
    ensure!(
        supports_block_cloning(donor)?,
        "volume does not support block cloning (ReFS or a Dev Drive is required)"
    );
    let (a, b) = (donor.metadata()?, target.metadata()?);
    const SETTINGS: u32 = FILE_ATTRIBUTE_SPARSE_FILE;
    ensure!(
        a.file_size() == length && b.file_size() == length,
        "length changed"
    );
    ensure!(
        a.file_attributes() & SETTINGS == b.file_attributes() & SETTINGS,
        "sparse settings differ"
    );
    let (left, right) = (integrity(donor)?, integrity(target)?);
    ensure!(
        left.ChecksumAlgorithm == right.ChecksumAlgorithm && left.Flags == right.Flags,
        "integrity settings differ"
    );
    let written = b.last_write_time();
    // A FILETIME of all ones tells Windows not to update this timestamp for
    // later operations on this handle. Git compares it, and the bytes do not
    // change.
    let keep = FILETIME {
        dwLowDateTime: u32::MAX,
        dwHighDateTime: u32::MAX,
    };
    // SAFETY: the handle is live; null leaves the creation time unchanged.
    if unsafe { SetFileTime(target.as_raw_handle(), null(), &keep, &keep) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    duplicate(donor, target, length, left.ClusterSizeInBytes)?;
    ensure!(
        target.metadata()?.last_write_time() == written,
        "target timestamp changed while sharing"
    );
    Ok(length)
}

fn integrity(file: &File) -> Result<FSCTL_GET_INTEGRITY_INFORMATION_BUFFER> {
    // SAFETY: an all-zero buffer is a valid output buffer for this control code.
    let mut integrity: FSCTL_GET_INTEGRITY_INFORMATION_BUFFER = unsafe { zeroed() };
    control::<(), _>(
        file,
        FSCTL_GET_INTEGRITY_INFORMATION,
        None,
        Some(&mut integrity),
    )?;
    Ok(integrity)
}

/// Regions are cluster aligned; the last one rounds up past the end of both
/// files, as Windows documents for files of equal length.
fn duplicate(source: &File, destination: &File, length: u64, cluster: u32) -> Result<()> {
    let cluster = u64::from(cluster);
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
            destination,
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            Some(&request),
            None,
        )
        .with_context(|| format!("block clone failed after {offset} bytes"))?;
        offset += count;
    }
    Ok(())
}

/// Open a directory to walk from. The handle denies deletion, so the directory
/// cannot be renamed or replaced while it is held.
pub fn directory(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure!(
        identity_of(&file)?.directory,
        "not a real directory: {path:?}"
    );
    Ok(file)
}

/// How `open_relative` opens the final component.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Read, letting others read and write meanwhile.
    Read,
    /// Read, denying everyone else writes and deletion.
    ReadLocked,
    /// Read and write, denying everyone else writes and deletion.
    WriteLocked,
}

/// Open `relative` below `root` one component at a time, refusing any
/// symlink, junction or other reparse point on the way, the counterpart of
/// Unix `openat(O_NOFOLLOW)`. Each directory is held open denying deletion
/// while the next component is opened, so no name on the path can be renamed
/// or replaced meanwhile, and its current name is taken from the handle.
pub fn open_relative(root: &File, relative: &Path, access: Access) -> Result<File> {
    let mut parent = root.try_clone()?;
    let mut components = relative.components().peekable();
    ensure!(components.peek().is_some(), "empty tracked path");
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            bail!("unsafe tracked path: {relative:?}")
        };
        let leaf = components.peek().is_none();
        let path = final_path(&parent)?.join(name);
        let mut options = OpenOptions::new();
        options.read(true);
        let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
        if leaf {
            match access {
                Access::Read => options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE),
                Access::ReadLocked => options.share_mode(FILE_SHARE_READ),
                Access::WriteLocked => options.write(true).share_mode(FILE_SHARE_READ),
            };
        } else {
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
            flags |= FILE_FLAG_BACKUP_SEMANTICS;
        }
        let file = options.custom_flags(flags).open(&path)?;
        let id = identity_of(&file)?;
        let attributes = file.metadata()?.file_attributes();
        ensure!(
            attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "path component is a symlink or junction: {path:?}"
        );
        ensure!(
            leaf || id.directory,
            "path component is not a directory: {path:?}"
        );
        parent = file;
    }
    Ok(parent)
}

/// The current name of an open file, as a verbatim path.
fn final_path(file: &File) -> Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: the handle is live and the buffer holds `len` code units.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        if written == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if written < buffer.len() {
            buffer.truncate(written);
            return Ok(std::ffi::OsString::from_wide(&buffer).into());
        }
        buffer.resize(written + 1, 0);
    }
}

/// Positional reads leave nothing shared between callers. A size check is only
/// an early rejection; every accepted byte is compared, followed by EOF.
pub fn equal(a: &File, b: &File, length: u64) -> Result<bool> {
    use std::os::windows::fs::FileExt;
    fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> Result<()> {
        while !buffer.is_empty() {
            let count = file.seek_read(buffer, offset)?;
            ensure!(count > 0, "file shrank while being compared");
            buffer = &mut buffer[count..];
            offset += count as u64;
        }
        Ok(())
    }
    let mut left = vec![0u8; 65536];
    let mut right = vec![0u8; 65536];
    let mut offset = 0;
    while offset < length {
        let count = (length - offset).min(left.len() as u64) as usize;
        read_exact_at(a, &mut left[..count], offset)?;
        read_exact_at(b, &mut right[..count], offset)?;
        if left[..count] != right[..count] {
            return Ok(false);
        }
        offset += count as u64;
    }
    Ok(a.seek_read(&mut left[..1], length)? == 0 && b.seek_read(&mut right[..1], length)? == 0)
}

/// Identity and link count of an open file.
pub fn identity_of(file: &File) -> Result<Identity> {
    Ok(information(file)?.0)
}

pub fn links(file: &File) -> Result<u32> {
    Ok(information(file)?.1)
}

fn information(file: &File) -> Result<(Identity, u32)> {
    // SAFETY: an all-zero structure is a valid output buffer.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    // SAFETY: the handle is live and `info` is a valid output buffer.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok((
        Identity {
            volume: info.dwVolumeSerialNumber,
            index: u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow),
            directory: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0
                && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        },
        info.nNumberOfLinks,
    ))
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
    Ok(identity(path)?.volume)
}

/// What a path names, without following a final reparse point: the Windows
/// counterpart of a device and inode pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub volume: u32,
    pub index: u64,
    /// A real directory: not a file, and not a symlink or junction to one.
    pub directory: bool,
}

pub fn identity(path: &Path) -> Result<Identity> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    identity_of(&file)
}

/// Rename without ever replacing the destination. Without
/// `MOVEFILE_REPLACE_EXISTING` the filesystem refuses an occupied destination
/// atomically, and without `MOVEFILE_COPY_ALLOWED` it never copies across
/// volumes. Windows also refuses to move a directory holding open files.
pub fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |path: &Path| -> Vec<u16> { path.as_os_str().encode_wide().chain([0]).collect() };
    let (from, to) = (wide(from), wide(to));
    // SAFETY: both paths are live NUL-terminated UTF-16 strings.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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
        assert!(identity(&root)?.directory);
        fs::write(root.join("file"), "")?;
        assert!(!identity(&root.join("file"))?.directory);
        Ok(())
    }

    #[test]
    fn exclusive_renames_refuse_occupied_destinations() -> Result<()> {
        let root = fixture("rename")?;
        for name in ["from", "occupied"] {
            fs::create_dir(root.join(name))?;
        }
        fs::write(root.join("occupied/keep"), "keep")?;
        fs::write(root.join("file"), "file")?;
        for occupied in ["occupied", "file"] {
            let error = rename_exclusive(&root.join("from"), &root.join(occupied)).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists, "{error}");
        }
        assert_eq!(fs::read(root.join("occupied/keep"))?, b"keep");
        let before = identity(&root.join("from"))?;
        rename_exclusive(&root.join("from"), &root.join("vacant"))?;
        assert_eq!(identity(&root.join("vacant"))?, before);
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
