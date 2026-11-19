//! `git twig remove` keeps Git's refusals, and finishes a removal Git committed
//! to but could not complete.
use super::*;

/// A tracked path Git can check out relative to the worktree, but whose
/// absolute path exceeds PATH_MAX. Git deletes worktrees by absolute path.
fn deep_path(f: &Fixture) -> Result<String> {
    let limit = libc::PATH_MAX as usize;
    let prefix = f.destination().as_os_str().len() + 1;
    let length = limit + 16 - prefix;
    assert!(
        length < limit - 16,
        "fixture root too short to exceed PATH_MAX"
    );
    let mut relative = String::new();
    while relative.len() + 101 < length {
        relative.push_str(&"d".repeat(100));
        relative.push('/');
    }
    relative.push_str(&"f".repeat(length - relative.len()));
    let script = r#"mkdir -p "$(dirname "$1")" && printf 'deep\n' > "$1""#;
    checked(
        command("/bin/sh", &f.source)
            .args(["-c", script, "sh", &relative])
            .output()?,
    )?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "deep"])?;
    Ok(relative)
}

fn registered(f: &Fixture) -> Result<bool> {
    let list = checked(f.twig(&["list", "--porcelain"])?)?;
    let destination = format!("worktree {}\n", f.destination().display());
    Ok(String::from_utf8_lossy(&list.stdout).contains(&destination))
}

#[test]
fn worktrees_with_paths_longer_than_path_max_are_removed_completely() -> Result<()> {
    let f = Fixture::new()?;
    deep_path(&f)?;
    checked(f.add()?)?;
    let output = f.twig(&["remove", "../feature"])?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("failed to delete"), "{stderr}");
    assert!(!f.destination().exists(), "{stderr}");
    assert!(!registered(&f)?);
    Ok(())
}

#[test]
fn removal_refusals_are_kept_for_worktrees_with_long_paths() -> Result<()> {
    let f = Fixture::new()?;
    deep_path(&f)?;
    checked(f.add()?)?;
    fs::write(f.destination().join("untracked"), "keep me\n")?;
    let output = f.twig(&["remove", "../feature"])?;
    assert!(!output.status.success());
    assert_eq!(fs::read(f.destination().join("untracked"))?, b"keep me\n");
    assert!(registered(&f)?);
    checked(f.twig(&["lock", "../feature"])?)?;
    fs::remove_file(f.destination().join("untracked"))?;
    let output = f.twig(&["remove", "--force", "../feature"])?;
    assert!(!output.status.success());
    assert!(f.destination().join("hello.txt").exists());
    assert!(registered(&f)?);
    Ok(())
}
