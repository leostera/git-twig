//! `git twig remove` is Git's removal. Git checks for changes and locks, drops
//! the registration, then deletes the checkout by absolute path, which fails
//! once a path exceeds the system limit (1024 bytes on macOS). Twig finishes
//! only that last step, and only for checkouts Git has already unregistered.
use crate::registry;
use anyhow::Result;
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
};

pub(super) fn run(cwd: &Path, args: &[OsString]) -> Result<i32> {
    let before = registry::read(cwd).ok();
    let output = crate::git(cwd)
        .arg("worktree")
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .output()?;
    let code = output.status.code().unwrap_or(1);
    if output.status.success() {
        std::io::stderr().write_all(&output.stderr)?;
        return Ok(code);
    }
    match finish(cwd, before) {
        Ok(finished) if !finished.is_empty() => {
            for path in finished {
                crate::trace(&format!(
                    "Git could not delete every path under {}; removed the rest",
                    path.display()
                ));
            }
            Ok(0)
        }
        result => {
            std::io::stderr().write_all(&output.stderr)?;
            if let Err(error) = result {
                eprintln!("git-twig: {error:#}");
            }
            Ok(code)
        }
    }
}

/// Delete checkouts Git unregistered but left on disk. Removal is relative to
/// open directories and never follows symbolic links.
fn finish(cwd: &Path, before: Option<Vec<registry::Registration>>) -> Result<Vec<PathBuf>> {
    let Some(before) = before else {
        return Ok(Vec::new());
    };
    let after = registry::read(cwd)?;
    let unregistered: Vec<_> = before
        .into_iter()
        .filter(|registration| after.iter().all(|kept| kept.path != registration.path))
        .collect();
    // One removal unregisters one checkout. Anything else means another
    // process changed registrations meanwhile; leave every path alone.
    let [registration] = unregistered.as_slice() else {
        return Ok(Vec::new());
    };
    let path = &registration.path;
    let mut finished = Vec::new();
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        fs::remove_dir_all(path).map_err(|error| {
            anyhow::anyhow!("could not finish removing {}: {error}", path.display())
        })?;
        finished.push(path.clone());
    }
    Ok(finished)
}
