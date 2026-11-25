# RFD0010 — Bulk directory cloning

- Status: implementation snapshot (amended; clean subtrees only, issue #12)
- Author: leostera
- Date: 2026-11-15

## Decision and motivation

Replace per-tracked-file materialization with APFS directory clones of tracked
root subtrees, as requested after [release profiling](RFD0009-profile-release-overheads.md).
The Darwin API is still `clonefile`, accepting directories, as in
[Rift's exact-copy backend](https://github.com/anomalyco/rift/blob/7077f47ae1f3800bed2c689142d0b32ea568c5fa/crates/core/src/strategy/apfs.rs).
Correctness probes on this Mac confirmed directory cloning with no-follow flags,
symlink preservation, new inodes for source hardlink aliases, and private writes.
These are correctness probes, not performance claims.

## Contract

Build a typed directory trie from Git's parsed committed inventory. Bulk-clone each
tracked root directory that holds nothing Git does not track, clone root regular
files individually, and recreate root symlinks. Never clone the repository root: its .git and unrelated root trees are
not included. All clones reject source-path symlink traversal.

Reduce each private cloned subtree to its tracked inventory. Remove unexpected
entries without following symlinks, including ignored/untracked content inside a
tracked directory. Empty untracked directories disappear too. Reject type changes
or missing tracked entries. Normalize regular/executable file permissions and
directory permissions from the caller's umask rather than inheriting arbitrary
source restrictions. Read-only excluded directories are made owner-accessible only
inside the private snapshot for removal; source permissions remain unchanged.

Create staging atomically with owner-only directory permissions. A cloned tracked
subtree can temporarily include ignored files until pruning completes; failures
retain staging for inspection, so that privacy boundary matters. The final worktree
contains only committed tracked entries. Independently verify the staged bytes,
Git modes and symlinks before registration, as before. Unsupported inputs or failed
preparation delegate to stock Git without mutating its worktree registry.

Git still owns linked-worktree registration, refs, hooks and index construction.
Do not copy Rift's independent .git/repository snapshot semantics. Do not reuse the
staged index in this change: isolate directory-cloning effects first.

(amended; clean subtrees only, issue #12) A tracked root directory that holds an
untracked or ignored entry anywhere below it is not cloned whole. One pass reads
each tracked donor directory once, never descending into an untracked entry, and
marks a directory clean when every entry in it is tracked with the same type and
every tracked subdirectory is clean. Clean subtrees are cloned whole and pruned as
before; the directories on the way to an untracked entry are created, and their
own files and symlinks cloned one by one. The pass is only a hint: pruning,
normalization and verification still check every staged entry, so a donor that
changes between the pass and the clone costs time, never correctness. Ignored
trees, such as nested worktrees with `node_modules` under a tracked `.claude/`,
are therefore never copied into staging, which also keeps their contents (for
example `.env` files) out of retained staging after a failure. Measured on one
MacBook: a clone of this repository with a tracked `.claude/settings.json` and
78,870 ignored files under `.claude/worktrees/` went from 7.1 s to 1.0 s; a clean
Kibana clone (136,265 tracked files) went from 44.8 s to 45.8 s for the extra pass,
with the same 25 directory clones. Issue #12 reported more than 10 minutes to about
5 s on its monorepo.

## Validation and reporting

Retain existing compatibility tests. Add directory-clone selection, nested ignored
and untracked pruning, symlink escape rejection, hardlink-alias independence,
source isolation, private staging, read-only input and restrictive-umask tests.

Freeze the old and new optimized release executables. Compare real CLI creates
with the same pinned kernel tree, uninstrumented harness, case-sensitive APFS image
capacity, count and concurrency. Verify all outputs and source isolation after
creation. Keep old raw results, commit every new raw point, and generate summaries
from data. Profiling timings are not the baseline for an improvement claim.

## Alternatives and limitations

Cloning the entire root also clones .git and unrelated dependencies needlessly.
Per-file cloning is simpler to filter but repeats ancestor checks, directory
creation and clone syscalls. A maximal clean-subtree planner avoids cloning
nested ignored dependencies; it was deferred until measured need, which issue #12
supplied (amended; see Contract). Bulk-clone-plus-prune was expensive when a tracked
subtree held large ignored directory trees. It still requires per-entry permission normalization
and Git content verification. No speedup is assumed before the release comparison.

## Recorded comparison

The [frozen executable provenance](../../bench/results/sha256-a1431f8601d8/macos-apfs-case-sensitive-image/2026-11-15/kernel-directory10/kernel-directory10.provenance.json),
[fresh per-file baseline](../../bench/results/sha256-65efbfe49fac/macos-apfs-case-sensitive-image/2026-11-15/kernel-per-file10/kernel-per-file10.json),
[Git/directory pair](../../bench/results/sha256-a1431f8601d8/macos-apfs-case-sensitive-image/2026-11-15/kernel-directory10/kernel-directory10.json), and
[generated comparison](../../bench/results/multi-build/macos-apfs-case-sensitive-image/2026-11-15/kernel-directory-comparison/kernel-directory-comparison.svg)
record the uninstrumented experiment. Every checkout passed independent content,
mode and symlink verification; sources remained unchanged and owned images were
removed. The directory candidate was faster than the fresh per-file candidate in
this observation but still slower to create than stock Git, while retaining the
large allocation advantage. No index handling was changed.

This is a sequential single round on a shared host, not a causal or stable speedup
estimate. The same per-file executable has an earlier completed run with different
observed timing; retain both datasets rather than selecting a favorable baseline.
Future work on the independently observed index-refresh/first-status cost should
be isolated and remeasured rather than attributed to directory cloning.
