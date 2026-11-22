//! Per-invocation staging ownership. Only successful commands, and commands interrupted
//! before registration, discard their scratch.
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub(super) struct Staging {
    container: Option<PathBuf>,
    bootstrap: Option<PathBuf>,
}

impl Staging {
    pub fn create(&mut self, root: &Path, destination: &Path) -> Result<PathBuf> {
        let destination_parent = destination.parent().context("destination has no parent")?;
        let parent = crate::state_directory(root)?.join("staging");
        fs::create_dir_all(&parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            anyhow::ensure!(
                fs::metadata(&parent)?.dev() == fs::metadata(destination_parent)?.dev(),
                "Git metadata and destination are on different filesystems"
            );
        }
        #[cfg(windows)]
        anyhow::ensure!(
            crate::windows::volume_identity(&parent)?
                == crate::windows::volume_identity(destination_parent)?,
            "Git metadata and destination are on different volumes"
        );
        let container = create_private(&parent)?;
        self.container = Some(container.clone());
        crate::trace(&format!(
            "private staging: {} (retained if creation fails)",
            container.display()
        ));

        // Inherit the destination's GID, SGID and ACLs before relocating:
        // created under .git the checkout would inherit different metadata.
        // Never create the destination itself here. While it exists empty, a
        // concurrent creation hands it to Git, which accepts an empty directory,
        // and both would own it. `.twig/` passes on what the destination's
        // parent passes on, like every level below it.
        let bootstrap = bootstrap(destination_parent)?;
        self.bootstrap = Some(bootstrap.clone());
        crate::trace(&format!(
            "destination metadata: inherited through {}",
            bootstrap.display()
        ));
        let checkout = container.join("checkout");
        fs::rename(&bootstrap, &checkout).with_context(|| {
            format!(
                "cannot relocate empty staging {} into {}",
                bootstrap.display(),
                checkout.display()
            )
        })?;
        self.bootstrap = None;
        remove_empty_folder(&bootstrap);
        Ok(checkout)
    }

    pub fn cleanup(&mut self, outcome: &str) {
        // Never relocated: only the empty directory created above.
        if let Some(bootstrap) = self.bootstrap.take() {
            if let Err(error) = fs::remove_dir(&bootstrap) {
                eprintln!(
                    "git-twig: {outcome}; could not remove empty staging {}: {error}",
                    bootstrap.display()
                );
            }
            remove_empty_folder(&bootstrap);
        }
        if let Some(path) = self.container.take() {
            let result = fs::symlink_metadata(&path)
                .map_err(anyhow::Error::from)
                .and_then(|metadata| {
                    crate::snapshot::remove_private_tree(&path, metadata.file_type())
                });
            if let Err(error) = result {
                eprintln!(
                    "git-twig: {outcome}; could not remove staging at {}: {error:#}",
                    path.display()
                );
            }
        }
    }
}

fn private_directory() -> fs::DirBuilder {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = fs::DirBuilder::new();
    // Windows directories inherit the ACL of the directory they live in.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

/// An empty private directory in `.twig/` beside the destination. Another
/// Twig may remove an empty `.twig/` meanwhile; recreate it and try again.
fn bootstrap(destination_parent: &Path) -> Result<PathBuf> {
    #[cfg(unix)]
    const EINVAL: i32 = libc::EINVAL;
    #[cfg(not(unix))]
    const EINVAL: i32 = 0;
    // Creating in a directory another process is removing fails with ENOENT,
    // EINVAL on APFS, or access denied while Windows has its delete pending.
    let vanished = |error: &anyhow::Error| {
        error.chain().any(|cause| {
            cause.downcast_ref::<std::io::Error>().is_some_and(|error| {
                error.kind() == std::io::ErrorKind::NotFound
                    || (cfg!(unix) && error.raw_os_error() == Some(EINVAL))
                    || (cfg!(windows) && error.kind() == std::io::ErrorKind::PermissionDenied)
            })
        })
    };
    for _ in 0..64 {
        match crate::state_folder(destination_parent).and_then(|folder| create_private(&folder)) {
            Err(error) if vanished(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(2))
            }
            result => return result,
        }
    }
    anyhow::bail!("{:?} kept disappearing", destination_parent.join(".twig"))
}

/// Remove `.twig/` once nothing is left in it. Others may still use it.
fn remove_empty_folder(bootstrap: &Path) {
    if let Some(folder) = bootstrap.parent() {
        let _ = fs::remove_dir(folder);
    }
}

fn create_private(parent: &Path) -> Result<PathBuf> {
    for _ in 0..1000 {
        let path = parent.join(format!(
            "stage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match private_directory().create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(
                    anyhow::Error::from(error).context(format!("cannot create {}", path.display()))
                );
            }
        }
    }
    anyhow::bail!("unable to allocate private staging directory")
}
