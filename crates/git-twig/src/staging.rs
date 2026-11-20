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

        // Inherit the destination's GID, SGID and ACLs: create the destination
        // itself, exactly where Git would, then move it into staging. Created
        // under .git it would inherit different metadata, and a scratch name
        // beside it would leave Twig names beside the checkouts.
        private_directory()
            .create(destination)
            .with_context(|| format!("cannot create destination {}", destination.display()))?;
        self.bootstrap = Some(destination.to_path_buf());
        crate::trace(&format!("destination metadata: {}", destination.display()));
        let checkout = container.join("checkout");
        fs::rename(destination, &checkout).with_context(|| {
            format!(
                "cannot move empty destination {} into staging {}",
                destination.display(),
                checkout.display()
            )
        })?;
        self.bootstrap = None;
        Ok(checkout)
    }

    pub fn cleanup(&mut self, outcome: &str) {
        // Still at the destination: only the empty directory created above.
        if let Some(destination) = self.bootstrap.take()
            && let Err(error) = fs::remove_dir(&destination)
        {
            eprintln!(
                "git-twig: {outcome}; could not remove empty destination {}: {error}",
                destination.display()
            );
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
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("unable to allocate private staging directory")
}
