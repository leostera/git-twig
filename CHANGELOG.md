# Changelog

This log starts with one consolidated entry for the feature set available in
v0.3.0, including features introduced in earlier releases. Future entries will
describe changes from this baseline after they are implemented and verified.

## Unreleased

### Added

- Windows: native copy-on-write on ReFS and Dev Drive volumes through block
  cloning, with every shared byte verified as on macOS and Linux. NTFS volumes
  delegate to stock Git after one volume query. Builds on issue #4 (mateo-m)
  and PR #3 (InventingMaya). See RFD0027.
- Windows: `git twig adopt` layout moves, `--prepare` and in-place `--dedupe`
  on ReFS, holding both files locked against writers from comparison until
  sharing. Files open for writing or mapped are skipped.
- Windows: repositories with deep paths clone natively. Twig's staging is
  about 45 characters deeper than the worktree, so Git refused files there
  that it writes fine at the destination, and Twig fell back to stock Git.
  Git in staging now allows long paths. Without `core.longpaths`, a worktree
  with paths of 248 characters or more is left to Git.
- Windows: files smaller than one cluster are written rather than block
  cloned, because cloning cannot share a partial cluster and would move both
  the donor and the copy out of ReFS's resident storage into clusters of
  their own. In-place dedupe leaves such files alone.
- Windows CI on NTFS and a ReFS Dev Drive with native cloning required, and
  a Windows `.zip` in each release with its SHA-256.
- npm: `npm install -g git-twig` on macOS (arm64, x64), Linux (x64, arm64,
  glibc) and Windows (x64). One package carries every release binary; nothing
  is downloaded at install time, and on macOS and Linux the command becomes the
  native binary, so Node does not start for each run.
- Linux arm64 (`aarch64-unknown-linux-gnu`) release archives.
- Windows install with Scoop: `scoop bucket add git-twig
  https://github.com/leostera/git-twig`, then `scoop install git-twig/git-twig`.
  The release binary links the C runtime statically, so it runs without the
  Visual C++ Redistributable.
- Converted files are shared: under `core.autocrlf=true` (the Git for Windows
  default), `eol=crlf`, `ident` or `working-tree-encoding`, a donor file is kept
  when it equals the bytes `git archive` produces for it. See RFD0028.
- `core.symlinks=false` repositories no longer fall back: Git writes symlink
  entries as plain files while every other file stays shared.
- `twig-real-bench --filesystem windows-dev-drive` measures space on fresh
  ReFS Dev Drives, counting the distinct clusters files reference, because
  ReFS volume free space moves by hundreds of MiB on its own.
- Optional offline APFS compaction during adoption: `--compact` previews and
  applies verified native CoW replacements with supported target metadata and
  index contents preserved. Interactive use requires explicit saved/closed-tree
  confirmation; automation requires `--compact --offline --yes`. `--move-only`
  disables sharing. Old copies are retired only after verification with a native
  busy-file guard; failures retain both copies and a recovery journal. This is
  not safe live deduplication or a measured physical-space claim.
- `git twig adopt` discovers registered worktrees, previews a sibling layout and
  asks before relocating the original clone and linked checkouts. A single
  `--worktrees-dir` option selects a root; omitted roots are prompted and saved
  per repository after success. An optional target adopts only one checkout.
- `adopt --dry-run` previews without writes; noninteractive application requires
  `--yes` with an explicit or saved root. Atomic no-overwrite moves preserve
  dirty/staged/ignored work, repair Git links and retain a recovery journal.
  Cross-device, locked, colliding and submodule layouts are refused.
- After adoption, `git twig new -b <branch>` can use the saved root without an
  explicit destination. Linux attempts in-place sharing after relocation; move-only
  macOS adoption reports sharing unchanged. Explicit v0.5 `--prepare`/`--dedupe` modes remain
  sharing-only and never replace live files.

### Changed

- Shared files are read once, not twice. The stat data of every verified file
  is recorded in the installed index, so the first `git status` in a new
  worktree no longer rehashes every file. Verification itself is unchanged
  ([RFD0024](docs/rfds/RFD0024-verified-index-reuse.md)). Issue #9 (dsfaccini).
- Files under automatic text detection that `git archive` certifies also have
  their stat data recorded, so converted checkouts skip the rehash too.
- README benchmark times include each new worktree's first `git status`, and
  their multipliers use the same totals. Issue #8 (dsfaccini).
- On Linux, large inventories are cloned on up to eight threads.
- A donor with deleted tracked files, or tracked paths replaced by a symlink,
  directory or other entry, no longer disables sharing for the whole worktree.
  Git writes those paths and the remaining files are still shared.
- Clone normalization skips `chmod` for files that already have checkout
  permissions.

### Fixed

- `adopt` on a layout that is already adopted says nothing moved, rather than
  "Layout adopted".
- macOS: files in a new worktree inherit ACL entries from the destination's
  parent, as files Git writes do. Whole-directory clones kept each donor file's
  own ACL; staging that inherited an ACL now clones file by file.
- `git twig new` briefly creates its metadata bootstrap in `.twig/` beside the
  destination, instead of a `.git-twig-stage-*` folder, and removes `.twig/`
  once it is empty.
- Adoption and compaction no longer leave a `.twig-adopt-*` or `.twig-compact-*`
  folder beside the checkouts after every run. Compaction recovery lives in
  `.git/twigs/compact`. Adoption parks checkouts in one `.twig/` folder beside
  the main clone, because main and its Git directory move through it. Both are
  removed once nothing needs recovering, and kept only after a failure.
- `adopt` lists each checkout once, shows destinations relative to the root,
  and no longer previews compaction for macOS `--yes` runs, which only move.
- `git twig remove` deletes worktrees whose absolute paths exceed the system
  limit (1024 bytes on macOS). Git unregistered them, failed with "File name
  too long" and left the directory behind. Twig finishes deleting only the
  checkout Git has just unregistered, so Git's checks for changes and locks
  still apply.
- macOS offline compaction no longer reports its primitives unavailable when
  an indexer such as Spotlight briefly opens the capability probe's files.
- SIGINT, SIGTERM or SIGHUP before registration removes this invocation's
  staging, which bulk directory clones can briefly fill with ignored files, and
  starts no stock Git fallback. Later interrupts still retain staging; SIGKILL
  cannot be handled. Issue #10 (dsfaccini).
- A revision that moves while Twig verifies its clones no longer registers the
  moved commit: `--detach` registers the verified commit, and `-b` delegates to
  stock Git before registering if its start point moved. Issue #7 (dsfaccini).
- Conditional config includes (`includeIf`) no longer force stock Git when the
  included files set only identity, signing, remote, credential or alias
  settings, which never change checkout bytes. Includes that set anything else,
  include further files, or use relative paths still delegate. From issue #4
  (mateo-m).
- Windows: a repository on a volume mounted at a folder, rather than a drive
  letter, now clones natively. Twig asked which volume held the mount point
  itself and sent every creation to stock Git. Adoption no longer refuses a
  main checkout directly under such a folder.
- `core.eol=native` is CRLF in Git for Windows; `text` paths are no longer left
  with LF line endings there.
- Probe atomic no-overwrite directory renames before relocating any worktree.
  Drivers such as NTFS-3g that reject the primitive are refused without moving
  user data; there is no overwrite-prone ordinary-rename fallback.
- Blobless partial clones no longer fetch one blob per file. Tree listings stop
  asking for blob sizes, and a target that needs blobs written by Git is left
  to stock Git, which fetches absent blobs in one batch.
- On macOS, new worktrees no longer inherit the donor's extended attributes
  (including quarantine) or BSD flags such as hidden and immutable.
- A branch name Git would refuse (already existing or invalid) is refused before
  any checkout is staged, so a mistyped command no longer retains a full clone.
- Repositories with a separate Git directory, including submodules, share from
  the checkout Twig runs in rather than always delegating to Git.
- Staging is removed even when a tracked path is too long under the staging
  prefix, or (exFAT) when `._name` companion files vanish during removal.
- Immutable donor files, tracked or ignored, no longer force a fallback and
  leave undeletable staging that retained ignored bytes under Git metadata.

## [0.5.0] — 2026-11-17

Share unchanged files across revisions and more checkout policies, and inspect
existing worktrees with experimental adoption tools.

### Added

- Bounded same-filesystem donor selection across target revisions, while keeping
  private raw-byte verification and source indexes unchanged.
- Git-written builtin ident/encoding exceptions without losing other sharing.
- Verified NUL-bearing blobs stay shared under automatic CRLF policies; forced
  text conversion and dirty donor bytes still go through Git.
- Branch tracking setup no longer appears as a conversion-policy change.
- Gitlink directories no longer disable sharing for other files. Initialized
  donor submodules are never copied or recursively checked out.
- Conversion rules on symlinks/gitlinks do not disable unrelated sharing, and
  core.symlinks=false only delegates when the target has symlink entries.
- Agent skill guidance for choosing a checkout, creating task branches, and
  asking before cleanup, without requiring client-specific lifecycle hooks.

### Verification and benchmarks

- Differential revision/donor cases, dirty/staged source preservation, private
  writes, concurrent creators and deterministic interruption/race coverage.
- Linux filesystem coverage includes native XFS/Btrfs, tested OverlayFS layouts,
  NTFS drivers and portable-filesystem fallback subsets.
- Frozen checkout campaigns across XFS, Btrfs and default/case-sensitive APFS,
  with separate timing/allocation observations and retained failed/unstable points.
- Generated README tables for the frozen v0.5 candidate against stock Git and
  source-built v0.4; the website compares only stock Git with the latest candidate.
  Synthetic workload results remain separate from historical real-repository
  benchmarks; no blanket speedup or adoption-space-savings claim.

### Experimental

- `git twig adopt` inspects matching current tracked bytes in existing linked
  worktrees without restoring HEAD or changing the index/registration. Default
  mode and `--dry-run` are read-only; `--prepare` retains private verified clones.
- Linux `adopt --dedupe` shares identical ranges in place through FIDEDUPERANGE,
  preserving inode identity and open-writer/mapping behavior. Native XFS/Btrfs
  tests include stale-comparison rejection, private writes and dirty/staged data.
  APFS live replacement remains unsupported; preparation is not compaction, and
  reported logical/kernel bytes are not measured exclusive allocation savings.
- Adoption candidates must be tracked in both checkouts; ignored/untracked donor
  files are excluded even when the target tracks the same path.
- `adopt --prepare` rejects filesystems that cannot enforce private scratch
  permissions before writing candidate contents, retaining only an empty directory.
- Linux adoption CI independently probes cloning and deduplication, including
  safe refusal on unsupported filesystems and OverlayFS lower-layer restrictions.
  exFAT/VFAT coverage remains a regular-file portable subset, not POSIX parity.

External filters on regular files, sparse checkout, conditional config includes and
relative attribute-file configuration remain conservative whole-request fallbacks.

## [0.4.0] — 2026-11-17

### Fixed

- Reduce regular-file verification overhead by batching unfiltered Git blob hashes
  of private clones instead of inflating target blobs through a pipe (#5).
- Clarify that private staging is retained only if creation fails; successful
  creation still removes its owned staging directory.

- Distinguish absent attributes from literal filter names such as `unset` and
  `unspecified`; these filters can no longer bypass native eligibility checks.
- Verify private clone bytes against raw target blobs rather than trusting a
  normalized clean-filter/index comparison.
- Resolve attributes from the target index, not dirty, staged or untracked
  source attribute files. Recheck policy before registration and installation;
  retain partial state on a post-registration policy change.
- Delegate before staging when copied worktree configuration contains relative
  includes whose resolution would change in the new worktree; keep consistent
  ordinary worktree overrides and absolute includes eligible.

### Added

- Bounded parallel raw hashing for large file inventories, with length-based
  rejection of oversized dirty donors and opt-in verification phase timings.

- Native coverage for supported identity-checkout policies, including
  `text=auto eol=lf` and applicable `core.autocrlf=input` configurations.
- Hybrid creation: Git writes files requiring line-ending conversion or whose
  candidate bytes differ; other verified files retain copy-on-write sharing.
- Trace counts for verified file clones and Git-written paths.
- Specific fallback reasons for unsupported requests, configuration, hooks and
  destinations; attribute refusals name the escaped path, attribute and value.
- Differential and failure tests for line endings, literal attribute values,
  attribute precedence, dirty policies, mapped donors, private writes, SHA-256
  repositories, unusual paths and policy changes during creation.
- CI coverage for the minimum supported Git 2.40 on macOS and Linux.
External filters, ident expansion, working-tree encodings, conditional config
includes and relative attribute-file configuration remain conservative
whole-request fallbacks. Other-revision cloning, braiding and re-sharing are
not included.

## [0.3.0] — 2026-11-16

### Worktree creation

- `git twig new` creates ordinary Git linked worktrees with native copy-on-write
  file sharing on macOS/APFS and Linux Btrfs or reflink-enabled XFS.
- Native creation supports explicit new branches (`-b`) and detached worktrees
  (`--detach`) at the source checkout's current `HEAD`.
- Git owns worktree registration, branch validation and shared repository data.
  Native creation runs the post-checkout hook after installation.
- Private clones use Git-based verification against the requested commit;
  uncommitted changes and untracked or ignored files are not intentionally
  inherited. Writes do not modify source files; source hardlinks are never used.
- macOS clones tracked subtrees in bulk and prunes unrelated entries. Linux
  clones tracked regular files individually. Executable modes and symlinks are
  handled as Git checkout entries.
- Existing destinations are not overwritten. Owned staging lives under the
  common Git directory in `twigs/staging`; successful operations clean their
  staging, while failed or interrupted attempts retain it for inspection.

### Commands and agent support

- `new` is the creation command. `add` is rejected with guidance to use `new`.
- `list`, `remove`, `move`, `lock`, `unlock`, `prune` and `repair` delegate to Git.
  Machine-readable listing is available through `list --porcelain [-z]`.
- `git twig skill` prints bundled agent instructions without requiring a checkout.
- Short help, subcommand help and `--version` work outside a repository.
- `TWIG_TRACE=1` reports native backend selection or fallback.
- The website serves agent setup instructions at
  <https://twig.leostera.dev/llms.txt> and provides a copyable onboarding prompt.

### Installation

- One `git-twig` Cargo package contains the CLI and Rust library.
- Homebrew and GitHub release archives provide macOS arm64/x86_64 and Linux
  x86_64 binaries. Homebrew and archives include the `git-twig(1)` manual.
- Requires Git 2.40+; building from source requires Rust 1.95+.

### Compatibility limits

- Native CoW requires source, destination and common Git metadata on the same
  supported filesystem. Other revisions, unsupported options/filesystems and
  unsupported checkout semantics delegate to `git worktree`.
- Native coverage excludes conversion attributes such as `text=auto eol=lf`,
  non-false `core.autocrlf`, sparse checkouts, submodules and certain hooks.
  This is not full native coverage of Git's checkout behavior.
- Failed installation can leave a partial registered worktree; inspect retained
  state before retrying. There is no automatic repair or sweep of old staging.
- JSON listing, commit/integration queues and re-sharing existing worktrees are
  not available in this release. macOS binaries are not signed or notarized.

[0.5.0]: https://github.com/leostera/git-twig/releases/tag/v0.5.0
[0.4.0]: https://github.com/leostera/git-twig/releases/tag/v0.4.0
[0.3.0]: https://github.com/leostera/git-twig/releases/tag/v0.3.0
