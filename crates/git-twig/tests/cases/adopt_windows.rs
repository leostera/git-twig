//! In-place sharing on Windows. ReFS has no kernel compare-and-share, so Twig
//! holds both files open denying writes from its comparison until the clusters
//! are shared. These cases check what that lock must refuse and keep.
use super as adopt;
use super::*;
use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

fn feature_with(f: &Fixture, files: &[(&str, Vec<u8>)]) -> Result<()> {
    for (name, bytes) in files {
        fs::write(f.source.join(name), bytes)?;
    }
    git(&f.source, &["add", "."])?;
    git(&f.source, &["commit", "-m", "payload"])?;
    git(
        &f.source,
        &["worktree", "add", "--detach", "../feature", "HEAD"],
    )?;
    Ok(())
}

fn deduped_bytes(output: &Output) -> Result<u64> {
    Ok(String::from_utf8_lossy(&output.stdout)
        .split("kernel_deduped_bytes=")
        .nth(1)
        .and_then(|s| s.split(';').next())
        .ok_or_else(|| anyhow::anyhow!("missing dedupe byte counter: {output:?}"))?
        .parse()?)
}

#[test]
fn dedupe_shares_in_place_and_keeps_identity_timestamps_and_index() -> Result<()> {
    let f = Fixture::new()?;
    // Longer than a 64 KiB cluster and ending inside one.
    let payload: Vec<u8> = (0..300_001u32).map(|i| (i % 251) as u8).collect();
    feature_with(
        &f,
        &[("payload.bin", payload.clone()), ("empty", Vec::new())],
    )?;
    fs::write(f.destination().join("different"), "only here\n")?;
    let source = adopt::files(&f.source)?;
    let target = adopt::files(&f.destination())?;
    let indices = (adopt::index(&f.source)?, adopt::index(&f.destination())?);
    let output = f.twig(&["adopt", "--dedupe", "../feature"])?;
    if !cow_available(&f)? {
        assert!(!output.status.success() || deduped_bytes(&output)? == 0);
        assert_eq!(adopt::files(&f.destination())?, target);
        return Ok(());
    }
    let output = checked(output)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("match \"payload.bin\""), "{stdout}");
    assert!(stdout.contains("skip \"empty\""), "{stdout}");
    assert!(deduped_bytes(&output)? >= payload.len() as u64);
    // Same bytes, file identity, attributes and last write time.
    assert_eq!(adopt::files(&f.source)?, source);
    assert_eq!(adopt::files(&f.destination())?, target);
    assert_eq!(
        (adopt::index(&f.source)?, adopt::index(&f.destination())?),
        indices
    );
    let status = git(&f.destination(), &["status", "--porcelain"])?.stdout;
    assert_eq!(String::from_utf8(status)?, "?? different\n");
    // Shared clusters are copy-on-write: a private write stays private.
    fs::write(f.destination().join("payload.bin"), "private\n")?;
    assert_eq!(fs::read(f.source.join("payload.bin"))?, payload);
    assert!(adopt::stages(&f)?.is_empty());
    Ok(())
}

#[test]
fn dedupe_skips_files_open_for_writing_or_mapped_writable() -> Result<()> {
    let f = Fixture::new()?;
    let payload = vec![b'x'; 200_000];
    feature_with(
        &f,
        &[
            ("written.bin", payload.clone()),
            ("mapped.bin", payload.clone()),
            ("donor-written.bin", payload.clone()),
            ("free.bin", payload.clone()),
        ],
    )?;
    if !cow_available(&f)? {
        return Ok(());
    }
    let open = |path: PathBuf| {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
    };
    let writer = open(f.destination().join("written.bin"))?;
    let donor_writer = open(f.source.join("donor-written.bin"))?;
    // A writable mapping whose file handle is already closed: only the mapping
    // can still write, and the lock must still refuse it.
    let mapping = Mapping::new(&f.destination().join("mapped.bin"))?;
    let output = checked(f.twig(&["adopt", "--dedupe", "../feature"])?)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for busy in ["written.bin", "mapped.bin", "donor-written.bin"] {
        assert!(stdout.contains(&format!("skip \"{busy}\"")), "{stdout}");
    }
    assert!(stdout.contains("match \"free.bin\""), "{stdout}");
    // Every writer still writes the file it opened.
    mapping.write(16, b'!');
    drop(mapping);
    use std::io::Write;
    (&writer).write_all(b"edit")?;
    (&donor_writer).write_all(b"edit")?;
    drop((writer, donor_writer));
    assert_eq!(fs::read(f.destination().join("mapped.bin"))?[16], b'!');
    assert_eq!(
        &fs::read(f.destination().join("written.bin"))?[..4],
        b"edit"
    );
    assert_eq!(&fs::read(f.source.join("donor-written.bin"))?[..4], b"edit");
    assert_eq!(fs::read(f.source.join("mapped.bin"))?, payload);
    Ok(())
}

#[test]
fn dedupe_refuses_junctions_and_hardlinks() -> Result<()> {
    let f = Fixture::new()?;
    fs::create_dir(f.source.join("tree"))?;
    feature_with(
        &f,
        &[
            ("tree/file", vec![b'y'; 100_000]),
            ("linked", vec![b'z'; 100_000]),
        ],
    )?;
    if !cow_available(&f)? {
        return Ok(());
    }
    fs::rename(f.destination().join("tree"), f.root.join("outside"))?;
    checked(
        command("cmd", &f.destination())
            .args(["/C", "mklink", "/J", "tree"])
            .arg(f.root.join("outside"))
            .output()?,
    )?;
    fs::hard_link(f.destination().join("linked"), f.root.join("second-name"))?;
    let outside = adopt::files(&f.root.join("outside"))?;
    let output = checked(f.twig(&["adopt", "--dedupe", "../feature"])?)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("skip \"tree/file\""), "{stdout}");
    assert!(stdout.contains("skip \"linked\""), "{stdout}");
    assert_eq!(adopt::files(&f.root.join("outside"))?, outside);
    Ok(())
}

struct Mapping {
    view: windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Mapping {
    fn new(path: &Path) -> Result<Self> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Memory::{CreateFileMappingW, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE},
        };
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)?;
        // SAFETY: a live file handle; the section covers the whole file.
        let section = unsafe {
            CreateFileMappingW(
                file.as_raw_handle(),
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                0,
                std::ptr::null(),
            )
        };
        ensure!(!section.is_null(), "{}", std::io::Error::last_os_error());
        // SAFETY: a live section handle; map its first page for writing.
        let view = unsafe { MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, 4096) };
        // SAFETY: the view keeps the section alive; close our handles to it.
        unsafe { CloseHandle(section) };
        drop(file);
        ensure!(!view.Value.is_null(), "{}", std::io::Error::last_os_error());
        Ok(Self { view })
    }

    fn write(&self, offset: usize, byte: u8) {
        // SAFETY: the view maps at least one writable page.
        unsafe { *self.view.Value.cast::<u8>().add(offset) = byte };
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: this owns exactly one successful view.
        unsafe { windows_sys::Win32::System::Memory::UnmapViewOfFile(self.view) };
    }
}
