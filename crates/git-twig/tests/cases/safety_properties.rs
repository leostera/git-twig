//! Differential properties: raw bytes and file types are the oracle, not status.
use super::*;
use std::{
    collections::BTreeMap,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
};

#[derive(Debug, PartialEq, Eq)]
enum Entry {
    Directory,
    File { bytes: Vec<u8>, executable: bool },
    Link(PathBuf),
}

fn inventory(root: &Path) -> Result<BTreeMap<PathBuf, Entry>> {
    fn visit(root: &Path, directory: &Path, entries: &mut BTreeMap<PathBuf, Entry>) -> Result<()> {
        for child in fs::read_dir(directory)? {
            let path = child?.path();
            let relative = path.strip_prefix(root)?.to_path_buf();
            if path.file_name() == Some(std::ffi::OsStr::new(".git")) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            let entry = if metadata.is_symlink() {
                Entry::Link(fs::read_link(&path)?)
            } else if metadata.is_dir() {
                visit(root, &path, entries)?;
                Entry::Directory
            } else {
                ensure!(
                    metadata.is_file(),
                    "unexpected special file: {}",
                    path.display()
                );
                Entry::File {
                    bytes: fs::read(&path)?,
                    executable: metadata.mode() & 0o111 != 0,
                }
            };
            entries.insert(relative, entry);
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

#[test]
fn generated_dirty_donors_match_stock_and_keep_private_writes_isolated() -> Result<()> {
    for format in ["sha1", "sha256"] {
        for seed in 0..6u64 {
            let f = Fixture::with_object_format(format)?;
            // Deterministic generation makes the fixture and failed seed reproducible.
            eprintln!("safety property format={format} seed={seed}");
            fs::create_dir_all(f.source.join("tree/deep"))?;
            fs::write(
                f.source.join(".gitattributes"),
                if seed % 2 == 0 {
                    "*.txt text eol=lf\n"
                } else {
                    "*.txt text eol=crlf\n"
                },
            )?;
            fs::write(f.source.join(".gitignore"), "node_modules/\nignored/\n")?;
            for index in 0..12u64 {
                let name = format!("tree/deep/file {index}.txt");
                let bytes = format!("seed={seed} index={index}\nsecond line\n");
                fs::write(f.source.join(&name), bytes)?;
                if index % 3 == 0 {
                    fs::set_permissions(f.source.join(&name), fs::Permissions::from_mode(0o755))?;
                }
            }
            fs::write(f.source.join("tree/é-雪\tnewline\n.bin"), [0, 255, 13, 10])?;
            symlink("deep/file 0.txt", f.source.join("tree/link"))?;
            symlink("missing", f.source.join("dangling"))?;
            git(&f.source, &["add", "."])?;
            git(&f.source, &["commit", "-m", "generated target"])?;
            git(
                &f.source,
                &["worktree", "add", "--detach", "../stock", "HEAD"],
            )?;
            // Stage some changes as well: donor index is not the target oracle.
            for index in 0..12u64 {
                let path = f.source.join(format!("tree/deep/file {index}.txt"));
                match (index + seed) % 4 {
                    0 => fs::write(&path, b"dirty\r\n")?,
                    1 if seed % 2 == 0 => {
                        fs::rename(&path, f.root.join(format!("retained-{index}")))?;
                    }
                    2 => fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?,
                    _ => {}
                }
            }
            git(&f.source, &["add", "-u"])?;
            for directory in [
                "node_modules",
                "tree/node_modules",
                "tree/deep/node_modules",
            ] {
                fs::create_dir_all(f.source.join(directory))?;
                for index in 0..32 {
                    fs::write(
                        f.source.join(directory).join(format!("ignored-{index}")),
                        b"private build output",
                    )?;
                }
                symlink(&f.root, f.source.join(directory).join("outside"))?;
            }
            let donor = inventory(&f.source)?;
            let index_before = fs::read(f.source.join(".git/index"))?;
            let output = checked(f.add()?)?;
            // Even seeds also move tracked files away: Git writes those paths.
            native(&output);
            let expected = inventory(&f.root.join("stock"))?;
            assert_eq!(
                inventory(&f.destination())?,
                expected,
                "seed={seed} format={format}"
            );
            assert_eq!(inventory(&f.source)?, donor);
            assert_eq!(fs::read(f.source.join(".git/index"))?, index_before);
            for (relative, entry) in &expected {
                if !matches!(entry, Entry::File { .. }) {
                    continue;
                }
                let private = f.destination().join(relative);
                let metadata = fs::metadata(&private)?;
                if let Ok(source) = fs::symlink_metadata(f.source.join(relative)) {
                    assert_ne!(
                        (metadata.dev(), metadata.ino()),
                        (source.dev(), source.ino())
                    );
                }
                fs::write(private, b"private mutation")?;
            }
            assert_eq!(inventory(&f.source)?, donor);
            assert_eq!(inventory(&f.root.join("stock"))?, expected);
            assert!(f.stages()?.is_empty());
        }
    }
    Ok(())
}

#[test]
fn registration_failure_and_abrupt_exit_preserve_recoverable_state() -> Result<()> {
    for action in [
        "exit 91",
        "kill -KILL \"$PPID\"; exit 92",
        "touch \"$TWIG_TEST_LATE_FAILURE\"",
    ] {
        let f = Fixture::new()?;
        #[cfg(target_os = "linux")]
        if !linux_reflinks_available(&f)? {
            continue;
        }
        let before = inventory(&f.source)?;
        let output = after_registration(&f, action)?;
        assert!(!output.status.success());
        assert_eq!(fs::read(f.root.join("marker"))?, b"registered\n");
        assert_eq!(inventory(&f.source)?, before);
        assert!(f.destination().join(".git").is_file());
        let stages = f.stages()?;
        assert_eq!(stages.len(), 1);
        let retained = inventory(&stages[0])?;
        assert!(!retained.is_empty());
        let partial = inventory(&f.destination())?;
        if action.contains("LATE_FAILURE") {
            assert_eq!(partial, before);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("injected index installation failure"),
                "{stderr}"
            );
            assert!(stderr.contains("partial worktree retained"), "{stderr}");
        } else {
            assert!(partial.is_empty());
        }
        assert!(
            !f.twig(&["new", "--detach", "../feature", "HEAD"])?
                .status
                .success()
        );
        assert_eq!(inventory(&f.destination())?, partial);
        checked(f.twig(&["new", "--detach", "../retry", "HEAD"])?)?;
        assert_eq!(inventory(&f.root.join("retry"))?, before);
        assert_eq!(inventory(&stages[0])?, retained);
        assert_eq!(f.stages()?, stages);
        assert_eq!(inventory(&f.source)?, before);
        assert_eq!(registration_count(&f)?, 3);
    }
    Ok(())
}

#[test]
fn interrupts_before_registration_remove_staging() -> Result<()> {
    use std::os::unix::process::ExitStatusExt;
    for (action, signal) in [
        (r#"kill -INT "$PPID""#, libc::SIGINT),
        (r#"kill -TERM "$PPID""#, libc::SIGTERM),
        // A terminal interrupt reaches the whole process group, including Git.
        // The send runs in a subshell ignoring SIGINT: on macOS a sender that
        // dies of its own kill(0) can stop delivery to the rest of the group.
        // Git then fails the way an interrupted Git does.
        ("(trap '' INT; kill -INT 0); exit 130", libc::SIGINT),
    ] {
        let f = Fixture::new()?;
        #[cfg(target_os = "linux")]
        if !linux_reflinks_available(&f)? {
            continue;
        }
        let before = inventory(&f.source)?;
        let output = during_verification(&f, action)?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.signal(), Some(signal), "{action}: {stderr}");
        assert!(f.root.join("interrupted").is_dir(), "{action}: {stderr}");
        assert!(!stderr.contains("stock Git"), "{action}: {stderr}");
        assert!(f.stages()?.is_empty(), "{action}: {stderr}");
        f.assert_no_sibling_stages()?;
        assert!(fs::symlink_metadata(f.destination()).is_err());
        assert_eq!(registration_count(&f)?, 1);
        assert_eq!(inventory(&f.source)?, before);
    }
    Ok(())
}

/// Signal Twig once, while Git hashes private clones before registration.
fn during_verification(f: &Fixture, action: &str) -> Result<Output> {
    wrapped_twig(
        f,
        &format!(
            r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = "hash-object" ] && mkdir "$TWIG_TEST_INTERRUPTED" 2>/dev/null; then
    {action}
  fi
done
exec "$TWIG_TEST_REAL_GIT" "$@"
"#
        ),
        &["new", "--detach", "../feature", "HEAD"],
    )
}

fn after_registration(f: &Fixture, action: &str) -> Result<Output> {
    wrapped_twig(
        f,
        &format!(
            r#"#!/bin/sh
if [ -f "$TWIG_TEST_LATE_FAILURE" ]; then
  for arg in "$@"; do
    if [ "$arg" = "update-index" ]; then
      printf 'injected index installation failure\n' >&2
      exit 93
    fi
  done
fi
"$TWIG_TEST_REAL_GIT" "$@"
status=$?
if [ "$status" = 0 ]; then
  for arg in "$@"; do
    if [ "$arg" = "--no-checkout" ]; then
      printf 'registered\n' > "$TWIG_TEST_MARKER"
      {action}
    fi
  done
fi
exit "$status"
"#
        ),
        &["new", "--detach", "../feature", "HEAD"],
    )
}

/// Run Twig with `script` installed as the `git` found first on PATH.
fn wrapped_twig(f: &Fixture, script: &str, args: &[&str]) -> Result<Output> {
    let bin = f.root.join("bin");
    fs::create_dir(&bin)?;
    let wrapper = bin.join("git");
    fs::write(&wrapper, script)?;
    fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755))?;
    let real_git = String::from_utf8(git_path(&f.source)?)?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    let mut child = command(env!("CARGO_BIN_EXE_git-twig"), &f.source);
    child
        .args(args)
        .env("PATH", path)
        .env("TWIG_TEST_REAL_GIT", real_git.trim())
        .env("TWIG_TEST_MARKER", f.root.join("marker"))
        .env("TWIG_TEST_LATE_FAILURE", f.root.join("late-failure"))
        .env("TWIG_TEST_INTERRUPTED", f.root.join("interrupted"))
        .env("TWIG_TEST_SOURCE", &f.source)
        .env("TWIG_TEST_DESTINATION", f.destination());
    bounded_output(child)
}

#[test]
fn registration_races_never_install_unverified_or_overwrite_foreign_data() -> Result<()> {
    for scenario in [
        "donor-write",
        "destination-file",
        "destination-symlink",
        "head-move",
    ] {
        let f = Fixture::new()?;
        #[cfg(target_os = "linux")]
        if !linux_reflinks_available(&f)? {
            continue;
        }
        git(
            &f.source,
            &["commit", "--allow-empty", "-m", "second commit"],
        )?;
        let before = inventory(&f.source)?;
        let target = git(&f.source, &["rev-parse", "HEAD"])?.stdout;
        let action = match scenario {
            "donor-write" => r#"printf 'concurrent donor edit\n' > "$TWIG_TEST_SOURCE/hello.txt""#,
            "destination-file" => r#"printf 'foreign data\n' > "$TWIG_TEST_DESTINATION/hello.txt""#,
            "destination-symlink" => {
                r#"ln -s "$TWIG_TEST_SOURCE/hello.txt" "$TWIG_TEST_DESTINATION/hello.txt""#
            }
            "head-move" => {
                r#""$TWIG_TEST_REAL_GIT" -C "$TWIG_TEST_DESTINATION" update-ref HEAD HEAD^"#
            }
            _ => unreachable!(),
        };
        let output = after_registration(&f, action)?;
        assert_eq!(fs::read(f.root.join("marker"))?, b"registered\n");
        assert_eq!(registration_count(&f)?, 2);
        if scenario == "donor-write" {
            checked(output)?;
            assert_eq!(inventory(&f.destination())?, before);
            let mut expected = before;
            expected.insert(
                PathBuf::from("hello.txt"),
                Entry::File {
                    bytes: b"concurrent donor edit\n".to_vec(),
                    executable: false,
                },
            );
            assert_eq!(inventory(&f.source)?, expected);
            assert!(f.stages()?.is_empty());
            continue;
        }
        assert!(!output.status.success(), "scenario={scenario}");
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("partial worktree retained"), "{stderr}");
        assert_eq!(inventory(&f.source)?, before);
        assert_eq!(git(&f.source, &["rev-parse", "HEAD"])?.stdout, target);
        assert_eq!(f.stages()?.len(), 1);
        match scenario {
            "destination-file" => assert_eq!(
                fs::read(f.destination().join("hello.txt"))?,
                b"foreign data\n"
            ),
            "destination-symlink" => assert_eq!(
                fs::read_link(f.destination().join("hello.txt"))?,
                f.source.join("hello.txt")
            ),
            "head-move" => {
                assert!(inventory(&f.destination())?.is_empty());
                assert_eq!(
                    git(&f.destination(), &["rev-parse", "HEAD"])?.stdout,
                    git(&f.source, &["rev-parse", "HEAD^"])?.stdout
                );
            }
            _ => unreachable!(),
        }
        let retained = inventory(&f.stages()?[0])?;
        let partial = inventory(&f.destination())?;
        checked(f.twig(&["new", "--detach", "../retry", "HEAD"])?)?;
        assert_eq!(inventory(&f.root.join("retry"))?, before);
        assert_eq!(inventory(&f.destination())?, partial);
        assert_eq!(inventory(&f.stages()?[0])?, retained);
    }
    Ok(())
}

#[test]
fn revisions_moved_during_preparation_register_verified_commits_or_delegate() -> Result<()> {
    // Move the source branch while preparation hashes staged clones. Hashing may run in
    // parallel batches, so mkdir makes the move happen once.
    let script = r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = "hash-object" ] && mkdir "$TWIG_TEST_MARKER" 2>/dev/null; then
    "$TWIG_TEST_REAL_GIT" -C "$TWIG_TEST_SOURCE" update-ref HEAD HEAD^ || exit 94
  fi
done
exec "$TWIG_TEST_REAL_GIT" "$@"
"#;
    for detach in [true, false] {
        let f = Fixture::new()?;
        #[cfg(target_os = "linux")]
        if !linux_reflinks_available(&f)? {
            continue;
        }
        git(
            &f.source,
            &["commit", "--allow-empty", "-m", "second commit"],
        )?;
        let before = inventory(&f.source)?;
        let prepared = git(&f.source, &["rev-parse", "HEAD"])?.stdout;
        let moved = git(&f.source, &["rev-parse", "HEAD^"])?.stdout;
        let args: &[&str] = if detach {
            &["new", "--detach", "../feature", "HEAD"]
        } else {
            &["new", "-b", "feature", "../feature", "HEAD"]
        };
        let output = checked(wrapped_twig(&f, script, args)?)?;
        assert_eq!(git(&f.source, &["rev-parse", "HEAD"])?.stdout, moved);
        let expected = if detach {
            native(&output);
            prepared
        } else {
            // A new branch keeps Git's start-point semantics, so stock Git creates it.
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("stock Git: requested revision moved"),
                "{stderr}"
            );
            assert!(!stderr.contains("CoW: verified tracked files"), "{stderr}");
            moved
        };
        assert_eq!(
            git(&f.destination(), &["rev-parse", "HEAD"])?.stdout,
            expected,
            "detach={detach}"
        );
        assert!(f.stages()?.is_empty());
        assert_eq!(registration_count(&f)?, 2);
        clean(&f.destination())?;
        assert_eq!(inventory(&f.source)?, before);
    }
    Ok(())
}

fn registration_count(f: &Fixture) -> Result<usize> {
    Ok(git(&f.source, &["worktree", "list", "--porcelain"])?
        .stdout
        .split(|b| *b == b'\n')
        .filter(|line| line.starts_with(b"worktree "))
        .count())
}

#[test]
fn concurrent_creates_have_one_owner_per_destination() -> Result<()> {
    use std::sync::{Arc, Barrier};
    for shared_destination in [false, true] {
        let f = Fixture::new()?;
        let donor = inventory(&f.source)?;
        let barrier = Arc::new(Barrier::new(4));
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|index| {
                    let source = &f.source;
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || -> Result<(String, Output)> {
                        let destination = format!(
                            "../concurrent-{}",
                            if shared_destination { 0 } else { index }
                        );
                        let mut child = command(env!("CARGO_BIN_EXE_git-twig"), source);
                        child.args(["new", "--detach", &destination, "HEAD"]);
                        barrier.wait();
                        Ok((destination, bounded_output(child)?))
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("worker panicked"))
                .collect::<Result<Vec<_>>>()
        })?;
        let expected_successes = if shared_destination { 1 } else { 4 };
        assert_eq!(
            results
                .iter()
                .filter(|(_, result)| result.status.success())
                .count(),
            expected_successes
        );
        for (destination, result) in &results {
            if !result.status.success() {
                continue;
            }
            assert_eq!(inventory(&f.source.join(destination))?, donor);
            fs::write(f.source.join(destination).join("hello.txt"), destination)?;
        }
        assert_eq!(inventory(&f.source)?, donor);
        let registrations = git(&f.source, &["worktree", "list", "--porcelain"])?;
        assert_eq!(
            registrations
                .stdout
                .split(|b| *b == b'\n')
                .filter(|line| line.starts_with(b"worktree "))
                .count(),
            expected_successes + 1
        );
        // Failed contenders may retain their own scratch; success must never
        // mistake a peer's scratch for its own. Every surviving stage is private.
        for stage in f.stages()? {
            assert_eq!(fs::metadata(stage)?.mode() & 0o777, 0o700);
        }
    }
    Ok(())
}

#[test]
fn existing_destinations_and_symlink_targets_are_never_overwritten() -> Result<()> {
    for kind in ["directory", "file", "symlink", "dangling"] {
        let f = Fixture::new()?;
        let outside = f.root.join("outside");
        fs::create_dir(&outside)?;
        fs::write(outside.join("sentinel"), b"keep me")?;
        match kind {
            "directory" => {
                fs::create_dir(f.destination())?;
                fs::write(f.destination().join("sentinel"), b"keep me")?;
            }
            "file" => fs::write(f.destination(), b"keep me")?,
            "symlink" => symlink(&outside, f.destination())?,
            "dangling" => symlink(f.root.join("missing"), f.destination())?,
            _ => unreachable!(),
        }
        let before = inventory(&f.root)?;
        let registrations = git(&f.source, &["worktree", "list", "--porcelain"])?.stdout;
        assert!(!f.add()?.status.success(), "kind={kind}");
        // Ignore Git metadata changes when comparing user files.
        assert_eq!(
            inventory(&outside)?,
            BTreeMap::from([(
                PathBuf::from("sentinel"),
                Entry::File {
                    bytes: b"keep me".to_vec(),
                    executable: false
                }
            )])
        );
        assert_eq!(inventory(&f.root)?, before, "kind={kind}");
        assert_eq!(
            git(&f.source, &["worktree", "list", "--porcelain"])?.stdout,
            registrations
        );
    }
    Ok(())
}

fn git_path(cwd: &Path) -> Result<Vec<u8>> {
    Ok(checked(command("sh", cwd).args(["-c", "command -v git"]).output()?)?.stdout)
}
