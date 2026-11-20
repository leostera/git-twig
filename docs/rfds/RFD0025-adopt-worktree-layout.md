# RFD0025 — Adopt existing worktrees into a confirmed layout

- Status: implementation contract (unreleased)
- Author: leostera
- Date: 2026-11-18

## Decision and motivation

Make `git twig adopt` discover and relocate existing worktrees, rather than mean
only duplicate-byte inspection. Agents frequently create nested worktrees inside
the original checkout. The owner wants a single repository container containing
`main/` and sibling task checkouts, without losing staged, dirty, untracked or
ignored work. This supersedes the default command meaning in
[RFD0023](RFD0023-adopt-existing-worktrees.md); its file-sharing safety contract
continues to apply. No layout enums, global path slug scheme or client hooks.

## Guide-level contract

`git twig adopt` lists this repository's Git-registered worktrees and proposes
old-to-new paths. It prompts for `--worktrees-dir` when there is no saved root,
recommending a local container: `repo/{main,feat-x,...}`. The original clone always
occupies `main/`, regardless of its current branch. Branch slashes become hyphens;
unsafe filename bytes are percent encoded. Detached checkouts retain their old
basename. Ambiguous/case-folded slot collisions are refused, not overwritten.

`git twig adopt --worktrees-dir=/path/to/repo` selects that container explicitly.
`git twig adopt <linked-worktree>` selects only that registered checkout, leaving
the main clone where it is. Its first-use root recommendation must be outside the
main clone. `--dry-run` previews without creating directories, writing config or
moving anything. For compatibility, an explicit target dry-run also reports the
existing read-only byte comparison when invoked from a distinct donor.

Show the entire plan and warn that sessions, editors and absolute-path setup may
need restarting/updating. Interactive confirmation defaults to no. Noninteractive
application requires `--yes` and an explicit or previously saved root. A canceled
or unavailable confirmation changes nothing. Save `twig.worktreesDir` in common
local Git configuration only after successful relocation. A later
`git twig new -b feat/x` with no destination uses that root; explicitly provided
creation destinations retain their existing semantics.

## Reference-level contract

Use the lossless NUL-delimited registration inventory in `src/registry.rs`, not a
filesystem crawl or branch-name ownership inference. Read from any linked
checkout, but identify the original clone from Git's main registration. Refuse
bare repositories and main clones with external/symlinked Git metadata. Check
all registrations, locks, pruning notices, unknown fields, destination slots and
filesystem identities before confirmation, then recheck the inventory under the
existing Twig registrar lock before any mutation. Git and third-party agents do
not honor this lock; users must coordinate active sessions. This is trusted-local
repository tooling, not an adversarial filesystem sandbox.

Never move an unselected registered descendant implicitly. Refuse submodule
checkouts for relocation until their independent Git metadata has a proven
repair contract. Do not refresh indexes, reset files, change branches, run hooks,
force unlock, copy secrets or replace existing destinations. Refuse cross-device
relocation before writes; copying-and-deleting is not a safe fallback.

For in-place restructuring (`repo` becomes `repo/main`), first park selected
checkouts in a fresh private recovery directory, `.twig/adopt-<id>` beside the
main clone, deepest-first. It cannot live in Git metadata, because the main clone
and its Git directory are parked in it. `.twig/` must be a directory, not a link,
owned by the user and writable by no one else. Park the
main clone last. Create the container only after its old contents are parked,
then install the main clone and linked checkouts using atomic, no-replace directory
renames. Git `worktree repair` owns updating Git links after relocation, including
unselected peers when the common Git directory moved. Persist a lossless,
NUL-delimited intent/completion journal, flushing before destructive steps.

On an ordinary error, reverse completed renames and empty owned-directory
creation, then repair original registrations. Never overwrite a newly occupied
path during rollback. If interruption or rollback prevents completion, retain
all files and the recovery journal and report its location. No automatic retry or
sweep; SIGKILL cannot promise rollback. After success, or an ordinary error whose
rollback restored the original layout, nothing needs recovering: remove the
journal, its directory and `.twig/` if empty (amended; journals were first kept
even on success, which left a folder per run beside the checkouts). Anything
unexpected inside keeps the directory.

## Sharing and compatibility

Layout adoption does not turn a worktree into a different Git object: Twigs remain
ordinary Git worktrees. After a confirmed relocation, attempt existing Linux
in-place deduplication against the main clone, reporting unsupported sharing
separately from successful relocation. Preserve inode identity and current bytes,
using the kernel's comparison, not a stale inspection. Deduplication failure is
not permission to replace files or undo successful relocation.

On macOS, report that placement changed but existing sharing is unchanged.
`--prepare` and `--dedupe` with an explicit target retain their shipped v0.5
file-sharing behavior as separate explicit modes; they cannot be combined with
layout arguments. Prepared APFS candidates are never installed. No claim of
exclusive physical space savings, automatic APFS compaction, agent integration,
Windows support or published performance improvement.

## Validation

Use retained fixture repositories, not the owner's live worktrees. Exercise
nested and sibling worktrees, invocation from a linked checkout, main relocation
into its own former root, single-target moves, branch/detached naming, dirty and
staged bytes, untracked/ignored files, original index bytes and inode identity.
Check no-write previews and refusals for missing confirmation, occupied slots,
locked/prunable/unknown registrations, nested unselected worktrees, initialized
submodules, foreign targets, unsupported flags, symlinks and device boundaries.
Inject failure after parking/installation to verify rollback and retained recovery
journals. Recheck Git registrations, status, branch, HEAD and private writes after
moves; keep existing sharing and checkout regression coverage.

## Alternatives and limits

Keeping only `git worktree move` is simplest but cannot move the main clone or
restructure a clone into its own container, nor repair every peer after the common
metadata moves. Doing nothing leaves agents' nested placement unchanged. Global
layout modes and remote-derived slugs add policy the owner explicitly removed.
Rebuilding worktrees from HEAD loses uncommitted and ignored state. Atomic
clone-and-replace on APFS changes open-file identity and can lose active writes;
relocation must not be presented as safe compaction.

Runtime journals are recovery data, not an adoption registry: Git remains the
source of truth for registration and branches, and one local Git config value
stores only the selected root. External session paths remain an owner-coordinated
limitation, not something a CLI can infer or silently rewrite.
