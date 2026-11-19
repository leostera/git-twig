//! Verification already read every shared file. The installed index must keep
//! that knowledge, or the first `git status` reads them all a second time.
use super::*;

/// Stat fields Git recorded for one path, from `ls-files --debug`.
#[cfg_attr(not(unix), allow(dead_code))]
fn recorded(checkout: &Path, name: &str) -> Result<(u64, u64)> {
    let listing = String::from_utf8(git(checkout, &["ls-files", "--debug", "--", name])?.stdout)?;
    let field = |key: &str| -> Result<u64> {
        let start = listing
            .find(key)
            .ok_or_else(|| anyhow::anyhow!("no {key} in {listing}"))?
            + key.len();
        let digits: String = listing[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        Ok(digits.parse()?)
    };
    Ok((field("ino: ")?, field("size: ")?))
}

#[cfg(unix)]
fn actual(path: &Path) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.ino(), metadata.len()))
}

#[cfg(unix)]
#[test]
fn installed_index_records_the_files_it_verified() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir_all(f.source.join("tree/deep"))?;
    fs::write(f.source.join("tree/deep/file"), "nested\n")?;
    fs::write(f.source.join("tree/dirty"), "clean\n")?;
    std::os::unix::fs::symlink("deep/file", f.source.join("tree/link"))?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "nested"])?;
    // A dirty donor file is rewritten by Git, and must be recorded as well.
    fs::write(f.source.join("tree/dirty"), "dirty\n")?;
    let output = checked(f.add()?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join("tree/dirty"))?, b"clean\n");
    for name in ["hello.txt", "tree/deep/file", "tree/dirty", "tree/link"] {
        assert_eq!(
            recorded(&f.destination(), name)?,
            actual(&f.destination().join(name))?,
            "index stat data for {name}"
        );
    }
    clean(&f.destination())
}

/// The recorded stat data must never vouch for bytes Git did not hash.
#[test]
fn same_size_dirty_donor_bytes_are_not_recorded_as_clean() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir(f.source.join("tree"))?;
    fs::write(f.source.join("tree/file"), "nested\n")?;
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "nested"])?;
    fs::write(f.source.join("tree/file"), "NESTED\n")?;
    fs::write(f.source.join("hello.txt"), "HELLO\n")?;
    let output = checked(f.add()?)?;
    native(&output);
    assert_eq!(fs::read(f.destination().join("tree/file"))?, b"nested\n");
    assert_eq!(fs::read(f.destination().join("hello.txt"))?, b"hello\n");
    assert_eq!(fs::read(f.source.join("tree/file"))?, b"NESTED\n");
    clean(&f.destination())
}
