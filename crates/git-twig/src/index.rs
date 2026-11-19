//! Record what verification learned as a draft Git index, so the new worktree's
//! first status does not read every file again. Git reads the draft, checks it
//! against the target commit and rewrites it; no draft reaches a worktree.
use crate::tree::{Entry, Kind};
use anyhow::{Context, Result, ensure};
use std::{fs, path::Path};

/// Stat fields of one private file, as Git's index stores them (32 bits each).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    ctime: (u32, u32),
    mtime: (u32, u32),
    device: u32,
    inode: u32,
    owner: u32,
    group: u32,
    bytes: u32,
}

impl Stat {
    #[cfg(unix)]
    pub fn of(metadata: &fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        // Git keeps the low 32 bits of every field and compares them the same way.
        Self {
            ctime: (metadata.ctime() as u32, metadata.ctime_nsec() as u32),
            mtime: (metadata.mtime() as u32, metadata.mtime_nsec() as u32),
            device: metadata.dev() as u32,
            inode: metadata.ino() as u32,
            owner: metadata.uid(),
            group: metadata.gid(),
            bytes: metadata.size() as u32,
        }
    }

    /// Git for Windows reports the creation time as ctime, the last write time
    /// as mtime, and zero for the device, inode, owner and group.
    #[cfg(windows)]
    pub fn of(metadata: &fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt;
        Self {
            ctime: crate::windows::unix_time(metadata.creation_time()),
            mtime: crate::windows::unix_time(metadata.last_write_time()),
            bytes: metadata.file_size() as u32,
            ..Self::default()
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub fn of(_: &fs::Metadata) -> Self {
        Self::default()
    }
}

/// Write an index naming every target entry at stage 0. `certified` holds, per
/// entry, the stat data seen just before its private bytes were hashed equal to
/// the target blob: the file must still have exactly that identity now. Only
/// those entries are recorded as unchanged.
pub fn write_draft(
    data: &Path,
    index: &Path,
    entries: &[Entry],
    certified: &[Option<Stat>],
) -> Result<()> {
    ensure!(
        entries.len() == certified.len(),
        "one certificate slot per entry"
    );
    let mut records = Vec::with_capacity(entries.len());
    for (entry, certified) in entries.iter().zip(certified) {
        // Entries without a certificate carry no stat data: Git compares those
        // by content when it reads the draft, as it would after writing them.
        let stat = match certified {
            None => Stat::default(),
            Some(seen) => {
                let path = data.join(&entry.path);
                let metadata = fs::symlink_metadata(&path)
                    .with_context(|| format!("verified file vanished: {path:?}"))?;
                ensure!(
                    metadata.is_file() && Stat::of(&metadata) == *seen,
                    "private file changed after verification: {path:?}"
                );
                *seen
            }
        };
        records.push((crate::native::path_bytes(&entry.path), entry, stat));
    }
    // Index order is plain byte order of the full path.
    records.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    ensure!(
        records.windows(2).all(|pair| pair[0].0 != pair[1].0),
        "duplicate index path"
    );
    let object_bytes = entries
        .first()
        .map_or(20, |entry| entry.object.as_str().len() / 2);
    let mut bytes = Vec::with_capacity(12 + records.len() * 96);
    bytes.extend_from_slice(b"DIRC");
    bytes.extend_from_slice(&2u32.to_be_bytes());
    bytes.extend_from_slice(&u32::try_from(records.len())?.to_be_bytes());
    for (path, entry, stat) in &records {
        let start = bytes.len();
        let mode: u32 = match entry.kind {
            Kind::File { executable: false } => 0o100644,
            Kind::File { executable: true } => 0o100755,
            Kind::Symlink => 0o120000,
            Kind::Submodule => 0o160000,
        };
        for field in [
            stat.ctime.0,
            stat.ctime.1,
            stat.mtime.0,
            stat.mtime.1,
            stat.device,
            stat.inode,
            mode,
            stat.owner,
            stat.group,
            stat.bytes,
        ] {
            bytes.extend_from_slice(&field.to_be_bytes());
        }
        let object = entry.object.as_str();
        ensure!(object.len() == object_bytes * 2, "mixed object formats");
        for pair in object.as_bytes().chunks(2) {
            bytes.push(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?);
        }
        // Stage 0, no flags; the length field saturates and the NUL still ends it.
        bytes.extend_from_slice(&(path.len().min(0xfff) as u16).to_be_bytes());
        bytes.extend_from_slice(path);
        // One to eight NULs, so every entry's length is a multiple of eight.
        let padding = 8 - (bytes.len() - start) % 8;
        bytes.resize(bytes.len() + padding, 0);
    }
    // A null checksum is Git's own "not computed" marker. Git rewrites this file
    // before anything else may read it.
    bytes.resize(bytes.len() + object_bytes, 0);
    fs::write(index, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Result<std::path::PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-fixtures")
            .join(format!(
                "index-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_nanos()
            ));
        fs::create_dir_all(root.join("data"))?;
        eprintln!("fixture retained: {}", root.display());
        Ok(root)
    }

    fn entries(listing: &str) -> Vec<Entry> {
        crate::tree::parse(listing.as_bytes()).unwrap()
    }

    #[test]
    fn entries_are_sorted_padded_and_null_terminated() -> Result<()> {
        let root = fixture("layout")?;
        let data = root.join("data");
        fs::create_dir(data.join("a"))?;
        fs::write(data.join("a/b"), "nested")?;
        fs::write(data.join("a.c"), "sibling")?;
        let object = "ab".repeat(20);
        // Tree order lists `a/b` after `a.c`: '.' sorts before '/'.
        let entries = entries(&format!(
            "100644 blob {object}\ta/b\0100755 blob {object}\ta.c\0160000 commit {object}\tmodule\0"
        ));
        let index = root.join("index");
        write_draft(&data, &index, &entries, &[None, None, None])?;
        let bytes = fs::read(&index)?;
        assert_eq!(&bytes[..12], b"DIRC\0\0\0\x02\0\0\0\x03");
        let mut offset = 12;
        let mut seen = Vec::new();
        for _ in 0..3 {
            let mode = u32::from_be_bytes(bytes[offset + 24..offset + 28].try_into()?);
            assert_eq!(&bytes[offset + 40..offset + 60], [0xab; 20]);
            let length = u16::from_be_bytes(bytes[offset + 60..offset + 62].try_into()?) as usize;
            let path = &bytes[offset + 62..offset + 62 + length];
            assert_eq!(bytes[offset + 62 + length], 0);
            seen.push((String::from_utf8(path.to_vec())?, mode));
            let unpadded = 62 + length;
            offset += unpadded + (8 - unpadded % 8);
        }
        assert_eq!(
            seen,
            vec![
                ("a.c".to_owned(), 0o100755),
                ("a/b".to_owned(), 0o100644),
                ("module".to_owned(), 0o160000)
            ]
        );
        assert_eq!(&bytes[offset..], [0; 20]);
        Ok(())
    }

    #[test]
    fn a_file_that_changed_after_verification_is_refused() -> Result<()> {
        let root = fixture("stale")?;
        let data = root.join("data");
        fs::write(data.join("file"), "verified")?;
        let seen = Stat::of(&fs::symlink_metadata(data.join("file"))?);
        let entries = entries(&format!("100644 blob {}\tfile\0", "0".repeat(40)));
        let index = root.join("index");
        write_draft(&data, &index, &entries, &[Some(seen)])?;
        fs::write(data.join("file"), "longer than verified")?;
        assert!(write_draft(&data, &index, &entries, &[Some(seen)]).is_err());
        fs::remove_file(data.join("file"))?;
        assert!(write_draft(&data, &index, &entries, &[Some(seen)]).is_err());
        // Without a certificate nothing is claimed, so nothing can be stale.
        write_draft(&data, &index, &entries, &[None])?;
        assert_eq!(fs::read(&index)?[12..36], [0; 24]);
        Ok(())
    }
}
