use super::adopt::index;
use super::*;
use std::{
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    os::{
        macos::fs::MetadataExt as MacMetadataExt,
        unix::fs::{MetadataExt, PermissionsExt},
    },
};

fn peer(f: &Fixture) -> Result<PathBuf> {
    git(
        &f.source,
        &[
            "worktree",
            "add",
            "-b",
            "feat/existing",
            "../feature",
            "HEAD",
        ],
    )?;
    Ok(f.destination())
}
fn compact(f: &Fixture, root: &Path, extra: &[&str]) -> Result<Output> {
    let mut args = vec![
        "adopt",
        "--compact",
        "--offline",
        "--yes",
        "--worktrees-dir",
        root.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    f.twig(&args)
}
/// Compaction keeps recovery state under Git metadata, which moves with main.
/// Nothing Twig-owned may appear beside the checkouts.
fn recoveries(f: &Fixture) -> Result<Vec<PathBuf>> {
    assert_eq!(
        super::adopt_layout::twig_state(&f.root)?,
        Vec::<PathBuf>::new()
    );
    let mut found = Vec::new();
    for main in [f.source.clone(), f.root.join("managed/main")] {
        match main.join(".git/twigs/compact").read_dir() {
            Ok(entries) => found.extend(entries.filter_map(|e| e.ok()).map(|e| e.path())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(found)
}
#[derive(Debug, PartialEq, Eq)]
struct Attributes {
    mode: u32,
    uid: u32,
    gid: u32,
    modified: SystemTime,
    created: SystemTime,
    flags: u32,
}
fn attributes(path: &Path) -> Result<Attributes> {
    let m = fs::metadata(path)?;
    Ok(Attributes {
        mode: m.mode(),
        uid: m.uid(),
        gid: m.gid(),
        modified: m.modified()?,
        created: m.created()?,
        flags: MacMetadataExt::st_flags(&m),
    })
}

#[test]
fn preview_and_missing_offline_ack_do_not_create_or_replace_anything() -> Result<()> {
    let f = Fixture::new()?;
    let target = peer(&f)?;
    let inode = fs::metadata(target.join("hello.txt"))?.ino();
    let root = f.root.join("managed");
    let preview = checked(f.twig(&[
        "adopt",
        "--compact",
        "--dry-run",
        "--worktrees-dir",
        root.to_str().unwrap(),
    ])?)?;
    assert!(String::from_utf8_lossy(&preview.stdout).contains("compaction-preview"));
    assert!(!root.exists());
    assert!(recoveries(&f)?.is_empty());
    let refused = f.twig(&[
        "adopt",
        "--compact",
        "--yes",
        "--worktrees-dir",
        root.to_str().unwrap(),
    ])?;
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--offline"));
    assert!(!root.exists());
    assert_eq!(fs::metadata(target.join("hello.txt"))?.ino(), inode);
    assert!(recoveries(&f)?.is_empty());
    Ok(())
}

#[test]
fn offline_compaction_preserves_target_metadata_and_dirty_staged_bytes() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.source.join("dirty.bin"), b"committed\0\xff")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "compaction input"])?;
    let target = peer(&f)?;
    fs::write(target.join("dirty.bin"), b"staged version")?;
    git(&target, &["add", "dirty.bin"])?;
    for root in [&f.source, &target] {
        fs::write(root.join("dirty.bin"), b"working current\0\xff")?;
    }
    checked(
        command("xattr", &f.source)
            .args(["-w", "com.twig.donor", "not target metadata", "hello.txt"])
            .output()?,
    )?;
    checked(
        command("xattr", &target)
            .args(["-w", "com.twig.target", "preserve target", "hello.txt"])
            .output()?,
    )?;
    checked(
        command("chmod", &target)
            .args(["+a", "everyone allow read", "hello.txt"])
            .output()?,
    )?;
    fs::set_permissions(target.join("hello.txt"), fs::Permissions::from_mode(0o751))?;
    checked(
        command("touch", &target)
            .args(["-t", "202001020304.05", "hello.txt"])
            .output()?,
    )?;
    checked(
        command("chflags", &target)
            .args(["hidden", "hello.txt"])
            .output()?,
    )?;
    let before = attributes(&target.join("hello.txt"))?;
    let acl_before = checked(
        command("ls", &target)
            .args(["-lde", "hello.txt"])
            .output()?,
    )?;
    let acl_before = String::from_utf8(acl_before.stdout)?
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    let inode = fs::metadata(target.join("hello.txt"))?.ino();
    let before_index = index(&target)?;
    let root = f.root.join("managed");
    let result = checked(compact(&f, &root, &[])?)?;
    let out = String::from_utf8_lossy(&result.stdout);
    assert!(out.contains("compaction-result"), "{out}");
    let adopted = root.join("feat-existing");
    assert_ne!(
        fs::metadata(adopted.join("hello.txt"))?.ino(),
        inode,
        "{out}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(attributes(&adopted.join("hello.txt"))?, before);
    assert_eq!(index(&adopted)?, before_index);
    assert_eq!(
        fs::read(adopted.join("dirty.bin"))?,
        b"working current\0\xff"
    );
    let acl_after = checked(
        command("ls", &adopted)
            .args(["-lde", "hello.txt"])
            .output()?,
    )?;
    assert_eq!(
        String::from_utf8(acl_after.stdout)?
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n"),
        acl_before
    );
    let x = checked(
        command("xattr", &adopted)
            .args(["-p", "com.twig.target", "hello.txt"])
            .output()?,
    )?;
    assert_eq!(String::from_utf8(x.stdout)?.trim(), "preserve target");
    assert!(
        !command("xattr", &adopted)
            .args(["-p", "com.twig.donor", "hello.txt"])
            .output()?
            .status
            .success()
    );
    fs::write(adopted.join("hello.txt"), "private write")?;
    assert_eq!(fs::read(root.join("main/hello.txt"))?, b"hello\n");
    // The same adopted layout can be revisited without losing new dirty bytes
    // or changing index entries; files no longer matching the donor are skipped.
    checked(twig(
        &root.join("main"),
        &["adopt", "--compact", "--offline", "--yes"],
    )?)?;
    assert_eq!(fs::read(adopted.join("hello.txt"))?, b"private write");
    assert_eq!(index(&adopted)?, before_index);
    // Verified retired originals no longer pin their old extents, and a
    // completed compaction keeps no recovery state.
    assert_eq!(recoveries(&f)?, Vec::<PathBuf>::new());
    Ok(())
}

#[test]
fn an_open_original_is_not_retired_and_recovery_keeps_its_late_writes() -> Result<()> {
    let f = Fixture::new()?;
    let target = peer(&f)?;
    let mut writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(target.join("hello.txt"))?;
    let old_inode = writer.metadata()?.ino();
    let root = f.root.join("managed");
    let output = compact(&f, &root, &[])?;
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("both versions retained"),
        "{output:?}"
    );
    assert!(root.join("main/.git").is_dir());
    let adopted = root.join("feat-existing");
    assert_ne!(fs::metadata(adopted.join("hello.txt"))?.ino(), old_inode);
    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(b"late original write\n")?;
    writer.sync_all()?;
    let mut contents = String::new();
    writer.seek(SeekFrom::Start(0))?;
    writer.read_to_string(&mut contents)?;
    assert!(contents.starts_with("late original write"));
    let originals: Vec<_> = recoveries(&f)?
        .iter()
        .flat_map(|p| p.read_dir().unwrap())
        .filter_map(|e| e.ok())
        .filter(|e| e.metadata().is_ok_and(|m| m.ino() == old_inode))
        .map(|e| e.path())
        .collect();
    assert_eq!(originals.len(), 1);
    assert_eq!(fs::read_to_string(&originals[0])?, contents);
    assert_eq!(fs::read(adopted.join("hello.txt"))?, b"hello\n");
    Ok(())
}

#[test]
fn odd_names_resource_forks_hardlinks_and_symlinks_survive() -> Result<()> {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let f = Fixture::new()?;
    let mut odd = std::ffi::OsString::from_vec(b"odd\x20path\n\t\xff.bin".to_vec());
    if let Err(error) = fs::write(f.source.join(&odd), b"odd bytes\0\xff") {
        ensure!(error.raw_os_error() == Some(libc::EILSEQ), "{error}");
        eprintln!(
            "fixture filesystem rejects non-UTF8 names; exercising Unicode/control-character name instead"
        );
        odd = "odd path\n\t雪.bin".into();
        fs::write(f.source.join(&odd), b"odd bytes\0\xff")?;
    }
    fs::write(f.source.join("hard"), b"hardlinked content")?;
    symlink("hello.txt", f.source.join("link"))?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "special compaction files"])?;
    let target = peer(&f)?;
    fs::hard_link(target.join("hard"), f.root.join("retained-hardlink"))?;
    let hard_inode = fs::metadata(target.join("hard"))?.ino();
    let odd_inode = fs::metadata(target.join(&odd))?.ino();
    checked(
        command("xattr", &target)
            .args(["-wx", "com.apple.ResourceFork", "01020304", "hello.txt"])
            .output()?,
    )?;
    let before_index = index(&target)?;
    let root = f.root.join("managed");
    checked(compact(&f, &root, &[])?)?;
    let adopted = root.join("feat-existing");
    assert_ne!(fs::metadata(adopted.join(&odd))?.ino(), odd_inode);
    assert_eq!(fs::read(adopted.join(&odd))?, b"odd bytes\0\xff");
    assert_eq!(fs::metadata(adopted.join("hard"))?.ino(), hard_inode);
    assert_eq!(
        fs::metadata(f.root.join("retained-hardlink"))?.ino(),
        hard_inode
    );
    assert_eq!(
        fs::read_link(adopted.join("link"))?,
        PathBuf::from("hello.txt")
    );
    assert_eq!(index(&adopted)?, before_index);
    let fork = checked(
        command("xattr", &adopted)
            .args(["-px", "com.apple.ResourceFork", "hello.txt"])
            .output()?,
    )?;
    assert_eq!(
        String::from_utf8(fork.stdout)?
            .split_whitespace()
            .collect::<String>(),
        "01020304"
    );
    Ok(())
}

#[test]
fn inherited_recovery_acl_is_refused_before_worktrees_move() -> Result<()> {
    let f = Fixture::new()?;
    let target = peer(&f)?;
    // Recovery state is created under Git metadata and inherits its ACL.
    let state = f.source.join(".git/twigs");
    fs::create_dir_all(&state)?;
    checked(
        command("chmod", &f.root)
            .args(["+a", "everyone allow read,file_inherit,directory_inherit"])
            .arg(&state)
            .output()?,
    )?;
    let root = f.root.join("managed");
    let output = compact(&f, &root, &[])?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("not private"),
        "{output:?}"
    );
    assert!(!root.exists());
    assert!(f.source.join(".git").is_dir());
    assert!(target.join(".git").is_file());
    for recovery in recoveries(&f)? {
        assert_eq!(recovery.read_dir()?.count(), 0);
    }
    Ok(())
}

#[test]
fn move_only_preserves_inodes_and_creates_no_compaction_scratch() -> Result<()> {
    let f = Fixture::new()?;
    let target = peer(&f)?;
    let inode = fs::metadata(target.join("hello.txt"))?.ino();
    let root = f.root.join("managed");
    checked(f.twig(&[
        "adopt",
        "--move-only",
        "--yes",
        "--worktrees-dir",
        root.to_str().unwrap(),
    ])?)?;
    assert_eq!(
        fs::metadata(root.join("feat-existing/hello.txt"))?.ino(),
        inode
    );
    assert!(recoveries(&f)?.is_empty());
    Ok(())
}
