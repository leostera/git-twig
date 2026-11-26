#![cfg_attr(not(unix), allow(unused_imports))]
use super::*;

#[cfg(unix)]
#[test]
fn policy_changes_before_and_after_registration_fail_safely() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for boundary in ["checkout-index", "--no-checkout"] {
        let f = Fixture::new()?;
        #[cfg(target_os = "linux")]
        if !linux_reflinks_available(&f)? {
            continue;
        }
        fs::write(f.source.join(".gitattributes"), "hello.txt text eol=crlf\n")?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "policy race"])?;
        let bin = f.root.join("bin");
        fs::create_dir(&bin)?;
        let wrapper = bin.join("git");
        fs::write(
            &wrapper,
            r#"#!/bin/sh
"$TWIG_TEST_REAL_GIT" "$@"
status=$?
if [ "$status" = 0 ]; then
  for arg in "$@"; do
    if [ "$arg" = "$TWIG_TEST_BOUNDARY" ]; then
      printf 'hello.txt text eol=lf\n' > "$TWIG_TEST_ATTRIBUTES"
      printf 'changed\n' >> "$TWIG_TEST_MARKER"
    fi
  done
fi
exit "$status"
"#,
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
        let real_git = String::from_utf8(
            checked(
                command("sh", &f.source)
                    .args(["-c", "command -v git"])
                    .output()?,
            )?
            .stdout,
        )?;
        let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))?;
        let mut child = command(env!("CARGO_BIN_EXE_git-twig"), &f.source);
        child
            .args(["new", "-b", "feature", "../feature", "HEAD"])
            .env("PATH", path)
            .env("TWIG_TEST_REAL_GIT", real_git.trim())
            .env("TWIG_TEST_BOUNDARY", boundary)
            .env(
                "TWIG_TEST_ATTRIBUTES",
                f.source.join(".git/info/attributes"),
            )
            .env("TWIG_TEST_MARKER", f.root.join("marker"));
        let output = bounded_output(child)?;
        assert_eq!(fs::read_to_string(f.root.join("marker"))?, "changed\n");
        assert_eq!(fs::read(f.source.join("hello.txt"))?, b"hello\n");
        if boundary == "checkout-index" {
            checked(output)?;
            // Pre-registration policy change delegates once under the new policy.
            assert_eq!(fs::read(f.destination().join("hello.txt"))?, b"hello\n");
            assert!(f.stages()?.is_empty());
            clean(&f.destination())?;
        } else {
            assert!(!output.status.success());
            let stderr = String::from_utf8(output.stderr)?;
            assert!(stderr.contains("partial worktree retained"), "{stderr}");
            assert!(
                stderr.contains("checkout configuration or attributes changed"),
                "{stderr}"
            );
            assert!(f.destination().join(".git").is_file());
            assert!(!f.destination().join("hello.txt").exists());
            assert_eq!(f.stages()?.len(), 1);
        }
        assert_eq!(
            git(&f.source, &["worktree", "list", "--porcelain"])?
                .stdout
                .split(|b| *b == b'\n')
                .filter(|line| line.starts_with(b"worktree "))
                .count(),
            2
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn dirty_attribute_filters_are_never_executed_during_native_preparation() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.source.join(".gitattributes"), "* text=auto eol=lf\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "target policy"])?;
    fs::write(f.source.join(".gitattributes"), "* filter=unexpected\n")?;
    fs::write(f.source.join("hello.txt"), "dirty\n")?;
    git(
        &f.source,
        &[
            "config",
            "filter.unexpected.smudge",
            "echo invoked > unexpected-filter; cat",
        ],
    )?;
    let output = checked(f.add()?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, b"hello\n");
    assert_eq!(
        fs::read(f.destination().join(".gitattributes"))?,
        b"* text=auto eol=lf\n"
    );
    assert!(!f.source.join("unexpected-filter").exists());
    assert!(!f.destination().join("unexpected-filter").exists());
    assert!(f.stages()?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn writable_mapped_donor_edits_are_not_inherited() -> Result<()> {
    use std::os::fd::AsRawFd;
    let f = Fixture::new()?;
    let path = f.source.join("mapped");
    fs::write(&path, vec![b'a'; 65536])?;
    let file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
    // SAFETY: live regular-file fd with length 65536; mapping is used only in this
    // test thread, within bounds, and unmapped after the fallible test body returns.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            65536,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    let result = (|| -> Result<()> {
        unsafe {
            *(mapping as *mut u8) = b'b';
        }
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "mapped committed bytes"])?;
        // A second write through an already-dirty mapping can evade stat caches.
        unsafe {
            *(mapping as *mut u8) = b'c';
        }
        let output = checked(f.add()?)?;
        native(&output);
        assert_eq!(fs::read(&path)?[0], b'c');
        assert_eq!(fs::read(f.destination().join("mapped"))?[0], b'b');
        clean(&f.destination())
    })();
    assert_eq!(unsafe { libc::munmap(mapping, 65536) }, 0);
    result
}

/// Git's hash-object fails like the read error seen on macOS disk images
/// ("read error while indexing <unknown>: Unknown error: 999"), either once
/// or every time. Returns the create's output and how often it failed.
#[cfg(unix)]
fn create_with_failing_hashes(f: &Fixture, failures: &str) -> Result<(Output, usize)> {
    use std::os::unix::fs::PermissionsExt;
    let bin = f.root.join("bin");
    fs::create_dir(&bin)?;
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        r#"#!/bin/sh
for arg in "$@"; do
  if [ "$arg" = hash-object ]; then
    if [ "$TWIG_TEST_FAILURES" = always ] || [ ! -e "$TWIG_TEST_MARKER" ]; then
      printf 'failed\n' >> "$TWIG_TEST_MARKER"
      cat > /dev/null
      echo 'error: read error while indexing <unknown>: Unknown error: 999' >&2
      exit 128
    fi
  fi
done
exec "$TWIG_TEST_REAL_GIT" "$@"
"#,
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
    let real_git = String::from_utf8(
        checked(
            command("sh", &f.source)
                .args(["-c", "command -v git"])
                .output()?,
        )?
        .stdout,
    )?;
    let path = std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))?;
    let marker = f.root.join("marker");
    let mut child = command(env!("CARGO_BIN_EXE_git-twig"), &f.source);
    child
        .args(["new", "-b", "feature", "../feature", "HEAD"])
        .env("PATH", path)
        .env("TWIG_TEST_REAL_GIT", real_git.trim())
        .env("TWIG_TEST_FAILURES", failures)
        .env("TWIG_TEST_MARKER", &marker);
    let output = bounded_output(child)?;
    let failed = fs::read_to_string(&marker)
        .unwrap_or_default()
        .lines()
        .count();
    Ok((output, failed))
}

#[cfg(unix)]
#[test]
fn a_transient_hash_read_error_keeps_native_sharing() -> Result<()> {
    let f = Fixture::new()?;
    if !cow_available(&f)? {
        return Ok(());
    }
    let (output, failed) = create_with_failing_hashes(&f, "once")?;
    let output = checked(output)?;
    assert_eq!(failed, 1);
    native(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("retrying"), "{stderr}");
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, b"hello\n");
    assert!(f.stages()?.is_empty());
    clean(&f.destination())
}

#[cfg(unix)]
#[test]
fn a_persistent_hash_read_error_delegates_to_git() -> Result<()> {
    let f = Fixture::new()?;
    if !cow_available(&f)? {
        return Ok(());
    }
    let (output, failed) = create_with_failing_hashes(&f, "always")?;
    let output = checked(output)?;
    // One attempt and one retry, then stock Git writes every file.
    assert_eq!(failed, 2);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("stock Git:"), "{stderr}");
    assert!(stderr.contains("Unknown error: 999"), "{stderr}");
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, b"hello\n");
    clean(&f.destination())
}
