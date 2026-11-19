//! Native CoW materialization with Git-owned linked-worktree bookkeeping.
// Adoption and existing-worktree sharing are Unix-only for now; on Windows their
// commands refuse with a reason, and their helpers go unused.
#[cfg_attr(not(unix), allow(dead_code, unused_imports))]
mod adopt;
mod checkout;
#[cfg(target_os = "macos")]
mod compact;
mod donor;
#[cfg(unix)]
mod file_bytes;
mod index;
mod native;
mod objects;
#[cfg_attr(not(unix), allow(dead_code))]
mod registry;
#[cfg_attr(not(unix), allow(dead_code))]
mod sharing;
mod snapshot;
mod staging;
mod tree;
#[cfg(windows)]
mod windows;

use anyhow::{Context, Result, ensure};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tree::{Entry, Kind};

/// Execute supported creation requests natively; delegate everything else to Git.
pub fn run(cwd: &Path, args: &[OsString]) -> Result<i32> {
    if args.first().is_some_and(|arg| arg == "adopt") {
        return adopt::run(cwd, &args[1..]);
    }
    ensure!(
        args.first().is_none_or(|arg| arg != "add"),
        "\'add\' has been removed; use \'git twig new\'"
    );
    // A remembered root supplies a destination only when one was omitted.
    // Both native preparation and fallback consume Git worktree arguments.
    let mut git_args = adopt::creation_args(cwd, args)?;
    if git_args.first().is_some_and(|arg| arg == "new") {
        git_args[0] = "add".into();
    }
    let args = &git_args;
    let mut staging = staging::Staging::default();
    let result = run_request(cwd, args, &mut staging);
    if matches!(result, Ok(0)) {
        staging.cleanup();
    }
    result
}

fn run_request(cwd: &Path, args: &[OsString], staging: &mut staging::Staging) -> Result<i32> {
    if let Some(request) = AddRequest::parse(args) {
        match prepare(cwd, &request, staging) {
            Ok(prepared) => return install(cwd, args, prepared),
            Err(error) => trace(&format!("stock Git: {error:#}")),
        }
    } else if args.first().is_some_and(|arg| arg == "add") {
        trace(
            "stock Git: creation options require Git; native creation supports -b or --detach, a destination, and an optional revision",
        );
    }
    Ok(git(cwd)
        .arg("worktree")
        .args(args)
        .status()?
        .code()
        .unwrap_or(1))
}

fn trace(message: &str) {
    if env::var_os("TWIG_TRACE").is_some() {
        eprintln!("git-twig: {message}");
    }
}

#[derive(Debug)]
struct AddRequest {
    branch: Option<OsString>,
    destination: PathBuf,
    revision: OsString,
}

impl AddRequest {
    fn parse(args: &[OsString]) -> Option<Self> {
        if args.first()? != "add" {
            return None;
        }
        let mut index = 1;
        let mut branch = None;
        if args.get(index)? == "-b" {
            let name = args.get(index + 1)?;
            if name.is_empty() || name.to_string_lossy().starts_with('-') {
                return None;
            }
            branch = Some(name.clone());
            index += 2;
        } else if matches!(args.get(index)?.to_str(), Some("--detach" | "-d")) {
            index += 1;
        } else {
            // Omitted branch has Git's branch-name inference semantics. Delegate for now.
            return None;
        }
        let destination = args.get(index)?;
        if destination.is_empty() || destination.to_string_lossy().starts_with('-') {
            return None;
        }
        index += 1;
        let revision = args.get(index).cloned().unwrap_or_else(|| "HEAD".into());
        if revision.to_str().is_none()
            || revision.to_string_lossy().starts_with('-')
            || args.len() > index + 1
        {
            return None;
        }
        Some(Self {
            branch,
            destination: destination.into(),
            revision,
        })
    }
}

struct Prepared {
    root: PathBuf,
    destination: PathBuf,
    stage: PathBuf,
    commit: String,
    entries: Vec<Entry>,
    policy: checkout::Policy,
}

fn prepare(cwd: &Path, request: &AddRequest, staging: &mut staging::Staging) -> Result<Prepared> {
    let started = std::time::Instant::now();
    ensure!(
        cfg!(any(target_os = "macos", target_os = "linux", windows)),
        "native CoW is supported only on macOS, Linux and Windows"
    );
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_ATTR_SOURCE",
        "GIT_REPLACE_REF_BASE",
    ] {
        ensure!(
            env::var_os(key).is_none(),
            "environment override {key} requires stock Git"
        );
    }
    let root = output_path(checked(git(cwd).args(["rev-parse", "--show-toplevel"]))?.stdout)?;
    let commit = text(
        checked(
            git(&root)
                .arg("rev-parse")
                .arg("--verify")
                .arg(format!("{}^{{commit}}", request.revision.to_string_lossy())),
        )?
        .stdout,
    )?;
    if let Some(branch) = &request.branch {
        ensure_new_branch(&root, branch)?;
    }
    ensure!(
        !config_true(&root, "core.sparseCheckout")
            && !config_true(&root, "core.sparseCheckoutCone"),
        "sparse checkout requires stock Git"
    );
    let destination = if request.destination.is_absolute() {
        request.destination.clone()
    } else {
        cwd.join(&request.destination)
    };
    ensure!(
        !destination.try_exists()? && fs::symlink_metadata(&destination).is_err(),
        "destination already exists: {:?}",
        destination
    );
    let parent = destination.parent().context("destination has no parent")?;
    ensure!(
        parent.is_dir(),
        "destination parent does not exist: {parent:?}"
    );
    let parent = native::canonicalize(parent)?;
    let destination = parent.join(destination.file_name().context("destination has no name")?);
    objects::ensure_literal_objects(&root)?;
    checkout::ensure_relocatable_worktree_config(&root)?;
    // Blob sizes bound verification work, but asking for them would fetch every
    // absent blob of a partial clone one at a time.
    let lazy = objects::fetches_lazily(&root)?;
    let listing = if lazy { "-rz" } else { "-rlz" };
    let entries = tree::parse(
        &checked(git(&root).args(["ls-tree", listing, "--full-tree", &commit]))?.stdout,
    )?;
    // Reference/index hooks run while Git registers or initializes the worktree.
    // They may themselves call Twig: never hold our registrar lock across them.
    let hook = output_path(checked(git(&root).args(["rev-parse", "--git-path", "hooks"]))?.stdout)?;
    let configured_hooks = git(&root)
        .args(["config", "--get-regexp", r"^hook\..*\.event$"])
        .output()?;
    let hooks = root.join(hook);
    for event in ["reference-transaction", "post-index-change"] {
        let configured = configured_hooks
            .stdout
            .split(|byte| byte.is_ascii_whitespace())
            .any(|word| word == event.as_bytes());
        ensure!(
            !configured && fs::symlink_metadata(hooks.join(event)).is_err(),
            "{event} hook requires stock Git to avoid reentrant registration locks"
        );
    }
    #[cfg(windows)]
    windows::ensure_block_cloning(&parent)?;
    let stage = staging.create(&root, &parent)?;
    let data = stage.join("data");
    fs::create_dir(&data)?;
    let index = stage.join("index");
    let checkout = checkout::Checkout::new(&root, &data, &index)?;
    checkout.initialize(&commit)?;
    let policy = checkout.policy(&entries)?;
    let modes = native::FileModes::new(&stage)?;
    // Listing peer Git metadata must share the registrar lock: another Twig may
    // otherwise be between creating its worktree directory and writing commondir.
    let mut donor = {
        let _registration_lock = lock_registration(&root)?;
        donor::choose(&root, &parent, &entries)?
    };
    if !policy.native_symlinks {
        donor.candidates.retain(|entry| entry.kind != Kind::Symlink);
    }
    trace(&format!(
        "donor: {:?}, {} matching target paths",
        donor.root,
        donor.candidates.len()
    ));
    let snapshot_started = std::time::Instant::now();
    let directories = snapshot::materialize(&donor.root, &data, &donor.candidates, &modes)?;
    let snapshot_elapsed = snapshot_started.elapsed();
    #[cfg(target_os = "macos")]
    trace(&format!("APFS directory clones: {directories}"));
    #[cfg(not(target_os = "macos"))]
    let _ = directories;
    let verification_started = std::time::Instant::now();
    let certified = checkout.repair(&root, &entries, &policy, lazy)?;
    checkout.record(&commit, &entries, &certified)?;
    trace(&format!(
        "native preparation: snapshot {:.3}s; raw verification/repair {:.3}s; total {:.3}s",
        snapshot_elapsed.as_secs_f64(),
        verification_started.elapsed().as_secs_f64(),
        started.elapsed().as_secs_f64()
    ));
    Ok(Prepared {
        root,
        destination,
        stage,
        commit,
        entries,
        policy,
    })
}

/// Git refuses an invalid or existing branch before registering anything. Leave
/// that refusal to Git now, rather than discover it after staging a checkout
/// that failure handling would then retain.
fn ensure_new_branch(root: &Path, branch: &OsString) -> Result<()> {
    let name = git(root)
        .args(["check-ref-format", "--branch"])
        .arg(branch)
        .output()?;
    ensure!(name.status.success(), "branch name {branch:?} is not valid");
    let reference = format!("refs/heads/{}", text(name.stdout)?);
    let exists = git(root)
        .args(["show-ref", "--verify", "--quiet", &reference])
        .status()?
        .success();
    ensure!(!exists, "branch {branch:?} already exists");
    Ok(())
}

fn install(cwd: &Path, args: &[OsString], prepared: Prepared) -> Result<i32> {
    #[cfg(target_os = "macos")]
    trace("APFS CoW: verified tracked files");
    #[cfg(target_os = "linux")]
    trace("Linux reflink CoW: verified tracked files");
    #[cfg(windows)]
    trace("Windows block clone CoW: verified tracked files");
    // Git enumerates peer worktree metadata while registering. Serialize Twig registrars
    // so none observes a peer between mkdir and writing commondir/HEAD.
    let registration_lock = lock_registration(&prepared.root)?;
    let data = prepared.stage.join("data");
    let index = prepared.stage.join("index");
    prepared.policy.ensure_same(
        &checkout::Checkout::new(&prepared.root, &data, &index)?.policy(&prepared.entries)?,
    )?;
    // Preserve Git's branch validation and registration. No fallback after this mutation.
    let status = git(cwd)
        .args(["worktree", "add", "--no-checkout"])
        .args(&args[1..])
        .status()?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }
    finish_install(&prepared).with_context(|| {
        format!(
            "partial worktree retained at {}; staging retained at {}; inspect before retrying",
            prepared.destination.display(),
            prepared.stage.display()
        )
    })?;
    // Do not hold the registry lock across user hooks (which may invoke Twig again).
    drop(registration_lock);
    let zero = "0".repeat(prepared.commit.len());
    let code = git(&prepared.destination)
        .args([
            "hook",
            "run",
            "--ignore-missing",
            "post-checkout",
            "--",
            &zero,
            &prepared.commit,
            "1",
        ])
        .status()?
        .code()
        .unwrap_or(1);
    Ok(code)
}

fn state_directory(root: &Path) -> Result<PathBuf> {
    let common_dir = output_path(
        checked(git(root).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))?
            .stdout,
    )?;
    let state_dir = common_dir.join("twigs");
    fs::create_dir_all(&state_dir)?;
    Ok(state_dir)
}

fn lock_registration(root: &Path) -> Result<fs::File> {
    let state_dir = state_directory(root)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_dir.join("registration.lock"))?;
    lock.lock()
        .context("cannot lock Git worktree registration")?;
    Ok(lock)
}

fn finish_install(prepared: &Prepared) -> Result<()> {
    // The ref may have moved while staging; never install files for a different HEAD.
    let actual = text(checked(git(&prepared.destination).args(["rev-parse", "HEAD"]))?.stdout)?;
    ensure!(
        actual == prepared.commit,
        "destination HEAD moved during creation"
    );
    let data = prepared.stage.join("data");
    let index = prepared.stage.join("index");
    prepared.policy.ensure_same(
        &checkout::Checkout::new(&prepared.destination, &data, &index)?
            .policy(&prepared.entries)?,
    )?;
    for entry in fs::read_dir(&data)? {
        let entry = entry?;
        let target = prepared.destination.join(entry.file_name());
        ensure!(
            fs::symlink_metadata(&target).is_err(),
            "destination changed during installation"
        );
        fs::rename(entry.path(), target)?;
    }
    // The staged index already records what verification read. Installing it,
    // rather than building an empty one, spares the first status a second read.
    let git_dir = output_path(
        checked(git(&prepared.destination).args(["rev-parse", "--absolute-git-dir"]))?.stdout,
    )?;
    for name in ["index", "index.lock"] {
        ensure!(
            fs::symlink_metadata(git_dir.join(name)).is_err(),
            "destination index changed during installation"
        );
    }
    fs::rename(&index, git_dir.join("index"))?;
    // Moving a top-level entry changes its own ctime. Git rehashes only those.
    checked(git(&prepared.destination).args([
        "-c",
        "submodule.recurse=false",
        "update-index",
        "-q",
        "--refresh",
    ]))?;
    trace(&format!("shared repository: {}", prepared.root.display()));
    Ok(())
}

fn config_true(root: &Path, key: &str) -> bool {
    git(root)
        .args(["config", "--bool", "--get", key])
        .output()
        .is_ok_and(|result| result.stdout == b"true\n")
}

fn git(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd);
    command
}

/// Drain Git output while feeding input, so large batches cannot fill both pipes.
fn checked_with_input(command: &mut Command, input: Vec<u8>) -> Result<Output> {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().context("missing Git input pipe")?;
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output();
    let written = writer
        .join()
        .map_err(|_| anyhow::anyhow!("Git input writer panicked"))?;
    let output = output?;
    ensure!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    written?;
    Ok(output)
}

fn checked(command: &mut Command) -> Result<Output> {
    let output = command.output().context("cannot execute Git")?;
    ensure!(
        output.status.success(),
        "Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

fn text(mut bytes: Vec<u8>) -> Result<String> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok(String::from_utf8(bytes)?)
}

fn output_path(mut bytes: Vec<u8>) -> Result<PathBuf> {
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok(native::git_path(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(words: &[&str]) -> bool {
        AddRequest::parse(&words.iter().map(OsString::from).collect::<Vec<_>>()).is_some()
    }
    #[test]
    fn parses_only_the_supported_subset() {
        assert!(parse(&["add", "-b", "feature", "../feature"]));
        assert!(parse(&["add", "--detach", "../feature", "HEAD"]));
        assert!(parse(&["add", "-d", "../feature", "HEAD~1"]));
        for words in [
            vec!["add"],
            vec!["add", "-b"],
            vec!["add", "-B", "a", "b"],
            vec!["add", "-b", "a", "b", "HEAD", "extra"],
            vec!["list"],
            vec!["add", "-b", "a", "--help"],
        ] {
            assert!(!parse(&words));
        }
    }
}
