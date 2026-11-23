//! Fallbacks must explain their concrete boundary without changing Git's outcome.
use super::*;

fn reason(output: &Output, expected: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("git-twig: stock Git:"), "{stderr}");
    assert!(stderr.contains(expected), "{stderr}");
    assert!(!stderr.contains("outside the CoW fast path"), "{stderr}");
    assert!(!stderr.contains("CoW: verified tracked files"), "{stderr}");
}

#[test]
fn attributes_name_the_path_key_and_literal_value() -> Result<()> {
    for value in ["unset", "unspecified", "upper"] {
        let f = Fixture::new()?;
        if !cow_available(&f)? {
            return Ok(()); // An unsupported volume is the first reason given.
        }
        fs::write(
            f.source.join(".gitattributes"),
            format!("hello.txt filter={value}\n"),
        )?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "opaque attribute"])?;
        let output = checked(f.add()?)?;
        reason(
            &output,
            &format!("checkout attribute filter=\"{value}\" on \"hello.txt\""),
        );
        assert_eq!(fs::read(f.destination().join("hello.txt"))?, b"hello\n");
        clean(&f.destination())?;
    }
    Ok(())
}

#[test]
fn environment_overrides_are_named_and_trace_is_opt_in() -> Result<()> {
    let f = Fixture::new()?;
    let output = checked(
        command(env!("CARGO_BIN_EXE_git-twig"), &f.source)
            .args(["new", "--detach", "../feature"])
            .env("GIT_ATTR_SOURCE", "HEAD")
            .output()?,
    )?;
    reason(
        &output,
        "environment override GIT_ATTR_SOURCE requires stock Git",
    );
    clean(&f.destination())?;

    let f = Fixture::new()?;
    git(&f.source, &["sparse-checkout", "init", "--cone"])?;
    let output = checked(
        command(env!("CARGO_BIN_EXE_git-twig"), &f.source)
            .args(["new", "--detach", "../feature"])
            .env_remove("TWIG_TRACE")
            .output()?,
    )?;
    assert!(!String::from_utf8_lossy(&output.stderr).contains("git-twig: "));
    clean(&f.destination())?;
    Ok(())
}

// Windows names cannot hold the control characters this escapes.
#[cfg(unix)]
#[test]
fn attribute_paths_are_escaped_in_diagnostics() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.source.join("odd\tname\n.txt"), "hello\n")?;
    fs::write(
        f.source.join(".gitattributes"),
        "\"odd\\tname\\n.txt\" filter=unset\n",
    )?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "escaped attribute path"])?;
    let output = checked(f.add()?)?;
    reason(
        &output,
        r#"checkout attribute filter="unset" on "odd\tname\n.txt""#,
    );
    assert_eq!(
        fs::read(f.destination().join("odd\tname\n.txt"))?,
        b"hello\n"
    );
    clean(&f.destination())?;
    Ok(())
}

#[test]
fn unsupported_creation_syntax_explains_the_supported_subset() -> Result<()> {
    let f = Fixture::new()?;
    let output = checked(f.twig(&["new", "--no-checkout", "--detach", "../feature"])?)?;
    reason(&output, "creation options require Git");
    assert!(!f.destination().join("hello.txt").exists());
    Ok(())
}

#[test]
fn sparse_checkout_has_a_specific_reason() -> Result<()> {
    let f = Fixture::new()?;
    git(&f.source, &["sparse-checkout", "init", "--cone"])?;
    let output = checked(f.add()?)?;
    reason(&output, "sparse checkout requires stock Git");
    Ok(())
}

#[test]
fn destination_reasons_preserve_existing_paths_and_git_parent_creation() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir(f.destination())?;
    fs::write(f.destination().join("keep"), "untouched")?;
    let output = f.add()?;
    assert!(!output.status.success());
    reason(&output, "destination already exists");
    assert_eq!(fs::read(f.destination().join("keep"))?, b"untouched");
    let f = Fixture::new()?;
    let output = checked(f.twig(&["new", "--detach", "../missing/feature"])?)?;
    reason(&output, "destination parent does not exist");
    clean(&f.root.join("missing/feature"))?;
    Ok(())
}

#[test]
fn hooks_are_identified() -> Result<()> {
    for event in ["reference-transaction", "post-index-change"] {
        let f = Fixture::new()?;
        fs::write(
            f.source.join(".git/hooks").join(event),
            "#!/bin/sh\nexit 0\n",
        )?;
        let output = checked(f.add()?)?;
        reason(&output, &format!("{event} hook requires stock Git"));
        clean(&f.destination())?;
    }
    Ok(())
}

#[test]
fn gitlinks_are_git_written_empty_directories() -> Result<()> {
    let f = Fixture::new()?;
    let oid = String::from_utf8(git(&f.source, &["rev-parse", "HEAD"])?.stdout)?;
    git(
        &f.source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            oid.trim(),
            "sub",
        ],
    )?;
    git(&f.source, &["commit", "-m", "gitlink"])?;
    let output = checked(f.add()?)?;
    native(&output);
    assert!(f.destination().join("sub").is_dir());
    assert!(fs::read_dir(f.destination().join("sub"))?.next().is_none());
    clean(&f.destination())?;
    Ok(())
}

/// Commit one file whose path in a worktree at `destination` is `length`
/// UTF-16 units long, through directories of at most 40 characters.
#[cfg(windows)]
fn commit_path_of_length(f: &Fixture, destination: &Path, length: usize) -> Result<String> {
    let base = destination.as_os_str().encode_wide().count() + 1;
    let mut remaining = length.checked_sub(base).context("fixture root too long")?;
    let file = "f.txt";
    remaining = remaining
        .checked_sub(file.len())
        .context("fixture root too long")?;
    let mut parts = Vec::new();
    while remaining > 0 {
        // Each directory is a name plus its separator.
        let name = remaining.min(41) - 1;
        ensure!(name > 0, "cannot reach exactly {length}");
        parts.push("d".repeat(name));
        remaining -= name + 1;
    }
    parts.push(file.into());
    let relative = parts.join("/");
    let path = f.source.join(&relative);
    fs::create_dir_all(path.parent().context("file has a parent")?)?;
    fs::write(&path, "deep\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-qm", "deep path"])?;
    assert_eq!(base + relative.encode_utf16().count(), length);
    Ok(relative)
}

#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[test]
#[cfg(windows)]
fn deep_paths_git_can_write_clone_natively_without_core_longpaths() -> Result<()> {
    let f = Fixture::new()?;
    if !cow_available(&f)? {
        return Ok(());
    }
    // Writable by Git at the destination, but past MAX_PATH inside Twig's staging.
    let relative = commit_path_of_length(&f, &f.destination(), 230)?;
    let output = checked(f.add()?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join(&relative))?, b"deep\n");
    git(&f.destination(), &["status", "--porcelain"])?;
    clean(&f.destination())
}

#[test]
#[cfg(windows)]
fn paths_near_max_path_delegate_unless_core_longpaths_is_set() -> Result<()> {
    let f = Fixture::new()?;
    if !cow_available(&f)? {
        return Ok(());
    }
    let relative = commit_path_of_length(&f, &f.destination(), 252)?;
    let output = checked(f.add()?)?;
    reason(&output, "need core.longpaths=true");
    assert_eq!(fs::read(f.destination().join(&relative))?, b"deep\n");
    clean(&f.destination())?;

    let f = Fixture::new()?;
    git(&f.source, &["config", "core.longpaths", "true"])?;
    let relative = commit_path_of_length(&f, &f.destination(), 300)?;
    let output = checked(f.add()?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join(&relative))?, b"deep\n");
    clean(&f.destination())
}
