//! Experimental existing-worktree sharing; never replace a worktree file.
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

const USAGE: &str =
    "prototype: git twig adopt [--dry-run | --prepare | --dedupe] <linked-worktree>";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Inspect,
    Prepare,
    Dedupe,
}

struct Request {
    mode: Mode,
    target: PathBuf,
}
impl Request {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut mode = Mode::Inspect;
        let mut selected = false;
        let mut target = None;
        for argument in args {
            let choice = match argument.to_str() {
                Some("--dry-run") => Some(Mode::Inspect),
                Some("--prepare") => Some(Mode::Prepare),
                Some("--dedupe") => Some(Mode::Dedupe),
                _ => None,
            };
            if let Some(choice) = choice {
                ensure!(!selected, "choose exactly one adoption mode; {USAGE}");
                mode = choice;
                selected = true;
            } else {
                ensure!(
                    !argument.is_empty() && !argument.to_string_lossy().starts_with('-'),
                    "{USAGE}"
                );
                ensure!(target.is_none(), "{USAGE}");
                target = Some(PathBuf::from(argument));
            }
        }
        Ok(Self {
            mode,
            target: target.context(USAGE)?,
        })
    }
}

pub(super) fn run(cwd: &Path, args: &[OsString]) -> Result<i32> {
    let request = Request::parse(args)?;
    #[cfg(any(unix, windows))]
    return imp::run(cwd, request);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cwd, request);
        anyhow::bail!("adoption requires macOS, Linux or Windows");
    }
}

#[cfg(any(unix, windows))]
mod imp {
    use super::*;
    use crate::native::canonicalize;
    #[cfg(unix)]
    use std::os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt},
    };
    use std::{
        collections::HashSet,
        fs::{self, File, OpenOptions},
        io::Write,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[cfg(unix)]
    use crate::file_bytes::{directory, equal, open_relative};
    use crate::registry::git;
    #[cfg(windows)]
    use crate::windows::{Access, directory, equal};
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(target_os = "macos")]
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    fn repository(path: &Path) -> Result<PathBuf> {
        let root = crate::output_path(
            crate::checked(git(path).args(["rev-parse", "--show-toplevel"]))?.stdout,
        )?;
        canonicalize(root)
    }
    fn common(root: &Path) -> Result<PathBuf> {
        canonicalize(crate::output_path(
            crate::checked(git(root).args([
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ]))?
            .stdout,
        )?)
    }
    struct Pair {
        donor: File,
        target: File,
        bytes: u64,
    }
    #[cfg(unix)]
    fn matching_pair(donor: &File, target: &File, relative: &Path, mode: Mode) -> Result<Pair> {
        let donor = open_relative(donor, relative, false)?;
        let target = open_relative(target, relative, mode == Mode::Dedupe)?;
        let a = donor.metadata()?;
        let b = target.metadata()?;
        ensure!(a.is_file() && b.is_file(), "not two regular files");
        ensure!(a.dev() == b.dev(), "different devices");
        ensure!(a.ino() != b.ino(), "same inode");
        ensure!(a.nlink() == 1 && b.nlink() == 1, "multiply-linked file");
        compared(donor, target, a.len(), b.len())
    }
    /// Windows has no kernel compare-and-share. Deduplication instead holds both
    /// files open denying writes to everyone else, from this comparison until
    /// the clusters are shared.
    #[cfg(windows)]
    fn matching_pair(donor: &File, target: &File, relative: &Path, mode: Mode) -> Result<Pair> {
        let (donor_access, target_access) = if mode == Mode::Dedupe {
            (Access::ReadLocked, Access::WriteLocked)
        } else {
            (Access::Read, Access::Read)
        };
        let donor = crate::windows::open_relative(donor, relative, donor_access)
            .context("donor is busy or not a regular file")?;
        let target = crate::windows::open_relative(target, relative, target_access)
            .context("target is busy or not a regular file")?;
        let a = donor.metadata()?;
        let b = target.metadata()?;
        ensure!(a.is_file() && b.is_file(), "not two regular files");
        let (left, right) = (
            crate::windows::identity_of(&donor)?,
            crate::windows::identity_of(&target)?,
        );
        ensure!(left.volume == right.volume, "different volumes");
        ensure!(left.index != right.index, "same file");
        ensure!(
            crate::windows::links(&donor)? == 1 && crate::windows::links(&target)? == 1,
            "multiply-linked file"
        );
        compared(donor, target, a.len(), b.len())
    }
    fn compared(donor: File, target: File, a: u64, b: u64) -> Result<Pair> {
        ensure!(a > 0 && a == b, "empty or unequal lengths");
        let pair = Pair {
            donor,
            target,
            bytes: a,
        };
        ensure!(
            equal(&pair.donor, &pair.target, pair.bytes)?,
            "different current bytes"
        );
        Ok(pair)
    }

    fn prepare_directory(common: &Path) -> Result<PathBuf> {
        // Do not reuse a path, remove previous attempts, or touch checkout files.
        let path = common.join(format!(
            "twig-adopt-prototype-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        #[cfg(unix)]
        {
            fs::DirBuilder::new().mode(0o700).create(&path)?;
            ensure!(
                fs::symlink_metadata(&path)?.mode() & 0o777 == 0o700,
                "filesystem cannot protect private preparation scratch; empty directory retained at {path:?}"
            );
        }
        // Windows scratch inherits the Git directory's ACL, as staging does.
        #[cfg(windows)]
        fs::create_dir(&path)?;
        eprintln!(
            "git-twig: prepared candidates retained at {path:?}; existing files will NOT be replaced"
        );
        Ok(path)
    }

    #[cfg(target_os = "macos")]
    fn clone_descriptor(source: &File, destination: &Path) -> Result<()> {
        let destination = CString::new(destination.as_os_str().as_bytes())?;
        // SAFETY: live source fd, NUL-terminated fresh destination; Darwin's
        // fclonefileat clones this inode, not a racy pathname lookup of donor.
        let result = unsafe {
            libc::fclonefileat(
                source.as_raw_fd(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                0x0008,
            )
        };
        ensure!(
            result == 0,
            "clone failed: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn clone_descriptor(source: &File, destination: &Path) -> Result<()> {
        let target = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)?;
        // SAFETY: both descriptors are live regular files; destination is new.
        let result = unsafe { libc::ioctl(target.as_raw_fd(), libc::FICLONE, source.as_raw_fd()) };
        ensure!(
            result == 0,
            "clone failed: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    #[cfg(windows)]
    fn clone_descriptor(source: &File, destination: &Path) -> Result<()> {
        crate::windows::clone_handle(source, destination)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    fn clone_descriptor(_: &File, _: &Path) -> Result<()> {
        anyhow::bail!("native cloning unavailable")
    }

    pub(super) fn run(cwd: &Path, request: Request) -> Result<i32> {
        crate::registry::ensure_environment()?;
        if request.mode == Mode::Dedupe {
            ensure!(
                cfg!(any(
                    all(
                        target_os = "linux",
                        any(target_arch = "x86_64", target_arch = "aarch64")
                    ),
                    windows
                )),
                "in-place adoption requires Linux or Windows; APFS live file replacement is intentionally unsupported"
            );
        }
        let donor_path = repository(cwd)?;
        let target_path = canonicalize(cwd.join(&request.target))?;
        ensure!(
            repository(&target_path)? == target_path,
            "target must be the worktree root"
        );
        ensure!(
            donor_path != target_path,
            "donor and target must be distinct worktrees"
        );
        let common = common(&donor_path)?;
        ensure!(
            common == self::common(&target_path)?,
            "worktrees must share Git common metadata"
        );
        ensure!(
            fs::symlink_metadata(target_path.join(".git"))?.is_file(),
            "target must be a linked worktree"
        );
        let registered = crate::registry::read(&donor_path)?;
        ensure!(
            registered
                .iter()
                .any(|record| { canonicalize(&record.path).ok().as_ref() == Some(&target_path) }),
            "target is not a registered worktree"
        );
        let output = crate::checked(git(&target_path).args([
            "ls-files",
            "--cached",
            "--deduplicate",
            "-z",
        ]))?;
        let donor_files =
            crate::checked(git(&donor_path).args(["ls-files", "--cached", "--deduplicate", "-z"]))?;
        let donor_paths: HashSet<&[u8]> = donor_files
            .stdout
            .split(|b| *b == 0)
            .filter(|path| !path.is_empty())
            .collect();
        let donor = directory(&donor_path)?;
        let target = directory(&target_path)?;
        let stage = if request.mode == Mode::Prepare {
            Some(prepare_directory(&common)?)
        } else {
            None
        };
        let mut journal = stage
            .as_ref()
            .map(|path| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path.join("manifest.txt"))
            })
            .transpose()?;
        let mut matched = 0u64;
        let mut matched_bytes = 0u64;
        let mut skipped = 0u64;
        let mut deduped_bytes = 0u64;
        for raw in output.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let relative = PathBuf::from(crate::native::os_string(raw.to_vec()));
            if !donor_paths.contains(raw) {
                skipped += 1;
                println!("skip {relative:?}: donor path is not tracked");
                continue;
            }
            let pair = match matching_pair(&donor, &target, &relative, request.mode) {
                Ok(pair) => pair,
                Err(error) => {
                    skipped += 1;
                    println!("skip {relative:?}: {error:#}");
                    continue;
                }
            };
            if let Some(stage) = &stage {
                let candidate = stage.join(format!("candidate-{matched:08}"));
                clone_descriptor(&pair.donor, &candidate)?;
                ensure!(
                    equal(&File::open(&candidate)?, &pair.target, pair.bytes)?,
                    "candidate changed relative to target; retained at {candidate:?}"
                );
                let journal = journal.as_mut().expect("stage owns journal");
                writeln!(
                    journal,
                    "{candidate:?} target={relative:?} logical_bytes={}",
                    pair.bytes
                )?;
                journal.flush()?;
            }
            if request.mode == Mode::Dedupe {
                deduped_bytes += dedupe(&pair)?;
            }
            matched += 1;
            matched_bytes += pair.bytes;
            println!("match {relative:?}: {} logical bytes", pair.bytes);
        }
        println!(
            "prototype: matched_files={matched} matched_logical_bytes={matched_bytes} skipped_files={skipped} kernel_deduped_bytes={deduped_bytes}; physical savings NOT measured"
        );
        if request.mode != Mode::Dedupe {
            println!("existing worktree files and index were not replaced or written");
        }
        Ok(0)
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    fn dedupe(pair: &Pair) -> Result<u64> {
        #[repr(C)]
        struct Info {
            fd: i64,
            offset: u64,
            bytes: u64,
            status: i32,
            reserved: u32,
        }
        #[repr(C)]
        struct Range {
            offset: u64,
            length: u64,
            count: u16,
            reserved1: u16,
            reserved2: u32,
            info: Info,
        }
        // Linux UAPI: _IOWR(0x94, 54, struct file_dedupe_range), whose
        // fixed header is 24 bytes; the single info entry is 32 bytes.
        const REQUEST: libc::c_ulong = 0xc0189436;
        const _: () = {
            assert!(std::mem::size_of::<Range>() == 56);
            assert!(std::mem::size_of::<Info>() == 32);
            assert!(std::mem::offset_of!(Range, info) == 24);
        };
        let mut offset = 0;
        while offset < pair.bytes {
            let length = (pair.bytes - offset).min(16 * 1024 * 1024);
            let mut range = Range {
                offset,
                length,
                count: 1,
                reserved1: 0,
                reserved2: 0,
                info: Info {
                    fd: pair.target.as_raw_fd().into(),
                    offset,
                    bytes: 0,
                    status: 0,
                    reserved: 0,
                },
            };
            // SAFETY: Range has the UAPI layout and space for exactly one info
            // entry; both file descriptors live throughout the synchronous call.
            let status = unsafe { libc::ioctl(pair.donor.as_raw_fd(), REQUEST, &mut range) };
            ensure!(
                status == 0,
                "dedupe syscall failed after {offset} bytes: {}",
                std::io::Error::last_os_error()
            );
            ensure!(
                range.info.status == 0,
                "dedupe range status {} after {offset} bytes (changed/unsupported range)",
                range.info.status
            );
            ensure!(
                range.info.bytes > 0 && range.info.bytes <= length,
                "invalid dedupe progress"
            );
            offset += range.info.bytes;
        }
        Ok(offset)
    }

    #[cfg(all(
        test,
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    #[ignore = "requires independently provisioned XFS/Btrfs; run with --include-ignored"]
    fn kernel_rejects_changed_bytes_after_a_successful_comparison() -> Result<()> {
        use std::os::unix::fs::FileExt;
        let root = std::env::var_os("TWIG_TEST_FIXTURE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-fixtures")
            });
        let root = root.join(format!(
            "adopt-kernel-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::create_dir_all(&root)?;
        eprintln!("fixture retained: {root:?}");
        fs::write(root.join("donor"), vec![b'x'; 16384])?;
        fs::write(root.join("target"), vec![b'x'; 16384])?;
        let probe = std::process::Command::new("cp")
            .arg("--reflink=always")
            .arg(root.join("donor"))
            .arg(root.join("probe"))
            .output()?;
        if !probe.status.success() {
            ensure!(
                std::env::var_os("TWIG_TEST_REQUIRE_NATIVE").is_none(),
                "required independent reflink probe failed"
            );
            return Ok(());
        }
        let pair = Pair {
            donor: File::open(root.join("donor"))?,
            target: OpenOptions::new()
                .read(true)
                .write(true)
                .open(root.join("target"))?,
            bytes: 16384,
        };
        assert!(equal(&pair.donor, &pair.target, pair.bytes)?);
        let inode = pair.target.metadata()?.ino();
        pair.target.write_all_at(b"new edit", 4096)?;
        let error =
            dedupe(&pair).expect_err("kernel must not accept the stale equality observation");
        assert!(error.to_string().contains("range status 1"), "{error:#}");
        assert_eq!(pair.target.metadata()?.ino(), inode);
        let mut bytes = [0u8; 8];
        pair.target.read_exact_at(&mut bytes, 4096)?;
        assert_eq!(&bytes, b"new edit");
        pair.donor.read_exact_at(&mut bytes, 4096)?;
        assert_eq!(&bytes, b"xxxxxxxx");
        Ok(())
    }

    #[cfg(windows)]
    fn dedupe(pair: &Pair) -> Result<u64> {
        crate::windows::dedupe_locked(&pair.donor, &pair.target, pair.bytes)
    }

    #[cfg(not(any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        windows
    )))]
    fn dedupe(_: &Pair) -> Result<u64> {
        anyhow::bail!("in-place deduplication unavailable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_modes_are_explicit_and_invalid_requests_fail() {
        for args in [
            vec![],
            vec!["--force", "target"],
            vec!["--prepare", "--dedupe", "target"],
            vec!["a", "b"],
        ] {
            assert!(
                Request::parse(&args.into_iter().map(OsString::from).collect::<Vec<_>>()).is_err()
            );
        }
        let request = Request::parse(&["target".into()]).unwrap();
        assert!(request.mode == Mode::Inspect);
    }
}
