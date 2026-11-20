use super::adopt::{files, index};
use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn nested(f: &Fixture) -> Result<PathBuf> {
    let path = f.source.join(".claude/worktrees/agent");
    fs::create_dir_all(path.parent().unwrap())?;
    git(
        &f.source,
        &[
            "worktree",
            "add",
            "-b",
            "feat/nested",
            path.to_str().unwrap(),
            "HEAD",
        ],
    )?;
    Ok(path)
}
/// Twig-owned entries beside checkouts. Adoption may use `.twig/` while it
/// runs, and keeps it only when recovery needs it.
pub(super) fn twig_state(directory: &Path) -> Result<Vec<PathBuf>> {
    Ok(directory
        .read_dir()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().as_encoded_bytes().starts_with(b".twig"))
        .map(|e| e.path())
        .collect())
}
fn apply(cwd: &Path, root: &Path, target: Option<&Path>) -> Result<Output> {
    let mut args = vec!["adopt", "--yes", "--worktrees-dir", root.to_str().unwrap()];
    if let Some(target) = target {
        args.push(target.to_str().unwrap());
    }
    twig(cwd, &args)
}

// Probe the filesystem independently, not by interpreting Twig's success. FUSE
// drivers can support ordinary rename while rejecting RENAME_NOREPLACE. On those
// drivers verify the product's no-write refusal instead of claiming relocation.
fn relocation_available_or_refusal_verified(f: &Fixture) -> Result<bool> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = f;
        Ok(true)
    }
    #[cfg(target_os = "linux")]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let probe = f.root.join("independent-rename-probe");
        fs::create_dir(&probe)?;
        let from = probe.join("from");
        let occupied = probe.join("occupied");
        let vacant = probe.join("vacant");
        fs::create_dir(&from)?;
        fs::create_dir(&occupied)?;
        let rename = |a: &Path, b: &Path| -> std::io::Result<()> {
            let a = CString::new(a.as_os_str().as_bytes()).unwrap();
            let b = CString::new(b.as_os_str().as_bytes()).unwrap();
            // SAFETY: only newly-created fixture probe directories are addressed.
            let result = unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    a.as_ptr(),
                    libc::AT_FDCWD,
                    b.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        };
        let unsupported = |e: &std::io::Error| {
            matches!(
                e.raw_os_error(),
                Some(libc::EINVAL | libc::EOPNOTSUPP | libc::ENOSYS)
            )
        };
        let available = match rename(&from, &occupied) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                match rename(&from, &vacant) {
                    Ok(()) => true,
                    Err(error) if unsupported(&error) => false,
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) if unsupported(&error) => false,
            Err(error) => return Err(error.into()),
            Ok(()) => false, // Ignored exclusive flag is not support.
        };
        if available {
            return Ok(true);
        }
        let before = files(&f.source)?;
        let before_index = index(&f.source)?;
        let registry = git(&f.source, &["worktree", "list", "--porcelain", "-z"])?.stdout;
        let root = f.root.join("unsupported-layout");
        let output = apply(&f.source, &root, None)?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("atomic no-overwrite directory renames are unsupported"),
            "{output:?}"
        );
        assert!(!root.exists());
        assert_eq!(files(&f.source)?, before);
        assert_eq!(index(&f.source)?, before_index);
        assert_eq!(
            git(&f.source, &["worktree", "list", "--porcelain", "-z"])?.stdout,
            registry
        );
        eprintln!(
            "independent filesystem probe lacks exclusive rename; safe adoption refusal verified"
        );
        Ok(false)
    }
}

#[test]
fn previews_and_missing_confirmation_do_not_write_layout_or_config() -> Result<()> {
    let f = Fixture::new()?;
    nested(&f)?;
    let config = fs::read(f.source.join(".git/config"))?;
    let before = files(&f.source)?;
    let before_index = index(&f.source)?;
    let output = checked(f.twig(&["adopt", "--dry-run"])?)?;
    assert!(String::from_utf8_lossy(&output.stdout).contains("->"));
    let root = f.root.join("managed");
    let refused = f.twig(&["adopt", "--worktrees-dir", root.to_str().unwrap()])?;
    assert!(!refused.status.success());
    assert!(!root.exists());
    assert_eq!(config, fs::read(f.source.join(".git/config"))?);
    assert_eq!(before_index, index(&f.source)?);
    assert_eq!(before, files(&f.source)?);
    assert_eq!(twig_state(&f.root)?, Vec::<PathBuf>::new());
    Ok(())
}

#[test]
fn adopts_nested_worktrees_and_main_into_its_own_old_root_without_losing_state() -> Result<()> {
    let f = Fixture::new()?;
    if !relocation_available_or_refusal_verified(&f)? {
        return Ok(());
    }
    let peer = nested(&f)?;
    fs::write(f.source.join("hello.txt"), "staged main\n")?;
    git(&f.source, &["add", "hello.txt"])?;
    fs::write(f.source.join("hello.txt"), "dirty main\n")?;
    fs::create_dir_all(f.source.join("ignored/cache"))?;
    fs::write(f.source.join("ignored/cache/keep"), "ignored main\n")?;
    fs::write(peer.join("hello.txt"), "staged peer\n")?;
    git(&peer, &["add", "hello.txt"])?;
    fs::write(peer.join("hello.txt"), "dirty peer\n")?;
    fs::write(peer.join("untracked"), "private bytes\n")?;
    let main_files: Vec<_> = files(&f.source)?
        .into_iter()
        .filter(|(p, _)| !p.starts_with(".claude/worktrees"))
        .collect();
    let peer_files = files(&peer)?;
    let main_index = index(&f.source)?;
    let peer_index = index(&peer)?;
    let main_inode = file_identity(&f.source)?;
    let output = checked(apply(&peer, &f.source, None)?)?;
    let main = f.source.join("main");
    let target = f.source.join("feat-nested");
    assert_eq!(file_identity(&main)?, main_inode);
    assert_eq!(files(&main)?.into_iter().collect::<Vec<_>>(), main_files);
    assert_eq!(files(&target)?, peer_files);
    assert_eq!(index(&main)?, main_index);
    assert_eq!(index(&target)?, peer_index);
    assert!(!peer.exists());
    assert!(!f.source.join(".git").exists());
    let inventory = git(&main, &["worktree", "list", "--porcelain"])?;
    // Git for Windows lists `C:/a/b`.
    let slashes = |text: &str| text.replace('\\', "/");
    let inventory = slashes(&String::from_utf8(inventory.stdout)?);
    assert!(inventory.contains(&slashes(main.to_str().unwrap())));
    assert!(inventory.contains(&slashes(target.to_str().unwrap())));
    let saved = git(
        &target,
        &["config", "--local", "--get", "twig.worktreesDir"],
    )?;
    assert_eq!(
        String::from_utf8(saved.stdout)?.trim(),
        f.source.to_str().unwrap()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    if cfg!(target_os = "macos") {
        assert!(stdout.contains("sharing will remain unchanged"), "{stdout}");
        // --yes alone only moves on macOS, so no compaction is previewed.
        assert!(!stdout.contains("compaction"), "{stdout}");
    }
    // Each checkout is listed once, in the plan. Only the nested checkout's
    // path names `.claude`; Windows prints it with its own separators.
    assert_eq!(stdout.matches(".claude").count(), 1, "{stdout}");
    // A completed adoption leaves nothing beside the checkouts.
    assert_eq!(twig_state(&f.root)?, Vec::<PathBuf>::new());
    assert_eq!(twig_state(&f.source)?, Vec::<PathBuf>::new());
    // Git still sees the original staged/dirty distinction after its links move.
    let status = git(&target, &["status", "--porcelain"])?;
    assert!(String::from_utf8_lossy(&status.stdout).contains("MM hello.txt"));
    Ok(())
}

#[test]
fn adopts_one_detached_checkout_and_remembers_the_root_for_creation() -> Result<()> {
    let f = Fixture::new()?;
    if !relocation_available_or_refusal_verified(&f)? {
        return Ok(());
    }
    git(
        &f.source,
        &["worktree", "add", "--detach", "../feature", "HEAD"],
    )?;
    fs::write(f.destination().join("untracked"), "preserve me")?;
    let before = files(&f.destination())?;
    let before_index = index(&f.destination())?;
    let root = f.root.join("managed");
    checked(apply(&f.source, &root, Some(&f.destination()))?)?;
    assert!(f.source.join(".git").is_dir());
    assert_eq!(files(&root.join("feature"))?, before);
    assert_eq!(index(&root.join("feature"))?, before_index);
    checked(f.twig(&["new", "-b", "feat/next"])?)?;
    assert!(root.join("feat-next/hello.txt").is_file());
    clean(&root.join("feat-next"))?;
    checked(f.twig(&["adopt", "--yes"])?)?;
    assert!(root.join("main/.git").is_dir());
    assert_eq!(files(&root.join("feature"))?, before);
    clean(&root.join("feat-next"))
}

#[test]
fn rejects_collisions_locks_foreign_targets_and_implicit_descendant_moves() -> Result<()> {
    let f = Fixture::new()?;
    let peer = nested(&f)?;
    let root = f.root.join("managed");
    git(&f.source, &["worktree", "lock", peer.to_str().unwrap()])?;
    assert!(!apply(&f.source, &root, None)?.status.success());
    git(&f.source, &["worktree", "unlock", peer.to_str().unwrap()])?;
    assert!(!apply(&f.source, &root, Some(&f.source))?.status.success());
    let foreign = Fixture::new()?;
    assert!(
        !apply(&f.source, &root, Some(&foreign.source))?
            .status
            .success()
    );
    git(
        &f.source,
        &[
            "worktree",
            "add",
            "-b",
            "feat-nested",
            "../collision",
            "HEAD",
        ],
    )?;
    assert!(!apply(&f.source, &root, None)?.status.success());
    assert!(!root.exists());
    assert!(peer.join("hello.txt").is_file());
    Ok(())
}

#[test]
fn restructures_main_with_existing_main_data_and_an_already_named_nested_slot() -> Result<()> {
    let f = Fixture::new()?;
    if !relocation_available_or_refusal_verified(&f)? {
        return Ok(());
    }
    fs::create_dir(f.source.join("main"))?;
    fs::write(f.source.join("main/keep"), "ordinary existing directory")?;
    let child = f.source.join("feat-one");
    git(
        &f.source,
        &[
            "worktree",
            "add",
            "-b",
            "feat/one",
            child.to_str().unwrap(),
            "HEAD",
        ],
    )?;
    fs::write(child.join("untracked"), "retained")?;
    let before = files(&child)?;
    checked(apply(&f.source, &f.source, None)?)?;
    assert_eq!(
        fs::read_to_string(f.source.join("main/main/keep"))?,
        "ordinary existing directory"
    );
    assert_eq!(files(&child)?, before);
    let status = git(&child, &["status", "--porcelain"])?;
    assert!(String::from_utf8_lossy(&status.stdout).contains("?? untracked"));
    Ok(())
}

#[test]
fn refuses_submodule_entries_before_moving_anything() -> Result<()> {
    let f = Fixture::new()?;
    let head = String::from_utf8(git(&f.source, &["rev-parse", "HEAD"])?.stdout)?;
    git(
        &f.source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},sub", head.trim()),
        ],
    )?;
    let root = f.root.join("managed");
    let output = apply(&f.source, &root, None)?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("submodule"));
    assert!(!root.exists());
    assert!(f.source.join(".git").is_dir());
    Ok(())
}

#[test]
fn refuses_occupied_destinations_and_active_git_locks_without_moving() -> Result<()> {
    let f = Fixture::new()?;
    let peer = nested(&f)?;
    let root = f.root.join("managed");
    fs::create_dir_all(root.join("main"))?;
    fs::write(root.join("main/owner"), "not yours")?;
    let before = files(&f.source)?;
    assert!(!apply(&f.source, &root, None)?.status.success());
    assert_eq!(fs::read(root.join("main/owner"))?, b"not yours");
    assert_eq!(files(&f.source)?, before);
    let other = f.root.join("other");
    fs::write(f.source.join(".git/index.lock"), "active owner")?;
    assert!(!apply(&f.source, &other, Some(&peer))?.status.success());
    assert!(!other.exists());
    assert_eq!(files(&f.source)?, before);
    Ok(())
}

#[test]
fn moving_only_main_repairs_unmoved_external_peers() -> Result<()> {
    let f = Fixture::new()?;
    if !relocation_available_or_refusal_verified(&f)? {
        return Ok(());
    }
    git(
        &f.source,
        &["worktree", "add", "--detach", "../feature", "HEAD"],
    )?;
    let peer = f.destination();
    let before = files(&peer)?;
    let before_index = index(&peer)?;
    checked(apply(&peer, &f.source, Some(&f.source))?)?;
    assert!(f.source.join("main/.git").is_dir());
    assert_eq!(files(&peer)?, before);
    assert_eq!(index(&peer)?, before_index);
    clean(&peer)
}

// The fault is injected by a shell script named git, which Windows will not run
// in place of git.exe. `open_files_block_relocation_and_roll_back` covers
// Windows rollback.
#[cfg(unix)]
#[test]
fn repair_failure_rolls_back_every_move_and_retains_recovery_journal() -> Result<()> {
    let f = Fixture::new()?;
    if !relocation_available_or_refusal_verified(&f)? {
        return Ok(());
    }
    let peer = nested(&f)?;
    let before = files(&f.source)?;
    let before_index = index(&peer)?;
    let wrapper = f.root.join("bin");
    fs::create_dir(&wrapper)?;
    let real_git = checked(
        command("sh", &f.source)
            .args(["-c", "command -v git"])
            .output()?,
    )?;
    let real_git = String::from_utf8(real_git.stdout)?;
    fs::write(
        wrapper.join("git"),
        r#"#!/bin/sh
previous=
for arg do
  if [ "$previous" = worktree ] && [ "$arg" = repair ] && [ ! -e "$TWIG_FAIL_ONCE" ]; then
    : > "$TWIG_FAIL_ONCE"
    echo injected-repair-failure >&2
    exit 91
  fi
  previous="$arg"
done
exec "$TWIG_REAL_GIT" "$@"
"#,
    )?;
    fs::set_permissions(wrapper.join("git"), fs::Permissions::from_mode(0o755))?;
    let mut paths = vec![wrapper];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = command(env!("CARGO_BIN_EXE_git-twig"), &f.source)
        .env("PATH", std::env::join_paths(paths)?)
        .env("TWIG_REAL_GIT", real_git.trim())
        .env("TWIG_FAIL_ONCE", f.root.join("failed-once"))
        .args(["adopt", "--yes", "--worktrees-dir"])
        .arg(&f.source)
        .output()?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("original layout was restored"),
        "{output:?}"
    );
    assert_eq!(files(&f.source)?, before);
    assert_eq!(index(&peer)?, before_index);
    git(&peer, &["status", "--porcelain"])?;
    // Restored layouts need no recovery, so no journal is kept.
    assert_eq!(twig_state(&f.root)?, Vec::<PathBuf>::new());
    Ok(())
}

/// Windows refuses to move a directory holding an open file. A file held open
/// in main makes its move fail after a child checkout has already moved: every
/// move must be undone, and the journal kept.
#[cfg(windows)]
#[test]
fn open_files_block_relocation_and_roll_back() -> Result<()> {
    let f = Fixture::new()?;
    let peer = nested(&f)?;
    fs::write(peer.join("untracked"), "private bytes\n")?;
    let before = files(&f.source)?;
    let before_index = index(&peer)?;
    // Rust shares delete access by default, which would allow the move.
    let held = {
        use std::os::windows::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ)
            .open(f.source.join("hello.txt"))?
    };
    let root = f.root.join("managed");
    let output = apply(&f.source, &root, None)?;
    drop(held);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("original layout was restored"),
        "{output:?}"
    );
    assert_eq!(files(&f.source)?, before);
    assert_eq!(index(&peer)?, before_index);
    git(&peer, &["status", "--porcelain"])?;
    // Restored layouts need no recovery, so no journal is kept.
    assert_eq!(twig_state(&f.root)?, Vec::<PathBuf>::new());
    Ok(())
}

/// `.twig/` beside main holds parked checkouts while they move, so a link or
/// a folder others can write to is refused before anything moves, and a
/// journal kept by an earlier failure is left alone.
#[cfg(unix)]
#[test]
fn the_twig_folder_must_be_private_and_keeps_other_journals() -> Result<()> {
    let f = Fixture::new()?;
    if !relocation_available_or_refusal_verified(&f)? {
        return Ok(());
    }
    nested(&f)?;
    let before = files(&f.source)?;
    let root = f.root.join("managed");
    let folder = f.root.join(".twig");
    let elsewhere = f.root.join("elsewhere");
    fs::create_dir(&elsewhere)?;
    std::os::unix::fs::symlink(&elsewhere, &folder)?;
    let output = apply(&f.source, &root, None)?;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not a directory"),
        "{output:?}"
    );
    assert_eq!(fs::read_dir(&elsewhere)?.count(), 0);
    fs::remove_file(&folder)?;

    fs::create_dir(&folder)?;
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o770))?;
    let output = apply(&f.source, &root, None)?;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("writable by no one else"),
        "{output:?}"
    );
    assert_eq!(files(&f.source)?, before);
    assert!(!root.exists());

    fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
    let kept = folder.join("adopt-1-1");
    fs::create_dir(&kept)?;
    fs::write(kept.join("journal.nul"), "earlier failure")?;
    checked(apply(&f.source, &root, None)?)?;
    assert!(root.join("main/.git").is_dir());
    assert_eq!(fs::read_dir(&folder)?.count(), 1);
    assert_eq!(fs::read(kept.join("journal.nul"))?, b"earlier failure");
    Ok(())
}
