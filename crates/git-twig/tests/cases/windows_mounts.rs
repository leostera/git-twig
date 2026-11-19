//! Volumes mounted at a folder instead of a drive letter, as Dev Drives in a
//! VHDX often are. The mount folder is a reparse point on the outer volume, so
//! asking which volume holds it without following it names the wrong one.
//! Creating a volume needs an elevated session (CI runners have one), so these
//! cases run only with `TWIG_TEST_WINDOWS_VHDX=1`.
use super::*;

/// A fresh ReFS Dev Drive mounted at an empty folder on the system volume.
struct FolderVolume {
    image: PathBuf,
    mount: PathBuf,
}

impl FolderVolume {
    fn create() -> Result<Option<Self>> {
        if std::env::var_os("TWIG_TEST_WINDOWS_VHDX").is_none() {
            eprintln!("skipped: set TWIG_TEST_WINDOWS_VHDX=1 in an elevated session");
            return Ok(None);
        }
        let base = std::env::temp_dir().join(format!(
            "twig-volume-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(base.join("mount"))?;
        let volume = Self {
            image: base.join("volume.vhdx"),
            mount: base.join("mount"),
        };
        diskpart(&format!(
            "create vdisk file=\"{}\" maximum=4096 type=expandable\nselect vdisk file=\"{0}\"\nattach vdisk\nconvert gpt\ncreate partition primary\nassign mount=\"{}\"\n",
            volume.image.display(),
            volume.mount.display()
        ))?;
        checked(
            Command::new("format.com")
                .arg(&volume.mount)
                .args(["/FS:ReFS", "/DevDrv", "/Q", "/V:TwigTest", "/Y"])
                .output()?,
        )?;
        Ok(Some(volume))
    }
}

impl Drop for FolderVolume {
    fn drop(&mut self) {
        let _ = diskpart(&format!(
            "select vdisk file=\"{}\"\ndetach vdisk\n",
            self.image.display()
        ));
        let _ = fs::remove_file(&self.image);
    }
}

fn diskpart(script: &str) -> Result<()> {
    let file = std::env::temp_dir().join(format!(
        "twig-diskpart-{}-{}.txt",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&file, script)?;
    let output = Command::new("diskpart").arg("/s").arg(&file).output();
    let _ = fs::remove_file(&file);
    let output = checked(output?)?;
    let text = String::from_utf8_lossy(&output.stdout);
    ensure!(!text.contains("error"), "diskpart: {text}");
    Ok(())
}

fn repository(root: &Path) -> Result<PathBuf> {
    let source = root.join("source");
    fs::create_dir(&source)?;
    git(&source, &["init", "-q", "-b", "main"])?;
    git(&source, &["config", "user.name", "Fixture"])?;
    git(
        &source,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    fs::write(source.join("hello.txt"), "hello\n")?;
    git(&source, &["add", "."])?;
    git(&source, &["commit", "-qm", "init"])?;
    Ok(source)
}

#[test]
fn worktrees_beside_a_repository_at_a_folder_mounted_volume_root_clone_natively() -> Result<()> {
    let Some(volume) = FolderVolume::create()? else {
        return Ok(());
    };
    let source = repository(&volume.mount)?;
    // The destination's parent is the mount folder itself.
    let output = checked(twig(&source, &["new", "--detach", "../feature", "HEAD"])?)?;
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Windows block clone CoW: verified tracked files"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(volume.mount.join("feature/hello.txt"))?,
        b"hello\n"
    );
    clean(&volume.mount.join("feature"))
}

#[test]
fn adoption_accepts_a_main_checkout_directly_under_a_folder_mounted_volume() -> Result<()> {
    let Some(volume) = FolderVolume::create()? else {
        return Ok(());
    };
    let source = repository(&volume.mount)?;
    git(
        &source,
        &["worktree", "add", "-q", "-b", "feature", "../feature"],
    )?;
    let output = checked(twig(&source, &["adopt", "--dry-run"])?)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("mount point"), "{stdout}");
    Ok(())
}
