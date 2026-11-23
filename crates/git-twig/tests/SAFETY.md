# Safety properties

Run `cargo test -p git-twig --test worktrees`. Fixtures are retained under
`target/test-fixtures`, or `TWIG_TEST_FIXTURE_ROOT`. Failures print the fixture
path. `TWIG_TEST_REQUIRE_NATIVE=1` makes Linux native coverage mandatory; an
independent reflink probe prevents silently testing fallback instead.

## New differential and lifecycle cases

`cases/safety_properties.rs` exercises these contracts through the actual CLI:

| Property | Oracle / injection |
| --- | --- |
| Target revision, not donor index, determines output | 12 deterministic SHA-1/SHA-256 cases; dirty/staged/missing files and changed modes; compare with independently created stock worktree |
| Raw output equals stock checkout | Exact recursive inventory, raw file bytes, executable bits, symlink targets; LF/CRLF attributes, binary data, Unicode/tab/newline paths |
| Ignored files cannot escape into the target | Root/nested/deep dependency directories, each containing small files and a symlink outside the clone |
| Source and source index remain untouched | Snapshot before creation, compare afterward, then mutate every private regular file and compare source again |
| No hardlinks to donor | Compare device/inode pairs for every surviving source regular file |
| Existing destinations stay untouched | Nonempty directory, file, symlink, dangling symlink; compare user inventory, outside sentinel, and Git registration |
| One owner per destination | Four CLI calls launched from a barrier, with shared or distinct destinations; exact success/registration counts and contents |
| Failure does not erase recovery evidence | Git wrapper performs real registration then fails or SIGKILLs Twig; also fails final read-tree after files are installed; inspect retained partial destination and staging |
| Verified private snapshots survive donor edits | Mutate donor immediately after registration; target remains equal to original stock contents, donor edit survives |
| Destination interference stops installation | Insert foreign file or source-pointing symlink after registration; neither it nor its target is overwritten |
| HEAD movement invalidates installation | Move detached destination HEAD after registration; reject installation before any target files are moved |
| Retrying cannot consume another invocation's scratch | Reject reuse of partial destination; succeed at fresh destination; compare retained staging before/after and exact registrations |
| Windows clones never replace or follow | Block-clone a 70,001-byte file (ends inside a cluster) and an empty file; refuse an existing destination and a directory source with nothing created; private writes never reach the source |
| Windows long paths match Git | Without `core.longpaths`, a 230-character destination path clones natively although staging is longer; 252 characters delegates with the reason; with `core.longpaths=true`, 300 characters clones natively |
| Windows paths and volumes match Git | Canonical fixture paths have no verbatim prefix; volume identity is stable; FILETIME conversion matches Git for Windows' index |
| Disabled symlinks keep the rest shared | `core.symlinks=false` root and nested links are Git-written plain files beside a shared file, on every platform |
| Converted clones equal Git's checkout | autocrlf, `eol=crlf`, legacy `crlf`, `ident`, `working-tree-encoding`: bytes and status match stock Git; a stale donor and an `export-subst` path are Git-written |
| Windows moves undo on open files | A file held open in main blocks its move after a child moved; every move is undone, files and index unchanged, no journal kept for a restored layout |
| Windows dedupe is locked, in place | Shares a 300,001-byte file keeping identity, attributes, last write time and index; skips files open for writing (target or donor), mapped writable with no open handle, reached through a junction, or hardlinked; writers still write their own file |

`cases/partial_clones.rs` counts promisor fetches in Git's own event trace: none
for the current revision, at most one batch when target blobs are absent.
`cases/donor_metadata.rs` and `cases/staging_cleanup.rs` (macOS) compare extended
attributes and BSD flags with a stock worktree, and require immutable tracked or
ignored donor files to leave neither flags in the target nor retained staging.

The same file covers scratch removal for paths beyond PATH_MAX under the staging
prefix, branch names Git refuses before registration, and (ignored, attaches a
disk image) exFAT companion files. `cases/donor_coverage.rs` covers a separate
Git directory, where Git lists the main checkout at a path holding no checkout.

`cases/staging_cleanup.rs` also requires creation to inherit metadata through a
private directory in `.twig/`, leaving no Twig-named entry beside checkouts, and
on macOS requires Twig's directories and files to carry the same inherited ACL
entries as a checkout Git creates beside it.

`cases/index_freshness.rs` requires the installed index to record inode and size
for root, nested, symlink and Git-rewritten entries, and same-size dirty donor
bytes to be rewritten rather than recorded as clean.

`cases/removal.rs` removes a worktree whose absolute paths exceed PATH_MAX, which
Git unregisters but cannot delete, and requires no directory or registration to
remain. With the same paths, Git's refusals for untracked files and locks
(including `--force` on a locked worktree) must leave the checkout and
registration intact. Twig deletes only the one checkout Git unregistered.

These are finite generated cases, not exhaustive proofs or a randomized shrinking
framework. The generation seed and fixture are retained. Parallel launch varies
scheduling but does not force every interleaving. The registration fault injection
is deterministic and has a process-group watchdog; no sleeps select the boundary.
Existing `checkout_races.rs` covers attribute changes before/after registration,
dirty filter policy, and writable mappings on Unix. The mapped-donor case must
also run on required-native Linux XFS/Btrfs, not only on APFS. Existing worktree/checkout-policy
tests remain part of the same suite.

## Interpretation

A clean `git status` is not the contents oracle. Donor paths that were deleted or
replaced by another kind of entry are written by Git while the rest stays shared;
both those and intact donor cases must use native CoW on macOS and on
required-native Linux runs. Symlink traversal is never used by the inventory helper. `.git`
metadata is excluded from raw checkout comparisons and registration is checked
separately. Index preservation is an explicit byte comparison.

The ignored-file fixture checks correctness, not a performance threshold. There
are no elapsed-time assertions. Dependency scripts are never executed. These tests
do not claim to fix or reproduce issue #5's two-minute slowdown.

## Existing-worktree adoption

`cases/adopt_layout.rs` and `cases/adopt_compact.rs` require that successful
adoption and compaction, and rollbacks that restored the layout, leave no `.twig`
entry beside the checkouts and no compaction state under `.git/twigs/compact`.
A `.twig` link or group-writable `.twig` folder is refused before anything moves,
and a journal kept by an earlier failure survives a later adoption.

`cases/adopt_linux.rs` compares current raw bytes, identities, modes, indexes and
registration for inspection, preparation and in-place deduplication. It exercises
non-clone filesystems instead of skipping inspection with the cloning tests.
Donor-only ignored/untracked files must be excluded even when the target tracks
the same path. Scratch preparation must reject filesystems that cannot enforce
private directory permissions before writing candidate contents.

`fixtures/probe-linux-adoption.py` compiles a separate C probe against installed
Linux UAPI headers. It distinguishes FICLONE from FIDEDUPERANGE and retains syscall,
per-range status and byte counts. `TWIG_TEST_DEDUPE_SUPPORTED=0|1` passes this
independent result into the CLI tests; never infer it from a filesystem name or a
successful reflink. Required-native jobs also execute the ignored live-descriptor,
writable-mapping and stale-equality cases.

`fixtures/prepare-adoption-overlay.py` creates registrations through a temporary
bind mount before enabling OverlayFS, so recorded paths match the future merged
view without changing an active lower layer. The lower-layer test independently
probes its read-only-donor/writable-target descriptor combination. Upper-layer
success does not imply lower-layer acceptance: a kernel refusal must preserve both
merged checkouts and the raw lower oracle. Private writes must remain private.

exFAT/VFAT exercise the regular-file portable subset and safe refusal, not POSIX
mode or symlink compatibility. Tests do not measure physical block reclamation,
and OpenZFS/NFS remain outside this local-filesystem adoption matrix.

## Still required for broader coverage

- Deterministic failure at each individual installation/repair step, not only
  registration and the existing policy boundaries.
- Concurrent donor mutation during cloning/hashing (post-verification mutation is
  covered), and destination races within individual rename/check transitions.
- Full mode, ACL and Unicode/case-collision semantics on both case-sensitive and
  default APFS; system-owned flags and attributes; Linux immutable attributes.
- Separate-volume/external-drive cases, ENOSPC and injected I/O failures using
  disposable volumes; never fill a user's main disk.
- Larger generated inventories with replayable randomness and shrinking, while
  retaining failures and avoiding unbounded fixture growth.
- Executing these additions on minimum Git/Rust, native Linux XFS/Btrfs, and
  multiple macOS releases and published archive architectures.

Passing on one local APFS machine does not establish the remaining matrix.
