//! Whatever the donor carried, a finished command must not strand scratch, or
//! ignored bytes, in Git metadata.
use super::*;

/// Git addresses files relative to the checkout root, so a tracked path may be
/// reachable there and still exceed PATH_MAX under the longer staging prefix.
#[test]
fn paths_too_long_for_staging_still_create_and_leave_no_scratch() -> Result<()> {
    let f = Fixture::new()?;
    let relative = format!("{}file", format!("{}/", "d".repeat(100)).repeat(9));
    let script = r#"mkdir -p "$(dirname "$1")" && printf 'deep\n' > "$1""#;
    checked(
        command("/bin/sh", &f.source)
            .args(["-c", script, "sh", &relative])
            .output()?,
    )?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "deep"])?;
    checked(f.add()?)?;
    let deep = checked(
        command("/bin/cat", &f.destination())
            .arg(&relative)
            .output()?,
    )?;
    assert_eq!(deep.stdout, b"deep\n");
    clean(&f.destination())?;
    assert_eq!(f.stages()?, Vec::<PathBuf>::new());
    Ok(())
}

/// Git refuses these before touching anything; so must the preparation.
#[test]
fn refused_branch_names_leave_no_scratch() -> Result<()> {
    let f = Fixture::new()?;
    git(&f.source, &["branch", "taken"])?;
    for (branch, complaint) in [("taken", "already exists"), ("bad..name", "not a valid")] {
        let output = f.twig(&["new", "-b", branch, "../feature", "HEAD"])?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert!(stderr.contains(complaint), "{stderr}");
        assert!(!f.destination().exists());
        assert_eq!(f.stages()?, Vec::<PathBuf>::new(), "{stderr}");
    }
    Ok(())
}

/// exFAT keeps macOS metadata in `._name` companions that disappear with their
/// file, after the directory listing already named them.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disposable exFAT disk image; run with --include-ignored"]
fn companion_files_that_vanish_do_not_strand_staging() -> Result<()> {
    struct Detach(PathBuf);
    impl Drop for Detach {
        fn drop(&mut self) {
            let _ = Command::new("/usr/bin/hdiutil")
                .args(["detach", "-force"])
                .arg(&self.0)
                .output();
        }
    }
    let f = Fixture::new()?;
    let image = f.root.join("exfat.dmg");
    let volume = f.root.join("volume");
    fs::create_dir(&volume)?;
    checked(
        command("/usr/bin/hdiutil", &f.root)
            .args(["create", "-quiet", "-size", "40m", "-fs", "ExFAT"])
            .args(["-volname", "twigtest"])
            .arg(&image)
            .output()?,
    )?;
    checked(
        command("/usr/bin/hdiutil", &f.root)
            .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
            .arg(&volume)
            .arg(&image)
            .output()?,
    )?;
    let _detach = Detach(volume.clone());
    let source = volume.join("source");
    git(
        &f.root,
        &["clone", "-q", "source", source.to_str().unwrap()],
    )?;
    let output = checked(twig(&source, &["new", "--detach", "../feature"])?)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("git-twig: stock Git:"), "{stderr}");
    assert!(!stderr.contains("could not remove staging"), "{stderr}");
    assert_eq!(fs::read_dir(source.join(".git/twigs/staging"))?.count(), 0);
    assert_eq!(fs::read(volume.join("feature/hello.txt"))?, b"hello\n");
    Ok(())
}

#[cfg(target_os = "macos")]
mod immutable {
    use super::*;

    fn chflags(flags: &str, path: &Path) -> Result<()> {
        checked(
            command("/usr/bin/chflags", path.parent().unwrap())
                .arg(flags)
                .arg(path)
                .output()?,
        )?;
        Ok(())
    }

    /// Fixtures are retained, but must stay removable by an ordinary `rm -r`.
    struct Unlock<'a>(&'a Path);
    impl Drop for Unlock<'_> {
        fn drop(&mut self) {
            let _ = Command::new("/usr/bin/chflags")
                .args(["-R", "nouchg"])
                .arg(self.0)
                .output();
        }
    }

    fn flags(path: &Path) -> Result<u32> {
        use std::os::macos::fs::MetadataExt;
        Ok(fs::symlink_metadata(path)?.st_flags())
    }

    #[test]
    fn immutable_tracked_files_are_shared_as_ordinary_private_files() -> Result<()> {
        let f = Fixture::new()?;
        let _unlock = Unlock(&f.root);
        fs::create_dir(f.source.join("tree"))?;
        fs::write(f.source.join("tree/file"), "nested\n")?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "nested"])?;
        for name in ["hello.txt", "tree/file", "tree"] {
            chflags("uchg", &f.source.join(name))?;
        }
        let output = checked(f.add()?)?;
        native(&output);
        for name in ["hello.txt", "tree/file", "tree"] {
            assert_eq!(flags(&f.destination().join(name))?, 0, "flags for {name}");
            assert_ne!(flags(&f.source.join(name))?, 0, "source flags for {name}");
        }
        fs::write(f.destination().join("tree/file"), "private\n")?;
        assert_eq!(fs::read(f.source.join("tree/file"))?, b"nested\n");
        assert_eq!(f.stages()?, Vec::<PathBuf>::new());
        Ok(())
    }

    #[test]
    fn immutable_ignored_files_are_pruned_from_staging() -> Result<()> {
        let f = Fixture::new()?;
        let _unlock = Unlock(&f.root);
        fs::create_dir_all(f.source.join("tree/ignored/deep"))?;
        fs::write(f.source.join("tree/file"), "nested\n")?;
        git(&f.source, &["add", "."])?;
        git(&f.source, &["commit", "-m", "nested"])?;
        fs::write(f.source.join("tree/ignored/deep/secret"), "secret\n")?;
        chflags("uchg", &f.source.join("tree/ignored/deep/secret"))?;
        chflags("uchg", &f.source.join("tree/ignored/deep"))?;
        let output = checked(f.add()?)?;
        native(&output);
        assert!(!f.destination().join("tree/ignored").exists());
        clean(&f.destination())?;
        // Ignored donor bytes must not survive anywhere under Git metadata.
        assert_eq!(f.stages()?, Vec::<PathBuf>::new());
        assert_eq!(
            fs::read(f.source.join("tree/ignored/deep/secret"))?,
            b"secret\n"
        );
        Ok(())
    }
}

/// The checkout inherits the destination's group and ACLs through a private
/// directory in `.twig/` beside it, never through the destination itself,
/// which a concurrent creation could hand to Git while it is empty. Nothing
/// is left beside the checkouts afterwards.
#[test]
fn destination_metadata_is_inherited_through_the_twig_folder() -> Result<()> {
    let f = Fixture::new()?;
    let output = checked(
        command(env!("CARGO_BIN_EXE_git-twig"), &f.source)
            .env("TWIG_TRACE", "1")
            .args(["new", "-b", "feature", "../feature", "HEAD"])
            .output()?,
    )?;
    native(&output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let traced = format!(
        "destination metadata: inherited through {}",
        f.root.join(".twig").join("stage-").display()
    );
    assert!(stderr.contains(&traced), "{stderr}");
    f.assert_no_sibling_stages()?;
    clean(&f.destination())
}

/// Inheritable ACL entries on the destination's parent reach Twig's checkout
/// exactly as they reach one Git creates.
#[cfg(target_os = "macos")]
#[test]
fn inherited_acl_entries_match_git() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir_all(f.source.join("tree"))?;
    fs::write(f.source.join("tree/file"), "data")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "tree"])?;
    let parent = f.root.join("shared");
    fs::create_dir(&parent)?;
    checked(
        command("chmod", &f.root)
            .args([
                "+a",
                "everyone allow read,list,search,file_inherit,directory_inherit",
            ])
            .arg(&parent)
            .output()?,
    )?;
    native(&checked(f.twig(&[
        "new",
        "-b",
        "feature",
        "../shared/twig",
        "HEAD",
    ])?)?);
    git(
        &f.source,
        &["worktree", "add", "--detach", "../shared/stock"],
    )?;
    let acl = |path: PathBuf| -> Result<String> {
        let listing = checked(command("ls", &f.root).arg("-led").arg(&path).output()?)?;
        Ok(String::from_utf8(listing.stdout)?
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n"))
    };
    for name in ["", "tree", "tree/file"] {
        let ours = acl(parent.join("twig").join(name))?;
        assert!(ours.contains("inherited"), "{name}: {ours}");
        assert_eq!(ours, acl(parent.join("stock").join(name))?, "{name}");
    }
    assert!(
        fs::read_dir(&parent)?.all(|e| !e.unwrap().file_name().to_string_lossy().starts_with("."))
    );
    Ok(())
}
