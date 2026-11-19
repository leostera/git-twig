# RFD0013 — Staging lifecycle and repository-independent help

- Status: implementation snapshot
- Author: leostera
- Date: 2026-11-16

## Decision and motivation

Keep materialized staging under the common Git directory in `twigs/staging`, not
beside user worktrees. Remove only this invocation's staging after a successful
create, including a successful Git fallback, and after SIGHUP, SIGINT or SIGTERM
before registration. Retain other failed or interrupted operations for diagnosis.
Existing leftovers are never discovered or deleted automatically.

Provide short help before repository discovery. Git itself intercepts top-level
`git twig --help` for a man-page lookup: install a manual with Homebrew and binary
archives, and document `git twig -h` as the portable form, including Cargo installs.
Subcommand help, particularly `git twig add --help`, works outside a checkout.

## Staging contract

Git registration still occurs only after verification. Staging and destination
must be on the same filesystem before attempting a native install; otherwise
use Git before registration rather than fail halfway through a cross-device move.

A private, empty bootstrap directory briefly inherits the destination parent's
GID, setgid and default ACL, then moves into the operation's private Git-metadata
container before materialization. This preserves the Linux inheritance contract
in [RFD0012](RFD0012-linux-correctness-and-measurement.md), without attempting to
reconstruct ACLs or chown into groups unavailable to the invoking user. Mode
probes and the checkout data live under that inherited directory. The outer
container and every bootstrap are exclusively created, never reused.

Success cleanup is explicit, not an unconditional Drop action. Hook failure and
post-registration failure retain their worktree and staging. Cleanup failure
warns with the retained path but does not turn an otherwise successful creation
into a failure. No glob, stale-PID sweep, or deletion of a previously existing
path is part of cleanup. A crash during bootstrap relocation can retain a private
empty bootstrap beside the destination; diagnostics/cleanup remain scoped to the
current invocation when it is still running.

Native creation defers SIGHUP, SIGINT and SIGTERM until registration starts.
Bulk APFS directory clones ([RFD0010](RFD0010-bulk-directory-cloning.md)) briefly
hold ignored and untracked files until pruning, and nothing refers to staging
before registration. A deferred signal therefore stops work at the next check
(before verification, fallback or registration), removes this invocation's
staging, and terminates by that signal; Git children in the foreground process
group fail on the same interrupt. Neither registration nor stock-Git fallback
starts after it. The previous dispositions return just before registration, so
later signals keep the retain-for-inspection behavior. Ignored or already handled
signals are left alone. SIGKILL cannot be handled and retains staging like any
other crash.

## Help and documentation

Bundle `git-twig.1` and install it from both Homebrew source and stable archives.
Cargo does not install manual pages; its users use `git twig -h` or the binary's
`git-twig --help`. Short help explains supported commands and common add options;
Git's manual remains authoritative for the full delegated worktree option set.
Document `git -C main twig add -b feat/x ../feat-x HEAD` and make the same-checkout
HEAD requirement explicit. This does not create a new fast path for older commits.

## Validation

Test native success cleanup, successful fallback cleanup, removal on SIGINT and
SIGTERM before registration, private retained staging on native/hook failure,
linked-source common-directory placement, concurrent creators, restrictive umasks
and Linux setgid inheritance. Preserve existing cross-filesystem and
byte-verification tests. Drive help through the real `git` external-command
dispatcher outside a repository, not only the binary. Check manual packaging,
generated formula installation and package contents.

## Alternatives and limits

Leaving successful staging behind is simpler but creates unbounded user-visible
clutter. Moving it without accounting for filesystem and inherited metadata can
break otherwise valid checkouts. Directly creating under Git metadata changes
Linux GID/default-ACL inheritance. Copying ACLs and chowning introduces additional
platform-specific failure modes. A short private bootstrap is the smaller change.

This is not general crash recovery or a cleanup command for old artifacts. No
benchmark speedup or compatibility expansion is claimed by this lifecycle change.
