use std::ffi::OsString;

const ROOT: &str = "git twig — lightweight Git worktrees

Usage: git twig <command> [options]

Commands:
  new       Create a linked worktree; use -b <branch> or --detach
  adopt     Preview and confirm a sibling layout for existing worktrees
  list      List worktrees (--porcelain for machine-readable output)
  remove    Remove a worktree
  move      Move a worktree
  lock      Protect a worktree registration from pruning/moving/removal
  unlock    Remove that protection
  prune     Remove stale registrations (--dry-run to preview)
  repair    Repair links after worktrees were moved manually
  skill     Print the bundled agent skill

Examples:
  git -C main twig new -b feat/x ../feat-x HEAD
  git twig list
  git twig skill

Options:
  -h, --help  Show short help
  --version   Show the installed version

Use git twig <command> -h for command help, even outside a repository.
Git reserves git twig --help for the installed manual; use git twig -h
or git-twig --help if no manual is installed.

Native CoW reuses matching files from nearby worktrees on APFS, Linux reflinks
or Windows ReFS and Dev Drives.
Other requests and unsupported worktree options use stock Git. Requires Git 2.40+.
Set TWIG_TRACE=1 to report backend selection to stderr.
";

const NEW: &str = "Usage: git twig new [<git-worktree options>] <path> [<commit>]\n       git twig new -b <branch>  # destination from a previously adopted root

  -b <branch>       Create a new branch
  -B <branch>       Create or reset a branch (delegates to Git)
  -d, --detach      Create a detached worktree
  -f, --force       Override Git's safety checks (delegates to Git)
  --no-checkout     Register without checking out files (delegates to Git)
  --lock           Lock the new worktree (delegates to Git)
  --reason <text>  Explain the lock

Native CoW: use -b <branch> or --detach with the requested revision.
Nearby worktrees can supply matching files. Unsupported options use stock Git.
Never overwrite an existing destination.

From the directory containing main/:
  git -C main twig new -b feat/x ../feat-x HEAD

The selected donor and destination must share a clone-capable filesystem. Git metadata must
also be on that filesystem; otherwise creation delegates to Git.
See git help worktree for the full Git worktree option reference.
";

const ADOPT: &str = "Usage: git twig adopt [options] [<worktree>]

  --worktrees-dir <root>  Preview root/{main,feat-x,...}; remember after adoption
  --dry-run              Preview layout/sharing without writes
  --yes                  Apply the reviewed plan; explicit/saved root required
  --compact              Request matching-file compaction as well as relocation
  --offline              Affirm donor/target worktrees are saved and closed
  --move-only            Relocate without attempting file sharing

No target: include the original clone and this repository's registered worktrees.
A target selects only that checkout. The original clone uses main/ regardless of branch.
An unsaved root is prompted; no global layout modes. No overwrites or forced unlocks.
Coordinate agents/editors first: stored absolute session paths are not updated.

macOS: interactive adoption offers compaction and requires explicit offline consent.
Automation requires --compact --offline --yes. --yes alone is move-only on macOS.
Stop agents/editors/builds/servers first; paused processes can retain writable mappings.
Verified native CoW replacements preserve current bytes, supported metadata and index
entries, but change inode/ctime. Old versions are retired only after verification.
An open original prevents retirement; failure retains copies and a recovery journal.
Never treat process scans or --offline as proof that a live worktree is safe.

Linux: native in-place sharing remains the default; --move-only disables it.
Matching logical/kernel bytes do not measure exclusive physical space reclaimed.
Explicit --prepare/--dedupe <linked-worktree> keep v0.5 sharing-only semantics:
--prepare clones are never installed; Linux --dedupe does not replace inodes.
";

fn is_help(arg: &OsString) -> bool {
    arg == "-h" || arg == "--help"
}

pub fn for_args(args: &[OsString]) -> Option<String> {
    if args.is_empty() || args.first().is_some_and(is_help) {
        return Some(ROOT.into());
    }
    let command = args.first()?.to_str()?;
    if !args[1..].iter().take_while(|arg| *arg != "--").any(is_help) {
        return None;
    }
    let help = match command {
        "new" => return Some(NEW.into()),
        "adopt" => return Some(ADOPT.into()),
        "list" => {
            "list [--verbose] [--porcelain [-z]]\nList worktrees; --porcelain is Git's stable machine-readable format."
        }
        "remove" => {
            "remove <path>\nRemove a worktree and its registration. Check for changes and ask first."
        }
        "move" => "move <path> <new-path>\nMove a worktree and update its registration.",
        "lock" => {
            "lock [--reason <text>] <path>\nProtect registration from pruning, moving or removal; does not lock files."
        }
        "unlock" => "unlock <path>\nRemove registration protection.",
        "prune" => {
            "prune [--dry-run] [--verbose] [--expire <time>]\nRemove stale registrations, not working directories. Preview with --dry-run."
        }
        "repair" => {
            "repair [<path>...]\nRepair repository/worktree links after a manual move, not file contents or conflicts."
        }
        "skill" => "skill\nPrint the bundled agent instructions, even outside a repository.",
        _ => return None,
    };
    Some(format!("Usage: git twig {help}\n"))
}
