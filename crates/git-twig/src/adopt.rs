//! Confirmed layout adoption. Relocation never reconstructs checkout contents.
use anyhow::{Context, Result, bail, ensure};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

use crate::registry::{self, Checkout, Registration, git};

const ROOT_KEY: &str = "twig.worktreesDir";
const USAGE: &str = "git twig adopt [--dry-run] [--yes] [--compact [--offline] | --move-only] [--worktrees-dir=<root>] [<worktree>]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sharing {
    Automatic,
    Compact,
    MoveOnly,
}

/// Constructed only after this module receives explicit offline consent. This
/// records the owner\'s assertion, not proof of exclusive kernel ownership.
#[cfg(target_os = "macos")]
pub(super) struct OfflineConsent {
    _confirmed: (),
}

struct Request {
    root: Option<PathBuf>,
    target: Option<PathBuf>,
    dry_run: bool,
    yes: bool,
    sharing: Sharing,
    offline: bool,
}
impl Request {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut request = Self {
            root: None,
            target: None,
            dry_run: false,
            yes: false,
            sharing: Sharing::Automatic,
            offline: false,
        };
        let mut args = args.iter();
        let mut positional = false;
        while let Some(arg) = args.next() {
            if !positional && arg == "--" {
                positional = true;
                continue;
            }
            if !positional && arg == "--dry-run" {
                ensure!(!request.dry_run, "duplicate --dry-run");
                request.dry_run = true;
            } else if !positional && arg == "--yes" {
                ensure!(!request.yes, "duplicate --yes");
                request.yes = true;
            } else if !positional && (arg == "--compact" || arg == "--move-only") {
                ensure!(
                    request.sharing == Sharing::Automatic,
                    "choose --compact or --move-only only once"
                );
                request.sharing = if arg == "--compact" {
                    Sharing::Compact
                } else {
                    Sharing::MoveOnly
                };
            } else if !positional && arg == "--offline" {
                ensure!(!request.offline, "duplicate --offline");
                request.offline = true;
            } else if !positional && arg == "--worktrees-dir" {
                ensure!(request.root.is_none(), "duplicate --worktrees-dir");
                let value = args.next().context("--worktrees-dir requires a path")?;
                ensure!(
                    !value.is_empty() && !value.as_encoded_bytes().starts_with(b"-"),
                    "--worktrees-dir requires a path, not another option; use --worktrees-dir=<path> for a hyphen-prefixed path"
                );
                request.root = Some(value.into());
            } else if !positional && arg.as_encoded_bytes().starts_with(b"--worktrees-dir=") {
                ensure!(request.root.is_none(), "duplicate --worktrees-dir");
                let value = &arg.as_encoded_bytes()[16..];
                ensure!(!value.is_empty(), "empty worktrees directory");
                request.root = Some(crate::native::os_string(value.to_vec()).into());
            } else {
                ensure!(
                    positional || !arg.as_encoded_bytes().starts_with(b"-"),
                    "unknown option {arg:?}; {USAGE}"
                );
                ensure!(!arg.is_empty() && request.target.is_none(), "{USAGE}");
                request.target = Some(arg.into());
            }
        }
        ensure!(
            !(request.yes && request.dry_run),
            "--yes and --dry-run are mutually exclusive"
        );
        ensure!(
            !request.offline || request.sharing == Sharing::Compact,
            "--offline requires --compact"
        );
        ensure!(
            !cfg!(target_os = "macos")
                || !request.yes
                || request.sharing != Sharing::Compact
                || request.offline,
            "macOS --compact --yes requires --offline: save work and close agents/editors/builds first"
        );
        Ok(request)
    }
}

/// Explicit v0.5 sharing modes keep their existing file-identity contract. The
/// default command and its layout preview are the new relocation interface.
pub(super) fn run(cwd: &Path, args: &[OsString]) -> Result<i32> {
    if args
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--prepare" || a == "--dedupe")
    {
        return crate::sharing::run(cwd, args);
    }
    let request = Request::parse(args)?;
    #[cfg(any(unix, windows))]
    return layout::run(cwd, request);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cwd, request);
        bail!("layout adoption requires macOS, Linux or Windows");
    }
}

pub(super) fn saved_root(cwd: &Path) -> Result<Option<PathBuf>> {
    registry::ensure_environment()?;
    let output = git(cwd)
        .args(["config", "--local", "--null", "--get", ROOT_KEY])
        .output()?;
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    ensure!(
        output.status.success(),
        "cannot read saved worktrees directory"
    );
    let bytes = output
        .stdout
        .strip_suffix(b"\0")
        .context("malformed worktrees directory config")?;
    let path = PathBuf::from(crate::native::os_string(bytes.to_vec()));
    ensure!(
        path.is_absolute(),
        "saved worktrees directory must be absolute"
    );
    Ok(Some(path))
}

fn slot(name: &OsStr) -> Result<OsString> {
    let mut encoded = String::new();
    for &byte in name.as_encoded_bytes() {
        match byte {
            b'/' => encoded.push('-'),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                encoded.push(byte as char)
            }
            _ => {
                use std::fmt::Write;
                write!(encoded, "%{byte:02X}")?;
            }
        }
    }
    ensure!(
        !encoded.is_empty() && encoded != "." && encoded != "..",
        "invalid worktree slot"
    );
    Ok(encoded.into())
}

/// Omitted destinations are inferred only for this unambiguous form. Explicit
/// destinations (and their existing Git semantics) are never reinterpreted.
pub(super) fn creation_args(cwd: &Path, args: &[OsString]) -> Result<Vec<OsString>> {
    let mut args = args.to_vec();
    if args.len() == 3
        && args[0] == "new"
        && args[1] == "-b"
        && let Some(root) = saved_root(cwd)?
    {
        args.push(root.join(slot(&args[2])?).into_os_string());
    }
    Ok(args)
}

#[cfg(any(unix, windows))]
mod layout {
    use super::*;
    use crate::native::canonicalize;
    use std::{
        collections::HashSet,
        fs::{File, OpenOptions},
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Repository {
        main: PathBuf,
        records: Vec<Registration>,
    }
    impl Repository {
        fn read(cwd: &Path) -> Result<Self> {
            let records = registry::read(cwd)?;
            let first = records.first().context("repository has no main checkout")?;
            ensure!(
                !matches!(first.checkout, Checkout::Bare),
                "bare repositories have no main checkout to adopt"
            );
            let main = canonicalize(&first.path)?;
            ensure!(
                fs::symlink_metadata(main.join(".git"))?.is_dir(),
                "main checkout must own its .git directory; external or symlinked Git metadata is unsupported"
            );
            Ok(Self { main, records })
        }
        fn recommended_root(&self, single: bool) -> Result<PathBuf> {
            let parent = self.main.parent().context("main checkout has no parent")?;
            if self.main.file_name() == Some(OsStr::new("main")) {
                return Ok(parent.into());
            }
            if single {
                let mut name = self
                    .main
                    .file_name()
                    .context("main checkout lacks a name")?
                    .to_os_string();
                name.push("-worktrees");
                return Ok(parent.join(name));
            }
            Ok(self.main.clone())
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    struct Identity {
        device: u64,
        inode: u64,
    }
    #[cfg(unix)]
    fn identity(path: &Path) -> Result<Identity> {
        use std::os::unix::fs::MetadataExt;
        let stat = fs::symlink_metadata(path)?;
        ensure!(stat.is_dir(), "checkout is not a real directory: {path:?}");
        Ok(Identity {
            device: stat.dev(),
            inode: stat.ino(),
        })
    }
    #[cfg(windows)]
    fn identity(path: &Path) -> Result<Identity> {
        let id = crate::windows::identity(path)?;
        ensure!(id.directory, "checkout is not a real directory: {path:?}");
        Ok(Identity {
            device: id.volume.into(),
            inode: id.index,
        })
    }
    /// The filesystem holding a path, followed through links like `metadata`.
    fn device(path: &Path) -> Result<u64> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(fs::metadata(path)?.dev())
        }
        #[cfg(windows)]
        Ok(crate::windows::volume_identity(&canonicalize(path)?)?.into())
    }
    struct Relocation {
        from: PathBuf,
        to: PathBuf,
        identity: Identity,
    }
    struct Plan {
        repository: Repository,
        root: PathBuf,
        main_destination: PathBuf,
        selected: Option<PathBuf>,
        moves: Vec<Relocation>,
    }

    /// Canonicalize the existing prefix, preserving nonexistent final components.
    fn destination(path: &Path) -> Result<PathBuf> {
        let mut prefix = path;
        let mut suffix = Vec::new();
        while !prefix.try_exists()? {
            // Dangling symlinks must not be mistaken for vacant paths.
            ensure!(
                fs::symlink_metadata(prefix).is_err(),
                "occupied destination: {prefix:?}"
            );
            suffix.push(
                prefix
                    .file_name()
                    .context("destination has an unresolved parent component")?
                    .to_owned(),
            );
            prefix = prefix
                .parent()
                .context("destination has no existing ancestor")?;
        }
        let mut result = canonicalize(prefix)?;
        ensure!(result.is_dir(), "destination ancestor is not a directory");
        for name in suffix.into_iter().rev() {
            result.push(name);
        }
        Ok(result)
    }
    fn existing_ancestor(mut path: &Path) -> Result<&Path> {
        while !path.try_exists()? {
            path = path.parent().context("no destination ancestor")?;
        }
        Ok(path)
    }
    fn ensure_repository_links(checkout: &Path, main: &Path) -> Result<()> {
        let root = crate::output_path(
            crate::checked(git(checkout).args(["rev-parse", "--show-toplevel"]))?.stdout,
        )?;
        let common = crate::output_path(
            crate::checked(git(checkout).args([
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ]))?
            .stdout,
        )?;
        ensure!(
            canonicalize(root)? == checkout && canonicalize(common)? == main.join(".git"),
            "registration points at another checkout/repository: {checkout:?}"
        );
        let git_dir = crate::output_path(
            crate::checked(git(checkout).args(["rev-parse", "--absolute-git-dir"]))?.stdout,
        )?;
        for directory in [git_dir, main.join(".git")] {
            for name in [
                "index.lock",
                "HEAD.lock",
                "config.lock",
                "packed-refs.lock",
                "shallow.lock",
            ] {
                ensure!(
                    fs::symlink_metadata(directory.join(name)).is_err(),
                    "active Git lock {:?}; stop its owner before adopting",
                    directory.join(name)
                );
            }
        }
        Ok(())
    }
    fn ensure_relocatable(from: &Path) -> Result<()> {
        let core = git(from)
            .args(["config", "--get", "core.worktree"])
            .output()?;
        ensure!(
            core.status.code() == Some(1),
            "checkout has core.worktree configuration; relocation is unsupported: {from:?}"
        );
        let tracked = crate::checked(git(from).args(["ls-files", "--stage", "-z"]))?;
        ensure!(
            !tracked
                .stdout
                .split(|b| *b == 0)
                .any(|r| r.starts_with(b"160000 ")),
            "worktrees with submodule entries cannot yet be relocated: {from:?}"
        );
        Ok(())
    }
    fn selected_path(cwd: &Path, target: &Option<PathBuf>) -> Result<Option<PathBuf>> {
        target
            .as_ref()
            .map(|p| canonicalize(cwd.join(p)))
            .transpose()
    }
    impl Plan {
        fn build(repository: Repository, root: PathBuf, selected: Option<&Path>) -> Result<Self> {
            let root = destination(&root)?;
            let mut moves = Vec::new();
            let mut names = HashSet::new();
            let mut found = selected.is_none();
            let mut main_destination = repository.main.clone();
            let device = identity(&repository.main)?.device;
            ensure!(
                self::device(existing_ancestor(&root)?)? == device,
                "worktrees directory is on a different filesystem; cross-device relocation is unsupported"
            );
            ensure!(
                identity(repository.main.parent().context("main has no parent")?)?.device == device,
                "main checkout is a mount point; cannot park it safely"
            );
            for (i, record) in repository.records.iter().enumerate() {
                let from = canonicalize(&record.path).with_context(|| {
                    format!(
                        "missing registered worktree {:?}; repair it first",
                        record.path
                    )
                })?;
                ensure!(
                    from == record.path,
                    "registration uses a symlink/alias path; repair it first: {:?}",
                    record.path
                );
                ensure!(
                    record.locked.is_none()
                        && record.prunable.is_none()
                        && record.unknown.is_empty(),
                    "locked, prunable or unknown registration: {:?}; resolve it explicitly first",
                    record.path
                );
                ensure_repository_links(&from, &repository.main)?;
                let chosen = selected.is_none_or(|p| p == from);
                if !chosen {
                    continue;
                }
                found = true;
                ensure!(
                    !matches!(record.checkout, Checkout::Bare),
                    "cannot adopt a bare registration"
                );
                let name = if i == 0 {
                    OsString::from("main")
                } else {
                    match &record.checkout {
                        Checkout::Branch { reference, .. } => {
                            let bytes = crate::native::path_bytes(Path::new(reference));
                            let bytes = bytes
                                .strip_prefix(b"refs/heads/")
                                .context("unsupported branch reference")?;
                            slot(&crate::native::os_string(bytes.to_vec()))?
                        }
                        Checkout::Detached { .. } => {
                            slot(from.file_name().context("detached worktree lacks a name")?)?
                        }
                        Checkout::Bare => unreachable!(),
                    }
                };
                ensure!(
                    i == 0 || !name.as_encoded_bytes().eq_ignore_ascii_case(b"main"),
                    "the main directory slot is reserved for the original clone"
                );
                ensure!(
                    names.insert(name.to_string_lossy().to_ascii_lowercase()),
                    "worktree directory collision at {name:?}; choose distinct branches or adopt separately"
                );
                let to = root.join(name);
                if i == 0 {
                    main_destination = to.clone();
                }
                let id = identity(&from)?;
                ensure!(
                    id.device == device,
                    "worktree is on a different filesystem: {from:?}"
                );
                moves.push(Relocation {
                    from,
                    to,
                    identity: id,
                });
            }
            ensure!(
                found,
                "target is not a registered worktree root of this repository"
            );
            // Even an already-correct child slot must be parked when its parent
            // moves; otherwise moving main would implicitly carry that child away.
            let moving_parents: Vec<_> = moves
                .iter()
                .filter(|m| m.from != m.to)
                .map(|m| m.from.clone())
                .collect();
            moves.retain(|m| {
                m.from != m.to
                    || moving_parents
                        .iter()
                        .any(|p| m.from != *p && m.from.starts_with(p))
            });
            for moving in &moves {
                ensure!(
                    !repository
                        .main
                        .parent()
                        .context("main has no parent")?
                        .starts_with(&moving.from),
                    "recovery directory would lie inside a moving checkout; this overlapping layout is unsupported"
                );
                ensure!(
                    !root.starts_with(&moving.from)
                        || (moving.from == repository.main && root == moving.from),
                    "worktrees directory must be outside moving checkouts (or exactly the original main path)"
                );
                let vacated = moves.iter().any(|m| moving.to.starts_with(&m.from));
                ensure!(
                    fs::symlink_metadata(&moving.to).is_err() || vacated,
                    "destination already exists: {:?}",
                    moving.to
                );
                ensure_relocatable(&moving.from)?;
                for record in &repository.records {
                    if record.path != moving.from && record.path.starts_with(&moving.from) {
                        ensure!(
                            moves.iter().any(|m| m.from == record.path),
                            "moving {:?} would implicitly move unselected worktree {:?}; adopt all worktrees instead",
                            moving.from,
                            record.path
                        );
                    }
                    ensure!(
                        !moving.to.starts_with(&record.path)
                            || moves.iter().any(|m| m.from == record.path),
                        "destination would remain nested inside an unmoved worktree: {:?}",
                        moving.to
                    );
                }
            }
            // Children must leave before parents are parked. Main is installed first later.
            moves.sort_by_key(|m| std::cmp::Reverse(m.from.components().count()));
            Ok(Self {
                repository,
                root,
                main_destination,
                selected: selected.map(Path::to_path_buf),
                moves,
            })
        }
        fn targets(&self, relocated: bool) -> Vec<PathBuf> {
            self.repository
                .records
                .iter()
                .skip(1)
                .filter(|r| self.selected.as_ref().is_none_or(|p| p == &r.path))
                .map(|r| {
                    if relocated {
                        self.moves
                            .iter()
                            .find(|m| m.from == r.path)
                            .map_or_else(|| r.path.clone(), |m| m.to.clone())
                    } else {
                        r.path.clone()
                    }
                })
                .collect()
        }
        fn show(&self, sharing: Sharing) {
            println!("Worktrees directory: {:?}", self.root);
            for record in &self.repository.records {
                if let Some(m) = self.moves.iter().find(|m| m.from == record.path) {
                    println!("  {:?} -> {:?}", m.from, m.to);
                } else {
                    println!("  {:?} (unchanged)", record.path);
                }
            }
            println!(
                "{} worktree(s) to move. Branches, index and working files are preserved.",
                self.moves.len()
            );
            println!(
                "Coordinate active agents/editors first: their stored absolute paths will not be updated."
            );
            if sharing == Sharing::MoveOnly {
                println!("Move-only: existing file sharing will remain unchanged.");
            } else if cfg!(target_os = "macos") {
                println!(
                    "macOS: move-only sharing will remain unchanged; optional compaction replaces file identities and requires saved, closed worktrees."
                );
            } else if cfg!(windows) {
                println!(
                    "Windows: after relocation, attempt safe in-place deduplication on ReFS; physical savings are not measured."
                );
            } else {
                println!(
                    "Linux: after relocation, attempt safe in-place deduplication; physical savings are not measured."
                );
            }
        }
    }

    fn prompt(message: &str) -> Result<String> {
        ensure!(
            io::stdin().is_terminal(),
            "interactive confirmation requires a terminal; preview with --dry-run, then use --yes with an explicit/saved --worktrees-dir after review"
        );
        eprint!("{message}");
        io::stderr().flush()?;
        let mut line = String::new();
        ensure!(
            io::stdin().read_line(&mut line)? > 0,
            "input closed; adoption canceled"
        );
        Ok(line.trim_end_matches(['\r', '\n']).into())
    }
    pub(super) fn run(cwd: &Path, request: Request) -> Result<i32> {
        ensure!(
            cfg!(any(target_os = "macos", target_os = "linux", windows)),
            "layout adoption requires macOS, Linux or Windows"
        );
        let repository = Repository::read(cwd)?;
        let selected = selected_path(cwd, &request.target)?;
        println!("Found {} registered worktree(s):", repository.records.len());
        for record in &repository.records {
            println!("  {:?}", record.path);
        }
        let saved = saved_root(cwd)?;
        let root = match request.root.or(saved) {
            Some(path) => cwd.join(path),
            None => {
                let default = repository.recommended_root(selected.is_some())?;
                if request.dry_run {
                    default
                } else {
                    ensure!(
                        !request.yes,
                        "first adoption requires --worktrees-dir with --yes"
                    );
                    let answer = prompt(&format!("Worktrees directory [{default:?}]: "))?;
                    if answer.is_empty() {
                        default
                    } else {
                        cwd.join(answer)
                    }
                }
            }
        };
        let plan = Plan::build(repository, root, selected.as_deref())?;
        plan.show(request.sharing);
        preview_sharing(&plan, request.sharing)?;
        if request.dry_run {
            println!("Dry run: no directories, configuration or files changed.");
            if request.sharing == Sharing::Automatic
                && let Some(target) = selected
            {
                let donor = canonicalize(crate::output_path(
                    crate::checked(git(cwd).args(["rev-parse", "--show-toplevel"]))?.stdout,
                )?)?;
                if donor != target && target != plan.repository.main {
                    crate::sharing::run(cwd, &["--dry-run".into(), target.into_os_string()])?;
                }
            }
            return Ok(0);
        }
        let sharing = choose_sharing(request.sharing, request.yes)?;
        if cfg!(target_os = "macos") && sharing == Sharing::Compact && !request.offline {
            ensure!(!request.yes, "macOS --compact --yes requires --offline");
            println!(
                "Offline compaction replaces file identities and retires verified old copies. Save work and CLOSE all agents, editors, builds and servers using donor/target worktrees. Paused processes may still hold files open."
            );
            if prompt("Type offline to acknowledge all these worktrees are closed: ")? != "offline"
            {
                println!("Adoption canceled; nothing changed.");
                return Ok(0);
            }
        }
        if !request.yes
            && !matches!(
                prompt("Apply the displayed layout and sharing plan? [y/N] ")?.as_str(),
                "y" | "Y" | "yes" | "YES"
            )
        {
            println!("Adoption canceled; nothing changed.");
            return Ok(0);
        }
        let lock = crate::lock_registration(&plan.repository.main)?;
        ensure!(
            registry::read(&plan.repository.main)? == plan.repository.records,
            "registration changed after preview; rerun adopt"
        );
        for m in &plan.moves {
            ensure!(
                identity(&m.from)? == m.identity,
                "checkout identity changed after preview: {:?}",
                m.from
            );
        }
        #[cfg(target_os = "macos")]
        let mut compaction = if sharing == Sharing::Compact {
            Some(crate::compact::Session::new(
                plan.repository
                    .main
                    .parent()
                    .context("main has no parent")?,
                OfflineConsent { _confirmed: () },
            )?)
        } else {
            None
        };
        // Windows cannot move a directory that is any process's working
        // directory, this one included. Twig's own Git commands name theirs.
        std::env::set_current_dir(
            plan.repository
                .main
                .parent()
                .context("main has no parent")?,
        )?;
        // Nor one holding an open file, however it is shared: the lock lives in
        // main's Git directory. Release it; a Twig registering meanwhile holds
        // that lock file open, so moving main fails and every move is undone.
        #[cfg(windows)]
        drop(lock);
        #[cfg(not(windows))]
        let _lock = lock;
        apply(&plan)?;
        crate::checked(
            git(&plan.main_destination)
                .args(["config", "--local", ROOT_KEY])
                .arg(&plan.root),
        ).context("layout was adopted, but saving the root failed; worktree files remain at the displayed destinations")?;
        println!("Layout adopted. Saved worktrees directory: {:?}", plan.root);
        #[cfg(target_os = "macos")]
        if let Some(session) = compaction.as_mut() {
            for target in plan.targets(true) {
                session
                    .compact(&plan.main_destination, &target)?
                    .show(&target, false);
            }
        }
        if sharing != Sharing::MoveOnly {
            share_after_move(&plan);
        }
        Ok(0)
    }

    fn choose_sharing(choice: Sharing, yes: bool) -> Result<Sharing> {
        if choice != Sharing::Automatic {
            return Ok(choice);
        }
        if !cfg!(target_os = "macos") {
            return Ok(Sharing::Compact);
        }
        if yes {
            return Ok(Sharing::MoveOnly);
        }
        Ok(
            if matches!(
                prompt(
                    "Also compact matching files? Requires an offline maintenance window [y/N] "
                )?
                .as_str(),
                "y" | "Y" | "yes" | "YES"
            ) {
                Sharing::Compact
            } else {
                Sharing::MoveOnly
            },
        )
    }
    fn preview_sharing(plan: &Plan, sharing: Sharing) -> Result<()> {
        if sharing == Sharing::MoveOnly {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        for target in plan.targets(false) {
            crate::compact::preview(&plan.repository.main, &target)?.show(&target, true);
        }
        #[cfg(any(target_os = "linux", windows))]
        if sharing == Sharing::Compact {
            for target in plan.targets(false) {
                crate::sharing::run(
                    &plan.repository.main,
                    &["--dry-run".into(), target.into_os_string()],
                )?;
            }
        }
        Ok(())
    }
    fn share_after_move(plan: &Plan) {
        if !cfg!(any(target_os = "linux", windows)) {
            return;
        }
        for record in &plan.repository.records {
            if plan.selected.as_ref().is_some_and(|p| p != &record.path) {
                continue;
            }
            let target = plan
                .moves
                .iter()
                .find(|m| m.from == record.path)
                .map_or(&record.path, |m| &m.to);
            if target == &plan.main_destination {
                continue;
            }
            match crate::sharing::run(
                &plan.main_destination,
                &["--dedupe".into(), target.as_os_str().into()],
            ) {
                Ok(_) => {}
                Err(error) => eprintln!(
                    "git-twig: layout adopted, sharing not completed for {:?}: {error:#}; existing files were not replaced",
                    target
                ),
            }
        }
    }

    enum Undo {
        Rename { from: PathBuf, to: PathBuf },
        Directory(PathBuf),
    }
    struct Transaction {
        journal: File,
        path: PathBuf,
        undo: Vec<Undo>,
    }
    /// Unix modes keep the journal private. On Windows it inherits the ACL of
    /// the directory holding the checkouts, which already guards their files.
    fn private_journal(path: &Path) -> Result<File> {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut directory = fs::DirBuilder::new();
        let mut journal = OpenOptions::new();
        journal.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            directory.mode(0o700);
            journal.mode(0o600);
        }
        directory.create(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                fs::metadata(path)?.mode() & 0o777 == 0o700,
                "cannot protect recovery journal: {path:?}"
            );
        }
        Ok(journal.open(path.join("journal.nul"))?)
    }
    impl Transaction {
        fn new(plan: &Plan) -> Result<Self> {
            let parent = plan
                .repository
                .main
                .parent()
                .context("main checkout has no parent")?;
            let path = parent.join(format!(
                ".twig-adopt-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
            ));
            let journal = private_journal(&path)?;
            let mut transaction = Self {
                journal,
                path,
                undo: Vec::new(),
            };
            transaction.record("twig-adopt-layout-v1", &plan.repository.main, &plan.root)?;
            for m in &plan.moves {
                transaction.record("plan", &m.from, &m.to)?;
            }
            println!(
                "Recovery journal retained at {:?}",
                transaction.path.join("journal.nul")
            );
            transaction.probe_rename().with_context(|| format!("atomic no-overwrite directory renames are unsupported; no worktrees moved; journal retained at {:?}", transaction.path))?;
            Ok(transaction)
        }
        fn probe_rename(&mut self) -> Result<()> {
            let from = self.path.join("probe-from");
            let occupied = self.path.join("probe-occupied");
            let vacant = self.path.join("probe-vacant");
            fs::create_dir(&from)?;
            fs::create_dir(&occupied)?;
            match rename_exclusive(&from, &occupied) {
                Err(error)
                    if error
                        .downcast_ref::<io::Error>()
                        .is_some_and(|e| e.kind() == io::ErrorKind::AlreadyExists) => {}
                Err(error) => return Err(error),
                Ok(()) => bail!("filesystem ignored exclusive rename for an occupied directory"),
            }
            rename_exclusive(&from, &vacant)?;
            // These are newly-created, empty probe directories, never user paths.
            fs::remove_dir(&occupied)?;
            fs::remove_dir(&vacant)?;
            self.record("rename-capabilities-verified", &from, &vacant)
        }
        fn record(&mut self, operation: &str, from: &Path, to: &Path) -> Result<()> {
            for field in [
                operation.as_bytes().to_vec(),
                crate::native::path_bytes(from),
                crate::native::path_bytes(to),
            ] {
                self.journal.write_all(&field)?;
                self.journal.write_all(b"\0")?;
            }
            self.journal.sync_all()?;
            Ok(())
        }
        fn rename(&mut self, from: &Path, to: &Path) -> Result<()> {
            self.record("rename-intent", from, to)?;
            rename_exclusive(from, to).with_context(|| {
                if cfg!(windows) {
                    format!(
                        "cannot move {from:?} to {to:?}; Windows cannot move a directory while a program has a file open in it or uses it as its current directory, so close editors, terminals and builds there first"
                    )
                } else {
                    format!("cannot move {from:?} to {to:?}")
                }
            })?;
            self.undo.push(Undo::Rename {
                from: from.into(),
                to: to.into(),
            });
            self.record("renamed", from, to)
        }
        fn directory(&mut self, path: &Path) -> Result<()> {
            if path.try_exists()? {
                ensure!(
                    fs::symlink_metadata(path)?.is_dir(),
                    "not a directory: {path:?}"
                );
                return Ok(());
            }
            if let Some(parent) = path.parent() {
                self.directory(parent)?;
            }
            self.record("mkdir-intent", path, path)?;
            fs::create_dir(path)?;
            self.undo.push(Undo::Directory(path.into()));
            self.record("created-directory", path, path)
        }
        fn rollback(&mut self) -> Result<()> {
            while let Some(action) = self.undo.pop() {
                match action {
                    Undo::Rename { from, to } => {
                        self.record("rollback-intent", &to, &from)?;
                        rename_exclusive(&to, &from)?;
                        self.record("rolled-back", &to, &from)?;
                    }
                    Undo::Directory(path) => {
                        fs::remove_dir(path)?;
                    }
                }
            }
            Ok(())
        }
    }

    fn repair(main: &Path, peers: impl Iterator<Item = PathBuf>) -> Result<()> {
        crate::checked(
            git(main)
                .args(["worktree", "repair", "--"])
                .args(peers.filter(|p| p != main)),
        )?;
        Ok(())
    }
    fn apply(plan: &Plan) -> Result<()> {
        if plan.moves.is_empty() {
            return Ok(());
        }
        let mut transaction = Transaction::new(plan)?;
        let result = (|| -> Result<()> {
            let mut parked = Vec::new();
            for (i, m) in plan.moves.iter().enumerate() {
                ensure!(
                    identity(&m.from)? == m.identity,
                    "checkout changed before move: {:?}",
                    m.from
                );
                let temporary = transaction.path.join(format!("checkout-{i}"));
                transaction.rename(&m.from, &temporary)?;
                parked.push((m, temporary));
            }
            transaction.directory(&plan.root)?;
            parked.sort_by_key(|(m, _)| m.to != plan.main_destination);
            for (m, temporary) in parked {
                transaction.rename(&temporary, &m.to)?;
            }
            let paths = plan.repository.records.iter().map(|r| {
                plan.moves
                    .iter()
                    .find(|m| m.from == r.path)
                    .map_or_else(|| r.path.clone(), |m| m.to.clone())
            });
            repair(&plan.main_destination, paths)?;
            transaction.record(
                "layout-complete",
                &plan.repository.main,
                &plan.main_destination,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            let recovery = transaction.rollback().and_then(|_| {
                repair(
                    &plan.repository.main,
                    plan.repository.records.iter().map(|r| r.path.clone()),
                )
            });
            match recovery {
                Ok(()) => bail!(
                    "adoption failed and original layout was restored: {error:#}; journal retained at {:?}",
                    transaction.path
                ),
                Err(rollback) => bail!(
                    "adoption failed: {error:#}; recovery incomplete: {rollback:#}; do not delete anything; recovery journal and parked files retained at {:?}",
                    transaction.path
                ),
            }
        }
        Ok(())
    }

    /// Never use ordinary rename here: a destination appearing after preview
    /// must cause failure, not replacement of an existing directory or symlink.
    #[cfg(windows)]
    fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
        crate::windows::rename_exclusive(from, to).context("atomic no-replace rename failed")
    }
    #[cfg(unix)]
    fn rename_exclusive(from: &Path, to: &Path) -> Result<()> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        #[cfg(target_os = "macos")]
        let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let result = {
            let _ = (from, to);
            bail!("atomic no-replace relocation unavailable")
        };
        // SAFETY: paths are NUL-terminated and both APIs atomically refuse an occupied destination.
        if result != 0 {
            return Err(io::Error::last_os_error()).context("atomic no-replace rename failed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_layout_options_before_any_effects() -> Result<()> {
        let r = Request::parse(&[
            "--worktrees-dir=/tmp/repo".into(),
            "--dry-run".into(),
            "a".into(),
        ])?;
        assert_eq!(r.root, Some(PathBuf::from("/tmp/repo")));
        assert!(r.dry_run);
        assert_eq!(r.target, Some(PathBuf::from("a")));
        for words in [
            vec!["--layout=global"],
            vec!["--compact", "--move-only"],
            vec!["--offline"],
            vec!["--worktrees-dir", "--compact", "--yes"],
            vec!["--yes", "--dry-run"],
            vec!["a", "b"],
            vec!["--worktrees-dir="],
            vec!["--worktrees-dir", "a", "--worktrees-dir=b"],
        ] {
            assert!(Request::parse(&words.iter().map(OsString::from).collect::<Vec<_>>()).is_err());
        }
        Ok(())
    }
    #[test]
    fn slots_are_literal_safe_components_not_paths() -> Result<()> {
        assert_eq!(slot(OsStr::new("feat/x"))?, "feat-x");
        assert_eq!(slot(OsStr::new("space name"))?, "space%20name");
        assert!(slot(OsStr::new("..")).is_err());
        Ok(())
    }
}
