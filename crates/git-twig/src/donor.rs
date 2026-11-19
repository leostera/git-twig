//! Bounded donor discovery. Tree identity nominates candidates; raw bytes certify them.
use crate::{
    checked, git, native,
    tree::{self, Entry, Kind, ObjectId},
};
use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const CANDIDATE_LIMIT: usize = 5;

pub struct Donor {
    pub root: PathBuf,
    pub candidates: Vec<Entry>,
}

#[derive(Debug)]
struct Worktree {
    root: PathBuf,
    head: Option<ObjectId>,
    usable: bool,
}

/// Git's NUL porcelain keeps newlines, quoting characters and non-UTF8 paths literal.
fn parse(bytes: &[u8]) -> Result<Vec<Worktree>> {
    ensure!(
        bytes.is_empty() || bytes.last() == Some(&0),
        "unterminated worktree listing"
    );
    let mut trees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in bytes.split(|b| *b == 0) {
        if let Some(path) = field.strip_prefix(b"worktree ") {
            ensure!(
                current.is_none() && !path.is_empty(),
                "invalid worktree record"
            );
            let root = native::git_path(path.to_vec());
            ensure!(root.is_absolute(), "relative path in worktree listing");
            current = Some(Worktree {
                root,
                head: None,
                usable: true,
            });
        } else if field.is_empty() {
            if let Some(tree) = current.take() {
                trees.push(tree);
            }
        } else {
            let tree = current.as_mut().context("worktree field before path")?;
            if let Some(head) = field.strip_prefix(b"HEAD ") {
                ensure!(tree.head.is_none(), "duplicate worktree HEAD");
                tree.head = Some(ObjectId::parse(std::str::from_utf8(head)?)?);
            } else if field == b"bare" || field == b"prunable" || field.starts_with(b"prunable ") {
                tree.usable = false;
            }
        }
    }
    // A single trailing NUL may terminate the last field without a blank record.
    if let Some(tree) = current {
        trees.push(tree);
    }
    Ok(trees)
}

pub fn choose(root: &Path, parent: &Path, target: &[Entry]) -> Result<Donor> {
    let listing = checked(git(root).args(["worktree", "list", "--porcelain", "-z"]))?.stdout;
    let mut worktrees = parse(&listing)?;
    // Git derives the main checkout's path from its Git directory, so a separated
    // one is listed where no checkout exists. The checkout Twig runs in is real.
    worktrees.retain(|tree| fs::symlink_metadata(tree.root.join(".git")).is_ok());
    if !worktrees.iter().any(|tree| tree.root == root) {
        let head = checked(git(root).args(["rev-parse", "--verify", "HEAD^{commit}"]))?.stdout;
        worktrees.push(Worktree {
            root: root.to_path_buf(),
            head: Some(ObjectId::parse(std::str::from_utf8(&head)?.trim_end())?),
            usable: true,
        });
    }
    worktrees.sort_by_key(|worktree| worktree.root != root);
    let clonable = target
        .iter()
        .filter(|entry| entry.kind != Kind::Submodule)
        .count();
    let mut best: Option<Donor> = None;
    for worktree in worktrees
        .into_iter()
        .filter(|tree| tree.usable && tree.head.is_some())
        .filter(|tree| same_filesystem(&tree.root, parent))
        .take(CANDIDATE_LIMIT)
    {
        let head = worktree.head.expect("eligible worktree has HEAD");
        let inventory = checked(git(root).args(["ls-tree", "-rz", "--full-tree", head.as_str()]))
            .and_then(|out| tree::parse(&out.stdout));
        let Ok(inventory) = inventory else { continue };
        let source: BTreeMap<_, _> = inventory.iter().map(|entry| (&entry.path, entry)).collect();
        let candidates = target
            .iter()
            .filter(|entry| {
                source.get(&entry.path).is_some_and(|source| {
                    source.object == entry.object && compatible(source.kind, entry.kind)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if best
            .as_ref()
            .is_none_or(|old| candidates.len() > old.candidates.len())
        {
            let complete = candidates.len() == clonable;
            best = Some(Donor {
                root: worktree.root,
                candidates,
            });
            // No later donor can beat all clonable paths; ties prefer this one.
            if complete {
                break;
            }
        }
    }
    let best = best.context("no usable same-filesystem donor worktree")?;
    ensure!(
        target.is_empty() || !best.candidates.is_empty(),
        "no matching donor blobs for target revision"
    );
    Ok(best)
}

fn compatible(source: Kind, target: Kind) -> bool {
    matches!(
        (source, target),
        (Kind::File { .. }, Kind::File { .. }) | (Kind::Symlink, Kind::Symlink)
    )
}

#[cfg(unix)]
fn same_filesystem(root: &Path, parent: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(root)
        .ok()
        .zip(fs::metadata(parent).ok())
        .is_some_and(|(source, target)| source.is_dir() && source.dev() == target.dev())
}

#[cfg(windows)]
fn same_filesystem(root: &Path, parent: &Path) -> bool {
    fs::symlink_metadata(root).is_ok_and(|metadata| metadata.is_dir())
        && crate::windows::volume_identity(root)
            .ok()
            .zip(crate::windows::volume_identity(parent).ok())
            .is_some_and(|(source, target)| source == target)
}

#[cfg(not(any(unix, windows)))]
fn same_filesystem(_: &Path, _: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_raw_paths_and_eligibility() -> Result<()> {
        // Git for Windows lists absolute paths with a drive and forward slashes.
        let drive = if cfg!(windows) { "C:" } else { "" };
        let mut bytes = format!("worktree {drive}/repo/odd\n\t\"").into_bytes();
        #[cfg(unix)]
        bytes.push(0xff);
        bytes.extend_from_slice(
            format!(
                "\0HEAD {}\0locked user lock\0\0worktree {drive}/gone\0HEAD {}\0prunable gone\0\0",
                "a".repeat(40),
                "b".repeat(64)
            )
            .as_bytes(),
        );
        let trees = parse(&bytes)?;
        assert_eq!(trees.len(), 2);
        assert!(trees[0].usable);
        assert!(!trees[1].usable);
        assert_eq!(
            trees[0].root,
            native::git_path(bytes[9..bytes.iter().position(|b| *b == 0).unwrap()].to_vec())
        );
        Ok(())
    }
    #[test]
    fn rejects_malformed_records() {
        for bytes in [
            b"HEAD abc\0".as_slice(),
            b"worktree relative\0",
            b"worktree /repo",
            b"worktree /repo\0HEAD abc\0",
        ] {
            assert!(parse(bytes).is_err());
        }
    }
    #[test]
    fn regular_modes_may_differ_but_types_may_not() {
        assert!(compatible(
            Kind::File { executable: false },
            Kind::File { executable: true }
        ));
        assert!(!compatible(Kind::File { executable: false }, Kind::Symlink));
        assert!(!compatible(Kind::Submodule, Kind::Submodule));
    }
}
