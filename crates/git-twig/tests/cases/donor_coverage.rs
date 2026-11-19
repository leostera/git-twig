//! Raw checkout parity, with an independent Git oracle that cannot become our donor.
use super::*;

// Windows names cannot hold control characters.
#[cfg(unix)]
const UNUSUAL_NAME: &str = " added\tname\n";
#[cfg(windows)]
const UNUSUAL_NAME: &str = " added name ü";
#[cfg(unix)]
const NEAR_DONOR: &str = "near\n donor";
#[cfg(windows)]
const NEAR_DONOR: &str = "near donor ü";
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq)]
enum File {
    Directory,
    Regular { bytes: Vec<u8>, executable: bool },
    Link(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Snapshot {
    files: BTreeMap<PathBuf, File>,
    head: Vec<u8>,
    index: Vec<u8>,
    status: Vec<u8>,
}

impl Snapshot {
    pub(super) fn capture(root: &Path) -> Result<Self> {
        let mut files = BTreeMap::new();
        inventory(root, root, &mut files)?;
        Ok(Self {
            files,
            head: git(root, &["rev-parse", "HEAD"])?.stdout,
            index: git(root, &["ls-files", "--stage", "-z"])?.stdout,
            status: git(root, &["status", "--porcelain", "-z"])?.stdout,
        })
    }
    fn differences(&self, other: &Self) -> Vec<&'static str> {
        let mut differences = Vec::new();
        if self.files != other.files {
            differences.push("files");
        }
        if self.head != other.head {
            differences.push("HEAD");
        }
        if self.index != other.index {
            differences.push("index");
        }
        if self.status != other.status {
            differences.push("status");
        }
        differences
    }
}

fn inventory(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, File>) -> Result<()> {
    for child in fs::read_dir(directory)? {
        let child = child?;
        if directory == root && child.file_name() == ".git" {
            continue;
        }
        let path = child.path();
        let kind = child.file_type()?;
        if kind.is_dir() {
            files.insert(path.strip_prefix(root)?.into(), File::Directory);
            inventory(root, &path, files)?;
            continue;
        }
        let file = if kind.is_symlink() {
            File::Link(fs::read_link(&path)?)
        } else {
            ensure!(kind.is_file(), "unexpected special checkout entry");
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                fs::metadata(&path)?.permissions().mode() & 0o100 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            File::Regular {
                bytes: fs::read(&path)?,
                executable,
            }
        };
        files.insert(path.strip_prefix(root)?.into(), file);
    }
    Ok(())
}

/// A separate clone means the oracle is not listed in the source's worktree registry.
fn oracle(f: &Fixture, revision: &str) -> Result<PathBuf> {
    let root = f.root.join("oracle");
    git(
        &f.root,
        &[
            "clone",
            "--shared",
            "--no-checkout",
            f.source.to_str().unwrap(),
            root.to_str().unwrap(),
        ],
    )?;
    git(&root, &["checkout", "--detach", revision])?;
    Ok(root)
}

#[test]
fn comparator_matches_git_and_detects_injected_differences() -> Result<()> {
    let f = Fixture::new()?;
    let oracle = oracle(&f, "HEAD")?;
    git(
        &oracle,
        &["worktree", "add", "--detach", "../control", "HEAD"],
    )?;
    let expected = Snapshot::capture(&oracle)?;
    let actual = Snapshot::capture(&f.root.join("control"))?;
    assert!(expected.differences(&actual).is_empty());
    for mutation in [
        "bytes",
        "missing",
        "extra",
        "mode",
        "link",
        "directory",
        "HEAD",
        "index",
        "status",
    ] {
        let mut broken = actual.clone();
        match mutation {
            "bytes" => {
                broken.files.insert(
                    "hello.txt".into(),
                    File::Regular {
                        bytes: b"wrong\n".to_vec(),
                        executable: false,
                    },
                );
            }
            "missing" => {
                broken.files.remove(Path::new("hello.txt"));
            }
            "extra" => {
                broken
                    .files
                    .insert("unexpected".into(), File::Link("elsewhere".into()));
            }
            "mode" => {
                broken.files.insert(
                    "hello.txt".into(),
                    File::Regular {
                        bytes: b"hello\n".to_vec(),
                        executable: true,
                    },
                );
            }
            "link" => {
                broken
                    .files
                    .insert("hello.txt".into(), File::Link("hello\n".into()));
            }
            "directory" => {
                broken.files.insert("hello.txt".into(), File::Directory);
            }
            "HEAD" => broken.head.push(b'x'),
            "index" => broken.index.push(b'x'),
            "status" => broken.status.push(b'x'),
            _ => unreachable!(),
        }
        let area = if ["HEAD", "index", "status"].contains(&mutation) {
            mutation
        } else {
            "files"
        };
        assert_eq!(expected.differences(&broken), vec![area], "{mutation}");
    }
    Ok(())
}

#[test]
fn revisions_added_paths_and_dirty_staged_donors_match_stock_git() -> Result<()> {
    for format in ["sha1", "sha256"] {
        for source_revision in ["HEAD~2", "HEAD"] {
            for target_revision in ["HEAD~2", "HEAD~1", "HEAD"] {
                let f = Fixture::with_object_format(format)?;
                fs::write(f.source.join("common"), b"shared\n".repeat(1024))?;
                fs::write(f.source.join("node"), "old file\n")?;
                fs::write(f.source.join("mode-only"), "same executable bytes\n")?;
                fs::write(f.source.join(".gitattributes"), "* text=auto eol=lf\n")?;
                git(&f.source, &["add", "."])?;
                git(&f.source, &["commit", "-m", "A"])?;
                fs::remove_file(f.source.join("node"))?;
                fs::create_dir(f.source.join("node"))?;
                fs::write(f.source.join("node/child"), "target-only child\n")?;
                fs::write(f.source.join("hello.txt"), "second\n")?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        f.source.join("mode-only"),
                        fs::Permissions::from_mode(0o755),
                    )?;
                }
                fs::write(f.source.join(UNUSUAL_NAME), "target-only root\n")?;
                fs::write(
                    f.source.join(".gitattributes"),
                    "* text=auto eol=lf\nhello.txt text eol=crlf\n",
                )?;
                git(&f.source, &["add", "."])?;
                git(&f.source, &["commit", "-m", "B"])?;
                fs::write(f.source.join("third"), "third\n")?;
                git(&f.source, &["add", "."])?;
                git(&f.source, &["commit", "-m", "C"])?;
                let target =
                    String::from_utf8(git(&f.source, &["rev-parse", target_revision])?.stdout)?;
                let source =
                    String::from_utf8(git(&f.source, &["rev-parse", source_revision])?.stdout)?;
                let oracle = oracle(&f, target.trim())?;
                git(&f.source, &["checkout", "--detach", source.trim()])?;
                fs::write(f.source.join("common"), "staged donor bytes\n")?;
                git(&f.source, &["add", "common"])?;
                fs::write(f.source.join("common"), "unstaged donor bytes\n")?;
                if target_revision == "HEAD" {
                    git(&f.source, &["update-index", "--assume-unchanged", "common"])?;
                } else if source_revision == "HEAD" {
                    git(&f.source, &["update-index", "--skip-worktree", "common"])?;
                }
                let source_files = {
                    let mut files = BTreeMap::new();
                    inventory(&f.source, &f.source, &mut files)?;
                    files
                };
                let index = fs::read(f.source.join(".git/index"))?;
                let output = checked(f.twig(&["new", "--detach", "../feature", target.trim()])?)?;
                native(&output);
                let expected = Snapshot::capture(&oracle)?;
                let actual = Snapshot::capture(&f.destination())?;
                assert!(
                    expected.differences(&actual).is_empty(),
                    "{format} / {source_revision} -> {target_revision}: {:?}",
                    expected.differences(&actual)
                );
                let mut after = BTreeMap::new();
                inventory(&f.source, &f.source, &mut after)?;
                assert_eq!(source_files, after);
                assert_eq!(index, fs::read(f.source.join(".git/index"))?);
            }
        }
    }
    Ok(())
}

#[test]
fn remote_tracking_configuration_is_not_a_checkout_policy_change() -> Result<()> {
    let f = Fixture::new()?;
    let old = String::from_utf8(git(&f.source, &["rev-parse", "HEAD"])?.stdout)?;
    fs::write(f.source.join("hello.txt"), "newer\n")?;
    git(&f.source, &["commit", "-am", "newer source"])?;
    git(
        &f.source,
        &["config", "remote.origin.url", f.source.to_str().unwrap()],
    )?;
    git(
        &f.source,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )?;
    git(
        &f.source,
        &["update-ref", "refs/remotes/origin/topic", old.trim()],
    )?;
    let oracle = oracle(&f, old.trim())?;
    let output = checked(f.twig(&["new", "-b", "feature", "../feature", "origin/topic"])?)?;
    native(&output);
    assert_eq!(
        Snapshot::capture(&oracle)?,
        Snapshot::capture(&f.destination())?
    );
    assert_eq!(
        git(&f.destination(), &["config", "branch.feature.remote"])?.stdout,
        b"origin\n"
    );
    assert_eq!(
        git(&f.destination(), &["config", "branch.feature.merge"])?.stdout,
        b"refs/heads/topic\n"
    );
    Ok(())
}

#[test]
fn no_shared_target_paths_delegate_explicitly() -> Result<()> {
    let f = Fixture::new()?;
    if !cow_available(&f)? {
        return Ok(()); // Twig delegates before looking for a donor.
    }
    let old = String::from_utf8(git(&f.source, &["rev-parse", "HEAD"])?.stdout)?;
    git(&f.source, &["rm", "-r", "."])?;
    fs::write(f.source.join("target-only"), "new tree\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "disjoint tree"])?;
    let target = String::from_utf8(git(&f.source, &["rev-parse", "HEAD"])?.stdout)?;
    let oracle = oracle(&f, target.trim())?;
    git(&f.source, &["checkout", "--detach", old.trim()])?;
    let output = checked(f.twig(&["new", "--detach", "../feature", target.trim()])?)?;
    let trace = String::from_utf8_lossy(&output.stderr);
    assert!(trace.contains("no matching donor blobs"), "{trace}");
    assert!(!trace.contains("CoW: verified"), "{trace}");
    assert_eq!(
        Snapshot::capture(&oracle)?,
        Snapshot::capture(&f.destination())?
    );
    Ok(())
}

#[test]
fn chooses_a_better_linked_donor_without_trusting_its_dirty_bytes() -> Result<()> {
    let f = Fixture::new()?;
    for name in ["a", "b", "c"] {
        fs::write(f.source.join(name), "first\n")?;
    }
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "donor A"])?;
    let first = String::from_utf8(git(&f.source, &["rev-parse", "HEAD"])?.stdout)?;
    git(
        &f.source,
        &[
            "worktree",
            "add",
            "--detach",
            &format!("../{NEAR_DONOR}"),
            "HEAD",
        ],
    )?;
    for name in ["a", "b", "c"] {
        fs::write(f.source.join(name), "second\n")?;
    }
    git(&f.source, &["commit", "-am", "current B"])?;
    let near = f.root.join(NEAR_DONOR);
    fs::write(near.join("a"), "dirty linked donor\n")?;
    let index_path = String::from_utf8(
        git(
            &near,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        )?
        .stdout,
    )?;
    let index = fs::read(index_path.trim_end_matches('\n'))?;
    let oracle = oracle(&f, first.trim())?;
    let output = checked(f.twig(&["new", "--detach", "../feature", first.trim()])?)?;
    native(&output);
    if String::from_utf8_lossy(&output.stderr).contains("CoW: verified") {
        assert!(String::from_utf8_lossy(&output.stderr).contains(&format!("donor: {:?}", near)));
    }
    assert_eq!(
        Snapshot::capture(&oracle)?,
        Snapshot::capture(&f.destination())?
    );
    assert_eq!(fs::read(near.join("a"))?, b"dirty linked donor\n");
    assert_eq!(index, fs::read(index_path.trim_end_matches('\n'))?);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(
            fs::metadata(near.join("b"))?.ino(),
            fs::metadata(f.destination().join("b"))?.ino()
        );
    }
    fs::write(f.destination().join("b"), "private target mutation\n")?;
    assert_eq!(fs::read(near.join("b"))?, b"first\n");
    Ok(())
}

/// Git lists a separated main checkout by its Git directory's parent, which is
/// not where the files are. The checkout Twig runs in is still a donor.
#[test]
fn separate_git_directories_share_from_the_real_checkout() -> Result<()> {
    let f = Fixture::new()?;
    let checkout = f.root.join("separate/main");
    fs::create_dir_all(&checkout)?;
    let metadata = f.root.join("metadata.git");
    checked(
        command("git", &f.root)
            .env("GIT_TEMPLATE_DIR", "")
            .args(["init", "-b", "main", "--separate-git-dir"])
            .arg(&metadata)
            .arg(&checkout)
            .output()?,
    )?;
    git(&checkout, &["config", "user.name", "Fixture"])?;
    git(
        &checkout,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    fs::create_dir(checkout.join("tree"))?;
    fs::write(checkout.join("tree/file"), "nested\n")?;
    // Names that also exist inside a Git directory must come from the checkout.
    fs::write(checkout.join("description"), "tracked\n")?;
    git(&checkout, &["add", "."])?;
    git(&checkout, &["commit", "-m", "seed"])?;
    let output = checked(twig(&checkout, &["new", "--detach", "../feature"])?)?;
    native(&output);
    let destination = f.root.join("separate/feature");
    assert_eq!(fs::read(destination.join("tree/file"))?, b"nested\n");
    assert_eq!(fs::read(destination.join("description"))?, b"tracked\n");
    clean(&destination)
}

/// A donor someone is working in has deleted and replaced tracked paths. Those
/// paths are Git's to write; every other file is still worth sharing.
#[cfg(unix)]
#[test]
fn deleted_and_replaced_donor_paths_do_not_disable_sharing() -> Result<()> {
    use std::os::unix::fs::symlink;
    let f = Fixture::new()?;
    for directory in ["tree/kept", "tree/gone", "linked", "swapped"] {
        fs::create_dir_all(f.source.join(directory))?;
    }
    let tracked = [
        "root-gone",
        "root-link",
        "tree/kept/file",
        "tree/gone/file",
        "tree/deleted",
        "tree/became-link",
        "tree/became-directory",
        "linked/file",
        "swapped/file",
    ];
    for name in tracked {
        fs::write(f.source.join(name), format!("{name}\n"))?;
    }
    symlink("kept/file", f.source.join("tree/was-link"))?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "tracked"])?;

    fs::remove_file(f.source.join("root-gone"))?;
    fs::remove_file(f.source.join("root-link"))?;
    symlink("hello.txt", f.source.join("root-link"))?;
    fs::remove_dir_all(f.source.join("tree/gone"))?;
    fs::remove_file(f.source.join("tree/deleted"))?;
    fs::remove_file(f.source.join("tree/became-link"))?;
    symlink("/etc/passwd", f.source.join("tree/became-link"))?;
    fs::remove_file(f.source.join("tree/became-directory"))?;
    fs::create_dir(f.source.join("tree/became-directory"))?;
    fs::write(f.source.join("tree/became-directory/inside"), "untracked\n")?;
    fs::remove_file(f.source.join("tree/was-link"))?;
    fs::write(f.source.join("tree/was-link"), "now a file\n")?;
    fs::remove_dir_all(f.source.join("linked"))?;
    symlink("tree", f.source.join("linked"))?;
    fs::remove_dir_all(f.source.join("swapped"))?;
    fs::write(f.source.join("swapped"), "now a file\n")?;

    let output = checked(f.add()?)?;
    native(&output);
    for name in tracked {
        let path = f.destination().join(name);
        assert!(fs::symlink_metadata(&path)?.is_file(), "{name}");
        assert_eq!(fs::read(path)?, format!("{name}\n").as_bytes(), "{name}");
    }
    assert_eq!(
        fs::read_link(f.destination().join("tree/was-link"))?,
        Path::new("kept/file")
    );
    assert!(
        !f.destination()
            .join("tree/became-directory/inside")
            .exists()
    );
    clean(&f.destination())?;
    // The donor keeps its work in progress.
    assert!(fs::symlink_metadata(f.source.join("linked"))?.is_symlink());
    assert_eq!(fs::read(f.source.join("swapped"))?, b"now a file\n");
    assert!(!f.source.join("tree/deleted").exists());
    Ok(())
}
