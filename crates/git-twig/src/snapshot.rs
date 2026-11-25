//! Clone tracked entries; macOS bulk-clones subtrees before pruning to the Git tree.
use crate::{
    native,
    tree::{Entry, Kind},
};
use anyhow::{Result, bail, ensure};
use std::{collections::BTreeMap, ffi::OsString, fs, path::Path};

#[derive(Default)]
struct Directory {
    children: BTreeMap<OsString, Node>,
}
enum Node {
    Directory(Directory),
    File { executable: bool },
    Symlink,
}
impl Directory {
    fn insert(&mut self, parts: &[OsString], kind: Kind) -> Result<()> {
        let (name, rest) = parts.split_first().expect("parsed paths are nonempty");
        if rest.is_empty() {
            ensure!(!self.children.contains_key(name), "duplicate tree path");
            let node = match kind {
                Kind::File { executable } => Node::File { executable },
                Kind::Symlink => Node::Symlink,
                Kind::Submodule => bail!("submodules require stock Git"),
            };
            self.children.insert(name.clone(), node);
        } else {
            let child = self
                .children
                .entry(name.clone())
                .or_insert_with(|| Node::Directory(Directory::default()));
            match child {
                Node::Directory(directory) => directory.insert(rest, kind)?,
                _ => bail!("tree file is also a directory"),
            }
        }
        Ok(())
    }
}

pub fn materialize(
    root: &Path,
    data: &Path,
    entries: &[Entry],
    modes: &native::FileModes,
) -> Result<usize> {
    let mut tree = Directory::default();
    for entry in entries {
        tree.insert(
            &entry.path.iter().map(OsString::from).collect::<Vec<_>>(),
            entry.kind,
        )?;
    }
    // A directory clone keeps every donor file's own ACL, while a new file, or
    // a clone of one, takes the ACL its directory passes on, as Git's files do.
    // Staging inherited any such ACL from the destination's parent.
    #[cfg(target_os = "macos")]
    let per_file = native::has_acl(data)?;
    #[cfg(target_os = "macos")]
    let mut directories = 0;
    // Never snapshot the repository root: .git and unrelated root trees stay out entirely.
    for (name, node) in &tree.children {
        let source = root.join(name);
        let target = data.join(name);
        // A donor in use has deleted and replaced tracked paths. Leave those for
        // Git to write, and keep sharing everything that is still what Git tracks.
        if !still_tracked_kind(&source, node)? {
            continue;
        }
        match node {
            Node::Directory(directory) => {
                #[cfg(any(target_os = "linux", windows))]
                {
                    let mut files = Vec::new();
                    plan_tracked_directory(&source, &target, directory, &mut files)?;
                    clone_planned_files(&files, modes)?;
                    continue; // The file walker already establishes types and checkout modes.
                }
                #[cfg(target_os = "macos")]
                {
                    if per_file {
                        let mut files = Vec::new();
                        plan_tracked_directory(&source, &target, directory, &mut files)?;
                        clone_planned_files(&files, modes)?;
                        continue;
                    }
                    // A directory clone copies everything under it, ignored trees
                    // included (nested worktrees, node_modules, build caches),
                    // and normalize then deletes them again. Clone whole only the
                    // subtrees that hold nothing Git does not track, and go file
                    // by file just along the way to untracked entries.
                    let mut plan = Plan::default();
                    if !plan.directory(&source, &target, directory)? {
                        directories += plan.run(modes)?;
                        continue;
                    }
                    native::clone_path(&source, &target)?;
                    directories += 1;
                }
            }
            Node::File { .. } => native::clone_path(&source, &target)?,
            Node::Symlink => native::symlink(&fs::read_link(&source)?, &target)?,
        }
        normalize(
            &target,
            node,
            fs::symlink_metadata(&target)?.file_type(),
            modes,
        )?;
    }
    #[cfg(target_os = "macos")]
    return Ok(directories);
    #[cfg(not(target_os = "macos"))]
    Ok(0)
}

// Linux FICLONE works on regular files, not directory trees. Walk only the
// committed inventory so ignored files and unrelated special files are never opened.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
struct PlannedFile {
    source: std::path::PathBuf,
    target: std::path::PathBuf,
    executable: bool,
}

/// Create the tracked directories and symlinks, and list the regular files to
/// clone. Directories exist before any file worker starts.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn plan_tracked_directory(
    source: &Path,
    target: &Path,
    directory: &Directory,
    files: &mut Vec<PlannedFile>,
) -> Result<()> {
    // mkdir already applies umask/default ACL and inherits SGID/GID like Git.
    // Even chmod(mode | SGID) clears SGID when its group is not in our groups.
    fs::create_dir(target)?;
    for (name, node) in &directory.children {
        let source = source.join(name);
        let target = target.join(name);
        match node {
            Node::Directory(directory) => {
                if still_tracked_kind(&source, node)? {
                    plan_tracked_directory(&source, &target, directory, files)?;
                }
            }
            // Checked by the worker that clones it, next to the clone itself.
            Node::File { executable } => files.push(PlannedFile {
                source,
                target,
                executable: *executable,
            }),
            Node::Symlink => {
                if still_tracked_kind(&source, node)? {
                    native::symlink(&fs::read_link(&source)?, &target)?;
                }
            }
        }
    }
    Ok(())
}

/// Reflink each planned file. One clone is a handful of system calls with no
/// shared state, so a large inventory is split across a few threads.
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn clone_planned_files(files: &[PlannedFile], modes: &native::FileModes) -> Result<()> {
    let clone = |file: &PlannedFile| -> Result<()> {
        let kind = Node::File {
            executable: file.executable,
        };
        if still_tracked_kind(&file.source, &kind)? {
            native::clone_path(&file.source, &file.target)?;
            // clonefile also copies the donor's mode, BSD flags and extended
            // attributes; Linux and Windows clones start with none of them.
            #[cfg(target_os = "macos")]
            {
                let metadata = fs::symlink_metadata(&file.target)?;
                native::discard_donor_metadata(&file.target, &metadata)?;
                modes.ensure(&file.target, &metadata, file.executable)?;
            }
            #[cfg(not(target_os = "macos"))]
            modes.apply(&file.target, file.executable)?;
        }
        Ok(())
    };
    let workers = clone_worker_count(
        files.len(),
        std::thread::available_parallelism().map_or(1, usize::from),
    );
    if workers == 1 {
        return files.iter().try_for_each(clone);
    }
    // Every worker joins before the first error is returned: nothing may still
    // be writing private files when preparation fails over to Git.
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(files.len().div_ceil(workers))
            .map(|chunk| scope.spawn(move || chunk.iter().try_for_each(clone)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| anyhow::anyhow!("clone worker panicked"))?
            })
            .collect::<Vec<Result<()>>>()
            .into_iter()
            .collect()
    })
}

#[cfg(any(target_os = "macos", target_os = "linux", windows, test))]
fn clone_worker_count(files: usize, cpus: usize) -> usize {
    if files >= 1024 { cpus.clamp(1, 8) } else { 1 }
}

/// How to clone one top-level donor directory that holds untracked entries:
/// whole-directory clones for its clean subtrees, and directories, symlinks and
/// single-file clones on the way to the untracked entries.
#[cfg(target_os = "macos")]
#[derive(Default)]
struct Plan<'a> {
    steps: Vec<Step<'a>>,
    files: Vec<PlannedFile>,
}

#[cfg(target_os = "macos")]
enum Step<'a> {
    Create(std::path::PathBuf),
    Clone {
        source: std::path::PathBuf,
        target: std::path::PathBuf,
        node: &'a Node,
    },
    Symlink {
        source: std::path::PathBuf,
        target: std::path::PathBuf,
    },
}

#[cfg(target_os = "macos")]
impl<'a> Plan<'a> {
    /// Plan a tracked directory and report whether the donor holds nothing in
    /// it that Git does not track, in which case the caller clones it whole and
    /// drops what this call planned. Reads each tracked directory once and
    /// never descends into an untracked entry. Only a hint: normalize and
    /// verification still check every byte afterwards.
    fn directory(
        &mut self,
        source: &Path,
        target: &Path,
        directory: &'a Directory,
    ) -> Result<bool> {
        let mut clean = true;
        for child in fs::read_dir(source)? {
            let child = child?;
            match directory.children.get(&child.file_name()) {
                Some(node) if same_kind(node, child.file_type()?) => {}
                _ => clean = false,
            }
        }
        self.steps.push(Step::Create(target.to_path_buf()));
        for (name, node) in &directory.children {
            let source = source.join(name);
            let target = target.join(name);
            match node {
                Node::Directory(child) => {
                    if !still_tracked_kind(&source, node)? {
                        continue;
                    }
                    let (steps, files) = (self.steps.len(), self.files.len());
                    if self.directory(&source, &target, child)? {
                        self.steps.truncate(steps);
                        self.files.truncate(files);
                        self.steps.push(Step::Clone {
                            source,
                            target,
                            node,
                        });
                    } else {
                        clean = false;
                    }
                }
                Node::File { executable } => self.files.push(PlannedFile {
                    source,
                    target,
                    executable: *executable,
                }),
                Node::Symlink => self.steps.push(Step::Symlink { source, target }),
            }
        }
        Ok(clean)
    }

    /// Create the planned directories and clones; returns how many whole
    /// directories were cloned.
    fn run(self, modes: &native::FileModes) -> Result<usize> {
        let mut directories = 0;
        for step in self.steps {
            match step {
                // As in plan_tracked_directory: mkdir applies umask like Git.
                Step::Create(target) => fs::create_dir(&target)?,
                Step::Clone {
                    source,
                    target,
                    node,
                } => {
                    native::clone_path(&source, &target)?;
                    normalize(
                        &target,
                        node,
                        fs::symlink_metadata(&target)?.file_type(),
                        modes,
                    )?;
                    directories += 1;
                }
                Step::Symlink { source, target } => {
                    if still_tracked_kind(&source, &Node::Symlink)? {
                        native::symlink(&fs::read_link(&source)?, &target)?;
                    }
                }
            }
        }
        clone_planned_files(&self.files, modes)?;
        Ok(directories)
    }
}

/// Whether a donor path still holds the kind of entry the target tracks there.
/// Only a hint for skipping work: private bytes are verified afterwards.
fn still_tracked_kind(source: &Path, node: &Node) -> Result<bool> {
    let kind = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata.file_type(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(same_kind(node, kind))
}

fn same_kind(node: &Node, kind: fs::FileType) -> bool {
    match node {
        Node::Directory(_) => kind.is_dir(),
        Node::File { .. } => kind.is_file(),
        Node::Symlink => kind.is_symlink(),
    }
}

fn normalize(
    path: &Path,
    node: &Node,
    actual: fs::FileType,
    modes: &native::FileModes,
) -> Result<()> {
    // The donor replaced this tracked path with something else. Drop the clone:
    // an absent private path is one Git writes.
    if !same_kind(node, actual) {
        return remove_private_tree(path, actual);
    }
    let metadata = fs::symlink_metadata(path)?;
    // Before anything else: an immutable clone refuses the changes below.
    native::discard_donor_metadata(path, &metadata)?;
    match node {
        Node::Directory(expected) => {
            // Source directories may be read-only; checkout permissions come from this umask.
            modes.ensure(path, &metadata, true)?;
            for child in fs::read_dir(path)? {
                let child = child?;
                let actual = child.file_type()?;
                match expected.children.get(&child.file_name()) {
                    Some(node) => normalize(&child.path(), node, actual, modes)?,
                    None => remove_private_tree(&child.path(), actual)?,
                }
            }
        }
        Node::File { executable } => modes.ensure(path, &metadata, *executable)?,
        Node::Symlink => {}
    }
    Ok(())
}

pub(super) fn remove_private_tree(path: &Path, kind: fs::FileType) -> Result<()> {
    match remove_entry(path, kind) {
        // A tracked path Git can reach relative to its checkout may exceed PATH_MAX
        // under the staging prefix. The standard library removes relative to open
        // directories, without following symlinks, so no path grows that long.
        Err(error) if too_long(&error) && kind.is_dir() => Ok(fs::remove_dir_all(path)?),
        result => result,
    }
}

fn too_long(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::InvalidFilename)
}

fn remove_entry(path: &Path, kind: fs::FileType) -> Result<()> {
    if kind.is_dir() {
        // Only owned staging/cloned entries reach here. Never follow a symlink.
        #[cfg(unix)]
        unlocked(path, || {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        })?;
        for child in fs::read_dir(path)? {
            let child = child?;
            remove_entry(&child.path(), child.file_type()?)?;
        }
        fs::remove_dir(path)?;
    } else {
        unlocked(path, || match fs::remove_file(path) {
            // Absence is the goal. exFAT's `._name` companions vanish with their
            // file after the directory listing already named them.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => result,
        })?;
    }
    Ok(())
}

/// Cloned donor entries keep the donor's immutable flags, which refuse removal.
/// Pay for unlocking only when an operation is actually refused.
fn unlocked(path: &Path, operation: impl Fn() -> std::io::Result<()>) -> Result<()> {
    match operation() {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            native::clear_user_flags(path)?;
            Ok(operation()?)
        }
        result => Ok(result?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_file_directory_collisions() {
        let mut tree = Directory::default();
        tree.insert(&["a".into()], Kind::File { executable: false })
            .unwrap();
        assert!(
            tree.insert(&["a".into(), "b".into()], Kind::Symlink)
                .is_err()
        );
    }
    #[test]
    fn small_inventories_clone_on_one_thread_and_large_ones_are_bounded() {
        assert_eq!(clone_worker_count(1023, 64), 1);
        assert_eq!(clone_worker_count(1024, 64), 8);
        assert_eq!(clone_worker_count(100_000, 2), 2);
        assert_eq!(clone_worker_count(100_000, 1), 1);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn plans_whole_clones_only_for_subtrees_without_untracked_entries() {
        let root = std::env::temp_dir().join(format!("twig-plan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for package in ["a", "b"] {
            fs::create_dir_all(root.join("packages").join(package).join("src")).unwrap();
            fs::write(root.join("packages").join(package).join("src/index.js"), "").unwrap();
        }
        let mut tree = Directory::default();
        for package in ["a", "b"] {
            tree.insert(
                &[
                    "packages".into(),
                    package.into(),
                    "src".into(),
                    "index.js".into(),
                ],
                Kind::File { executable: false },
            )
            .unwrap();
        }
        let Node::Directory(packages) = &tree.children[&OsString::from("packages")] else {
            unreachable!()
        };
        let target = root.join("target");
        let clones = |plan: &Plan| -> Vec<std::path::PathBuf> {
            plan.steps
                .iter()
                .filter_map(|step| match step {
                    Step::Clone { source, .. } => {
                        Some(source.strip_prefix(&root).unwrap().to_path_buf())
                    }
                    _ => None,
                })
                .collect()
        };
        let mut plan = Plan::default();
        assert!(
            plan.directory(&root.join("packages"), &target, packages)
                .unwrap()
        );

        fs::create_dir_all(root.join("packages/b/node_modules/dep")).unwrap();
        let mut plan = Plan::default();
        assert!(
            !plan
                .directory(&root.join("packages"), &target, packages)
                .unwrap()
        );
        assert_eq!(
            clones(&plan),
            [
                std::path::PathBuf::from("packages/a"),
                "packages/b/src".into()
            ]
        );
        assert!(plan.files.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn rejects_duplicate_paths() {
        let mut tree = Directory::default();
        tree.insert(&["a".into()], Kind::Symlink).unwrap();
        assert!(tree.insert(&["a".into()], Kind::Symlink).is_err());
    }
}
