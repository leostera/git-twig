use super::*;

#[cfg(target_os = "linux")]
#[path = "adopt_linux.rs"]
mod linux;
#[cfg(windows)]
#[path = "adopt_windows.rs"]
mod windows;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

/// Everything adoption must preserve about a file. Windows has no mode, owner
/// or group bits; its file attributes take their place.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    bytes: Vec<u8>,
    mode: u32,
    inode: u64,
    device: u64,
    links: u64,
    uid: u32,
    gid: u32,
    mtime: (i64, i64),
}

#[cfg(unix)]
fn snapshot(_: &Path, stat: &fs::Metadata, bytes: Vec<u8>) -> Result<Snapshot> {
    Ok(Snapshot {
        bytes,
        mode: stat.mode(),
        inode: stat.ino(),
        device: stat.dev(),
        links: stat.nlink(),
        uid: stat.uid(),
        gid: stat.gid(),
        mtime: (stat.mtime(), stat.mtime_nsec()),
    })
}
#[cfg(windows)]
fn snapshot(path: &Path, stat: &fs::Metadata, bytes: Vec<u8>) -> Result<Snapshot> {
    use std::os::windows::fs::MetadataExt;
    let (volume, index, links) = windows_identity(path)?;
    let written = stat.last_write_time();
    Ok(Snapshot {
        bytes,
        mode: stat.file_attributes(),
        inode: index,
        device: volume.into(),
        links: links.into(),
        uid: 0,
        gid: 0,
        mtime: ((written / 10_000_000) as i64, (written % 10_000_000) as i64),
    })
}

pub(super) fn files(root: &Path) -> Result<BTreeMap<PathBuf, Snapshot>> {
    fn visit(
        root: &Path,
        directory: &Path,
        result: &mut BTreeMap<PathBuf, Snapshot>,
    ) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.file_name() == Some(std::ffi::OsStr::new(".git")) {
                continue;
            }
            let stat = fs::symlink_metadata(&path)?;
            if stat.is_dir() {
                visit(root, &path, result)?;
                continue;
            }
            let bytes = if stat.is_symlink() {
                fs::read_link(&path)?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                fs::read(&path)?
            };
            result.insert(
                path.strip_prefix(root)?.to_path_buf(),
                snapshot(&path, &stat, bytes)?,
            );
        }
        Ok(())
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result)?;
    Ok(result)
}
pub(super) fn index(path: &Path) -> Result<Vec<u8>> {
    let output = git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    Ok(fs::read(String::from_utf8(output.stdout)?.trim_end())?)
}
pub(super) fn stages(f: &Fixture) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(f.source.join(".git"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("twig-adopt-prototype-")
        })
        .map(|entry| entry.path())
        .collect())
}

// Windows names cannot hold control characters.
#[cfg(unix)]
const UNUSUAL: &str = "雪\tline\n.bin";
#[cfg(windows)]
const UNUSUAL: &str = "雪 line.bin";

#[test]
fn inspection_and_preparation_preserve_existing_dirty_staged_worktrees() -> Result<()> {
    for format in ["sha1", "sha256"] {
        let f = Fixture::with_object_format(format)?;
        if !cow_available(&f)? {
            continue;
        }
        fs::create_dir(f.source.join("sub"))?;
        for name in [
            "dirty.bin",
            "staged.txt",
            "different",
            "missing",
            "hardlink",
            "sub/file",
            UNUSUAL,
        ] {
            fs::write(f.source.join(name), b"committed\n")?;
        }
        fs::write(
            f.source.join("large.bin"),
            (0..131072).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
        )?;
        #[cfg(unix)]
        symlink("hello.txt", f.source.join("link"))?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "adoption input"])?;
        git(
            &f.source,
            &["worktree", "add", "--detach", "../feature", "HEAD"],
        )?;
        fs::write(f.destination().join("staged.txt"), b"staged change\n")?;
        git(&f.destination(), &["add", "staged.txt"])?;
        for root in [&f.source, &f.destination()] {
            fs::write(root.join("staged.txt"), b"current dirty bytes\n")?;
            fs::write(root.join("dirty.bin"), b"same dirty binary\x00\xff\n")?;
        }
        #[cfg(unix)]
        fs::set_permissions(
            f.destination().join("large.bin"),
            fs::Permissions::from_mode(0o751),
        )?;
        #[cfg(windows)]
        {
            let mut permissions = fs::metadata(f.destination().join("large.bin"))?.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(f.destination().join("large.bin"), permissions)?;
        }
        fs::write(f.destination().join("different"), b"different!\n")?;
        fs::rename(
            f.destination().join("missing"),
            f.root.join("retained-missing"),
        )?;
        fs::hard_link(
            f.destination().join("hardlink"),
            f.root.join("retained-hardlink"),
        )?;
        fs::rename(f.destination().join("sub"), f.root.join("outside"))?;
        #[cfg(unix)]
        symlink(f.root.join("outside"), f.destination().join("sub"))?;
        // A junction is the Windows directory link that needs no privilege.
        #[cfg(windows)]
        checked(
            command("cmd", &f.destination())
                .args(["/C", "mklink", "/J", "sub"])
                .arg(f.root.join("outside"))
                .output()?,
        )?;
        fs::write(f.destination().join("untracked"), b"do not adopt")?;
        #[cfg(target_os = "macos")]
        {
            checked(
                command("xattr", &f.destination())
                    .args(["-w", "com.twig.adopt.fixture", "preserve me", "hello.txt"])
                    .output()?,
            )?;
            checked(
                command("chmod", &f.destination())
                    .args(["+a", "everyone deny delete", "hello.txt"])
                    .output()?,
            )?;
        }
        let before_source = files(&f.source)?;
        let before_target = files(&f.destination())?;
        let indices = (index(&f.source)?, index(&f.destination())?);
        let registrations = git(&f.source, &["worktree", "list", "--porcelain"])?.stdout;
        for mode in ["--dry-run", "--prepare"] {
            let output = checked(f.twig(&["adopt", mode, "../feature"])?)?;
            let stdout = String::from_utf8(output.stdout)?;
            assert!(stdout.contains("match \"dirty.bin\""), "{stdout}");
            assert!(stdout.contains("match \"staged.txt\""), "{stdout}");
            assert!(stdout.contains("skip \"different\""), "{stdout}");
            assert!(stdout.contains("skip \"sub/file\""), "{stdout}");
            // The file name as the report quotes it: fixture paths may contain
            // the word, as in a checkout named fix-macos-untracked-trees.
            assert!(!stdout.contains("\"untracked\""), "{stdout}");
            assert_eq!(files(&f.source)?, before_source);
            assert_eq!(files(&f.destination())?, before_target);
            #[cfg(target_os = "macos")]
            {
                let value = checked(
                    command("xattr", &f.destination())
                        .args(["-p", "com.twig.adopt.fixture", "hello.txt"])
                        .output()?,
                )?;
                assert_eq!(value.stdout, b"preserve me\n");
                let acl = checked(
                    command("ls", &f.destination())
                        .args(["-le", "hello.txt"])
                        .output()?,
                )?;
                assert!(String::from_utf8_lossy(&acl.stdout).contains("everyone deny delete"));
            }
            assert_eq!((index(&f.source)?, index(&f.destination())?), indices);
            assert_eq!(
                git(&f.source, &["worktree", "list", "--porcelain"])?.stdout,
                registrations
            );
            if mode == "--dry-run" {
                assert!(stages(&f)?.is_empty());
            }
        }
        let stages = stages(&f)?;
        assert_eq!(stages.len(), 1);
        #[cfg(unix)]
        assert_eq!(fs::metadata(&stages[0])?.mode() & 0o777, 0o700);
        let mut count = 0;
        for entry in fs::read_dir(&stages[0])? {
            let entry = entry?;
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("candidate-")
            {
                continue;
            }
            count += 1;
            let candidate = snapshot(&entry.path(), &entry.metadata()?, Vec::new())?;
            assert_eq!(candidate.links, 1);
            assert!(
                !before_source
                    .values()
                    .chain(before_target.values())
                    .any(|original| original.inode == candidate.inode
                        && original.device == candidate.device)
            );
            fs::write(entry.path(), b"private candidate write")?;
        }
        assert!(count >= 4);
        assert_eq!(files(&f.source)?, before_source);
        assert_eq!(files(&f.destination())?, before_target);
    }
    Ok(())
}

#[test]
fn adoption_rejects_bad_targets_and_unsupported_modes_without_scratch() -> Result<()> {
    let f = Fixture::new()?;
    let foreign = Fixture::new()?;
    for args in [
        vec!["adopt", "--force", "../feature"],
        vec!["adopt", "--prepare", "--dry-run", "../feature"],
        vec!["adopt", "."],
        vec!["adopt", "--prepare", foreign.source.to_str().unwrap()],
    ] {
        assert!(!f.twig(&args)?.status.success());
        assert!(stages(&f)?.is_empty());
    }
    #[cfg(target_os = "macos")]
    {
        let output = f.twig(&["adopt", "--dedupe", "."])?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("APFS live file replacement is intentionally unsupported")
        );
    }
    Ok(())
}

#[test]
fn adoption_does_not_run_checkout_filters_or_index_hooks() -> Result<()> {
    let f = Fixture::new()?;
    if !cow_available(&f)? {
        return Ok(());
    }
    fs::write(f.source.join(".gitattributes"), "*.txt filter=sentinel\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "filter policy"])?;
    git(
        &f.source,
        &["worktree", "add", "--detach", "../feature", "HEAD"],
    )?;
    for filter in ["filter.sentinel.clean", "filter.sentinel.smudge"] {
        git(
            &f.source,
            &["config", filter, "printf invoked > unexpected-filter; cat"],
        )?;
    }
    let hooks = f.root.join("hooks");
    fs::create_dir(&hooks)?;
    let script = "#!/bin/sh\nprintf invoked > \"$0.marker\"\nexit 1\n";
    for name in ["post-index-change", "fsmonitor"] {
        fs::write(hooks.join(name), script)?;
        #[cfg(unix)]
        fs::set_permissions(hooks.join(name), fs::Permissions::from_mode(0o755))?;
    }
    git(
        &f.source,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    )?;
    git(
        &f.source,
        &[
            "config",
            "core.fsmonitor",
            hooks.join("fsmonitor").to_str().unwrap(),
        ],
    )?;
    let before = (index(&f.source)?, index(&f.destination())?);
    for mode in ["--dry-run", "--prepare"] {
        checked(f.twig(&["adopt", mode, "../feature"])?)?;
    }
    assert_eq!((index(&f.source)?, index(&f.destination())?), before);
    for root in [&f.source, &f.destination()] {
        assert!(!root.join("unexpected-filter").exists());
    }
    assert!(!hooks.join("post-index-change.marker").exists());
    assert!(!hooks.join("fsmonitor.marker").exists());
    Ok(())
}

// Windows refuses to rename a file open without delete sharing.
#[cfg(unix)]
#[test]
fn open_writer_demonstrates_why_path_replacement_is_not_live_adoption() -> Result<()> {
    use std::io::Write;
    let f = Fixture::new()?;
    let path = f.root.join("old-inode");
    fs::write(&path, b"original\n")?;
    let mut writer = fs::OpenOptions::new().write(true).open(&path)?;
    // Retain both versions: this models replacement without discarding either.
    let retained = f.root.join("retained-inode");
    fs::rename(&path, &retained)?;
    fs::write(&path, b"original\n")?;
    writer.write_all(b"new edit\n")?;
    writer.flush()?;
    assert_eq!(fs::read(&path)?, b"original\n");
    assert_eq!(fs::read(&retained)?, b"new edit\n");
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires independently provisioned XFS/Btrfs; run with --include-ignored"]
fn in_place_linux_adoption_preserves_open_inode_and_private_writes() -> Result<()> {
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::fs::FileExt},
    };
    let f = Fixture::new()?;
    if !linux_reflinks_available(&f)? {
        return Ok(());
    }
    fs::write(f.source.join("payload"), vec![b'x'; 17 * 1024 * 1024 + 37])?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "payload"])?;
    git(
        &f.source,
        &["worktree", "add", "--detach", "../feature", "HEAD"],
    )?;
    let before_source = files(&f.source)?;
    let before_target = files(&f.destination())?;
    let indices = (index(&f.source)?, index(&f.destination())?);
    let mut writer = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.destination().join("payload"))?;
    struct MappedPage(*mut libc::c_void);
    impl Drop for MappedPage {
        fn drop(&mut self) {
            // SAFETY: this owns exactly one successful 4096-byte mapping.
            unsafe {
                libc::munmap(self.0, 4096);
            }
        }
    }
    // SAFETY: a live writable regular-file descriptor with length >4096.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            4096,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            writer.as_raw_fd(),
            0,
        )
    };
    ensure!(mapping != libc::MAP_FAILED, "mmap failed");
    let mapping = MappedPage(mapping);
    let output = checked(f.twig(&["adopt", "--dedupe", "../feature"])?)?;
    assert!(String::from_utf8_lossy(&output.stdout).contains("kernel_deduped_bytes="));
    assert_eq!(files(&f.source)?, before_source);
    // Kernel may change ctime/allocation, not bytes, identity, mode or mtime.
    assert_eq!(files(&f.destination())?, before_target);
    assert_eq!((index(&f.source)?, index(&f.destination())?), indices);
    writer.write_all(b"private")?;
    writer.flush()?;
    let mut read = [0u8; 7];
    fs::File::open(f.destination().join("payload"))?.read_exact_at(&mut read, 0)?;
    assert_eq!(&read, b"private");
    // A mapping established before adoption must still write this same inode,
    // not a detached backup left behind by a pathname replacement.
    unsafe {
        *((mapping.0 as *mut u8).add(16)) = b'!';
    }
    fs::File::open(f.destination().join("payload"))?.read_exact_at(&mut read[..1], 16)?;
    assert_eq!(read[0], b'!');
    assert_eq!(files(&f.source)?, before_source);
    assert!(stages(&f)?.is_empty());
    Ok(())
}
