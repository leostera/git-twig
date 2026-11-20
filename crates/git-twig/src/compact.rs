//! Explicitly offline APFS compaction; never an authorization to replace live files.
mod metadata;

use crate::{
    file_bytes::{directory, equal, open_relative},
    registry::git,
};
use anyhow::{Context, Result, bail, ensure};
use metadata::Metadata;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{CString, OsStr},
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const AT_NODELETEBUSY: libc::c_int = 0x4000;
const CLONE_NOFOLLOW_ANY: u32 = 0x0008;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    bytes: u64,
    links: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
impl Stamp {
    fn read(file: &File) -> Result<Self> {
        let stat = file.metadata()?;
        ensure!(
            stat.is_file() && stat.nlink() == 1 && stat.len() > 0,
            "not a nonempty single-linked regular file"
        );
        Ok(Self {
            device: stat.dev(),
            inode: stat.ino(),
            bytes: stat.len(),
            links: stat.nlink(),
            mtime: (stat.mtime(), stat.mtime_nsec()),
            ctime: (stat.ctime(), stat.ctime_nsec()),
        })
    }
    fn same_inode(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}
struct Pair {
    donor: File,
    original: File,
    parent: File,
    name: CString,
    stamp: Stamp,
    metadata: Metadata,
}
impl Pair {
    fn open(donor: &File, target: &File, relative: &Path) -> Result<Self> {
        let donor = open_relative(donor, relative, false)?;
        let parent = match relative.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(path) => open_relative(target, path, false)?,
            None => target.try_clone()?,
        };
        ensure!(
            parent.metadata()?.is_dir(),
            "target parent is not a directory"
        );
        let leaf = relative.file_name().context("missing target basename")?;
        let original = open_relative(&parent, Path::new(leaf), false)?;
        metadata::ensure_supported_flags(&donor)?;
        let a = Stamp::read(&donor)?;
        let stamp = Stamp::read(&original)?;
        ensure!(
            a.device == stamp.device && !a.same_inode(stamp),
            "different devices or same inode"
        );
        ensure!(
            a.bytes == stamp.bytes && equal(&donor, &original, stamp.bytes)?,
            "different current bytes"
        );
        let metadata = Metadata::read(&original)?;
        ensure!(
            Stamp::read(&original)? == stamp,
            "target changed during inspection"
        );
        Ok(Self {
            donor,
            original,
            parent,
            name: CString::new(leaf.as_bytes())?,
            stamp,
            metadata,
        })
    }
    fn entry(&self) -> Result<File> {
        open_relative(
            &self.parent,
            Path::new(OsStr::from_bytes(self.name.as_bytes())),
            false,
        )
    }
}

fn tracked(root: &Path) -> Result<Vec<PathBuf>> {
    let output = crate::checked(git(root).args(["ls-files", "--cached", "--deduplicate", "-z"]))?;
    ensure!(
        output.stdout.is_empty() || output.stdout.ends_with(b"\0"),
        "incomplete tracked-file inventory"
    );
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|raw| PathBuf::from(crate::native::os_string(raw.to_vec())))
        .collect())
}
#[derive(Default)]
pub(super) struct Summary {
    pub matched_files: u64,
    pub matched_bytes: u64,
    pub compacted_files: u64,
    pub compacted_bytes: u64,
    skipped: BTreeMap<String, u64>,
}
impl Summary {
    fn skip(&mut self, error: &anyhow::Error, path: &Path) {
        *self.skipped.entry(error.to_string()).or_default() += 1;
        crate::trace(&format!("compaction skip {path:?}: {error:#}"));
    }
    pub fn show(&self, target: &Path, preview: bool) {
        if preview {
            println!(
                "compaction-preview {target:?}: {} matching files ({} bytes) can become clones of main",
                self.matched_files, self.matched_bytes
            );
        } else {
            println!(
                "compaction-result {target:?}: {} of {} matching files ({} bytes) are now clones of main",
                self.compacted_files, self.matched_files, self.compacted_bytes
            );
        }
        for (reason, count) in &self.skipped {
            println!("  skipped {count}: {reason}");
        }
    }
}
enum Application {
    Preview,
    Compacted,
    Skipped(anyhow::Error),
}
fn visit(
    donor: &Path,
    target: &Path,
    mut action: impl FnMut(Pair, &Path) -> Result<Application>,
) -> Result<Summary> {
    let donor_paths: HashSet<_> = tracked(donor)?.into_iter().collect();
    let donor = directory(donor)?;
    let target_directory = directory(target)?;
    let mut summary = Summary::default();
    for relative in tracked(target)? {
        if !donor_paths.contains(&relative) {
            summary.skip(&anyhow::anyhow!("donor path is not tracked"), &relative);
            continue;
        }
        let pair = match Pair::open(&donor, &target_directory, &relative) {
            Ok(pair) => pair,
            Err(error) => {
                summary.skip(&error, &relative);
                continue;
            }
        };
        summary.matched_files += 1;
        summary.matched_bytes += pair.stamp.bytes;
        let bytes = pair.stamp.bytes;
        match action(pair, &relative)? {
            Application::Compacted => {
                summary.compacted_files += 1;
                summary.compacted_bytes += bytes;
            }
            Application::Skipped(error) => summary.skip(&error, &relative),
            Application::Preview => {}
        }
    }
    Ok(summary)
}
pub(super) fn preview(donor: &Path, target: &Path) -> Result<Summary> {
    visit(donor, target, |_, _| Ok(Application::Preview))
}

/// The caller must obtain explicit offline consent. This is an owner assertion,
/// not exclusive kernel ownership; no process scan is claimed to prove it.
pub(super) struct Session {
    path: PathBuf,
    directory: File,
    journal: File,
    sequence: u64,
}
impl Session {
    /// Recovery state lives under the common Git directory, in `twigs/compact`.
    /// Everything is done through open directory handles, so it keeps working
    /// when adoption moves main and its Git directory.
    pub fn new(main: &Path, _consent: crate::adopt::OfflineConsent) -> Result<Self> {
        let parent = crate::state_directory(main)?.join("compact");
        match fs::DirBuilder::new().mode(0o700).create(&parent) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            result => result?,
        }
        ensure!(
            fs::metadata(&parent)?.dev() == fs::metadata(main)?.dev(),
            "offline compaction needs Git metadata on the worktrees' filesystem; no worktrees moved"
        );
        let parent = parent.as_path();
        let path = parent.join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        ensure!(
            fs::symlink_metadata(&path)?.mode() & 0o777 == 0o700,
            "cannot protect compaction recovery directory: {path:?}"
        );
        let recovery_directory = directory(&path)?;
        metadata::ensure_private_directory(&recovery_directory).with_context(|| {
            format!("cannot protect {path:?}; empty recovery directory retained")
        })?;
        let journal = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join("journal.nul"))?;
        let mut session = Self {
            directory: recovery_directory,
            path,
            journal,
            sequence: 0,
        };
        let recovery_path = session.path.clone();
        let recovery_stat = session.directory.metadata()?;
        session.record(
            "twig-compact-v1",
            &recovery_path,
            &recovery_path,
            recovery_stat.dev(),
            recovery_stat.ino(),
        )?;
        directory(parent)?.sync_all()?;
        crate::trace(&format!(
            "compaction journal: {:?} (kept only if recovery needs it)",
            session.path.join("journal.nul")
        ));
        if let Err(error) = session.probe() {
            // Only probe files exist yet, and no worktree has moved.
            session.finish();
            bail!("offline compaction primitives unavailable; no worktrees moved: {error:#}");
        }
        Ok(session)
    }
    /// Adoption moved main, and the Git directory holding this session with it.
    pub fn relocated(&mut self, from: &Path, to: &Path) {
        if let Ok(rest) = self.path.strip_prefix(from) {
            self.path = to.join(rest);
        }
    }
    /// Discard recovery state once every target is compacted. Originals are
    /// retired by then; what remains is the journal and candidates refused
    /// before installation, which were never part of any worktree.
    pub fn finish(self) {
        let Self {
            path,
            directory,
            journal,
            ..
        } = self;
        drop((directory, journal));
        if let Err(error) = fs::remove_dir_all(&path) {
            eprintln!("git-twig: kept compaction state at {path:?}: {error}");
        } else if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
    fn record(
        &mut self,
        phase: &str,
        target: &Path,
        temporary: &Path,
        original: u64,
        clone: u64,
    ) -> Result<()> {
        for field in [
            phase.as_bytes(),
            target.as_os_str().as_bytes(),
            temporary.as_os_str().as_bytes(),
            original.to_string().as_bytes(),
            clone.to_string().as_bytes(),
        ] {
            self.journal.write_all(field)?;
            self.journal.write_all(b"\0")?;
        }
        self.journal.sync_all()?;
        Ok(())
    }
    fn probe(&mut self) -> Result<()> {
        let source = self.path.join("probe-source");
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&source)?;
        file.write_all(b"native compaction probe\n")?;
        clone_file(&file, &self.directory, c"probe-clone")?;
        swap(
            &self.directory,
            c"probe-source",
            &self.directory,
            c"probe-clone",
        )?;
        match retire(&self.directory, c"probe-clone") {
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {}
            Err(error) => bail!("busy-unlink guard is not supported: {error}"),
            Ok(()) => bail!("busy-unlink guard was ignored; refusing compaction"),
        }
        drop(file);
        retire_closed(&self.directory, c"probe-clone")?;
        retire_closed(&self.directory, c"probe-source")?;
        self.record("capabilities-verified", &source, &source, 0, 0)
    }
    pub fn compact(&mut self, donor: &Path, target: &Path) -> Result<Summary> {
        let result = visit(donor, target, |pair, relative| {
            self.replace(pair, &target.join(relative))
        });
        result.with_context(|| format!("compaction stopped; layout remains adopted; retain both installed and recovery files and inspect {:?} before restarting agents", self.path.join("journal.nul")))
    }
    fn replace(&mut self, pair: Pair, target: &Path) -> Result<Application> {
        let id = self.sequence;
        self.sequence += 1;
        let name = CString::new(format!("file-{id:08}"))?;
        let temporary = self.path.join(OsStr::from_bytes(name.as_bytes()));
        self.record("prepare-intent", target, &temporary, pair.stamp.inode, 0)?;
        clone_file(&pair.donor, &self.directory, &name)?;
        let candidate = open_relative(
            &self.directory,
            Path::new(OsStr::from_bytes(name.as_bytes())),
            false,
        )?;
        // A metadata mismatch is a safe refusal before installation, not permission
        // to lose target attributes. Its private candidate is retained for diagnosis.
        if let Err(error) = pair.metadata.copy_to(&pair.original, &candidate) {
            self.record(
                "metadata-refused",
                target,
                &temporary,
                pair.stamp.inode,
                candidate.metadata()?.ino(),
            )?;
            return Ok(Application::Skipped(error));
        }
        ensure!(
            Stamp::read(&pair.original)? == pair.stamp
                && Stamp::read(&pair.entry()?)? == pair.stamp,
            "target identity/state changed before swap"
        );
        ensure!(
            equal(&candidate, &pair.original, pair.stamp.bytes)?,
            "candidate bytes changed before swap"
        );
        let clone = Stamp::read(&candidate)?;
        ensure!(
            !clone.same_inode(pair.stamp) && clone.device == pair.stamp.device,
            "candidate is not a private same-filesystem inode"
        );
        candidate.sync_all()?;
        self.record(
            "swap-intent",
            target,
            &temporary,
            pair.stamp.inode,
            clone.inode,
        )?;
        swap(&self.directory, &name, &pair.parent, &pair.name)?;
        self.record("swapped", target, &temporary, pair.stamp.inode, clone.inode)?;
        let installed = pair.entry()?;
        let displaced = open_relative(
            &self.directory,
            Path::new(OsStr::from_bytes(name.as_bytes())),
            false,
        )?;
        ensure!(
            Stamp::read(&installed)?.same_inode(clone)
                && Stamp::read(&displaced)?.same_inode(pair.stamp),
            "namespace changed after swap; retain both versions"
        );
        ensure!(
            equal(&candidate, &pair.original, pair.stamp.bytes)?
                && Metadata::read(&pair.original)? == pair.metadata
                && Metadata::read(&installed)? == pair.metadata,
            "post-swap data/metadata changed; retain both versions"
        );
        // Persist installed namespace and retained original before retirement.
        pair.parent.sync_all()?;
        self.directory.sync_all()?;
        self.record(
            "verified-retire-intent",
            target,
            &temporary,
            pair.stamp.inode,
            clone.inode,
        )?;
        drop(displaced);
        drop(pair.original);
        // Only this verified original is retired. An external open descriptor
        // prevents deletion; failure retains both versions, never a blind rollback.
        retire_original(&self.directory, &name, pair.stamp).context("cannot retire original (possibly still open); offline precondition may be violated; both versions retained")?;
        self.directory.sync_all()?;
        self.record("retired", target, &temporary, pair.stamp.inode, clone.inode)?;
        Ok(Application::Compacted)
    }
}
fn clone_file(source: &File, directory: &File, name: &std::ffi::CStr) -> Result<()> {
    // SAFETY: live source/directory descriptors and fresh NUL-terminated basename.
    let result = unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            directory.as_raw_fd(),
            name.as_ptr(),
            CLONE_NOFOLLOW_ANY,
        )
    };
    ensure!(
        result == 0,
        "native clone failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
fn swap(a: &File, from: &std::ffi::CStr, b: &File, to: &std::ffi::CStr) -> Result<()> {
    // SAFETY: live parent descriptors and literal basenames; both entries must exist.
    let result = unsafe {
        libc::renameatx_np(
            a.as_raw_fd(),
            from.as_ptr(),
            b.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    ensure!(
        result == 0,
        "atomic compaction swap failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}
fn retire_original(directory: &File, name: &std::ffi::CStr, expected: Stamp) -> Result<()> {
    let entry = open_relative(
        directory,
        Path::new(OsStr::from_bytes(name.to_bytes())),
        false,
    )?;
    let current = Stamp::read(&entry)?;
    ensure!(
        current.same_inode(expected) && current.bytes == expected.bytes,
        "private original identity changed before retirement"
    );
    drop(entry);
    retire(directory, name)?;
    Ok(())
}
/// Retire a probe file Twig has just closed. Indexers such as Spotlight open
/// new files briefly, so the busy guard can still refuse it for a moment;
/// that says nothing about the primitives. Real retirements never retry: a
/// busy original keeps both versions.
fn retire_closed(directory: &File, name: &std::ffi::CStr) -> std::io::Result<()> {
    let mut delay = std::time::Duration::from_millis(10);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match retire(directory, name) {
            Err(error)
                if error.raw_os_error() == Some(libc::EBUSY)
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(delay);
                delay = (delay * 2).min(std::time::Duration::from_millis(500));
            }
            result => return result,
        }
    }
}
fn retire(directory: &File, name: &std::ffi::CStr) -> std::io::Result<()> {
    // SAFETY: parent descriptor is private/owned; the busy guard refuses open files.
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), AT_NODELETEBUSY) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_probe_files_are_retired_once_a_brief_opener_closes() -> Result<()> {
        let root = std::env::temp_dir().join(format!("twig-retire-{}", std::process::id()));
        fs::create_dir_all(&root)?;
        fs::write(root.join("probe"), b"probe")?;
        let parent = File::open(&root)?;
        // Stands in for an indexer that opens a file Twig has just closed.
        let opener = File::open(root.join("probe"))?;
        let busy = retire(&parent, c"probe").expect_err("open files are busy");
        assert_eq!(busy.raw_os_error(), Some(libc::EBUSY));
        let closer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(opener);
        });
        retire_closed(&parent, c"probe")?;
        closer.join().expect("closer panicked");
        assert!(!root.join("probe").exists());
        fs::remove_dir(&root)?;
        Ok(())
    }
}
