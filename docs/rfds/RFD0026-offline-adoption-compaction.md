# RFD0026 — Offline APFS compaction during layout adoption

- Status: implementation contract (unreleased)
- Author: leostera
- Date: 2026-11-18

## Decision and motivation

Extend [layout adoption](RFD0025-adopt-worktree-layout.md) so an owner can move and
compact existing working files in one reviewed migration. The owner explicitly
accepts an offline macOS maintenance step: save work and close agents, editors,
builds and servers accessing the selected trees before file replacement. Moving
alone does not establish this precondition. This is not safe live APFS deduplication
and does not relax [RFD0023](RFD0023-adopt-existing-worktrees.md)'s live-file rule.

## Guide-level contract

`git twig adopt --compact --dry-run --worktrees-dir=/path/to/repo` previews both
the layout and byte-identical, same-path tracked files eligible for sharing.
Matching logical bytes are not a promise of exclusive physical space reclaimed.

Interactive macOS adoption offers optional compaction. The owner must explicitly
acknowledge the offline requirement, then confirm the complete migration. Move-only
remains the default when compaction is declined. `--move-only` skips sharing on
both platforms. Automation requires `--compact --offline --yes` plus an explicit
or saved root; `--yes` alone never authorizes APFS replacement. `--offline` is an
explicit owner assertion, not proof that open descriptors or mappings do not exist.
A no-write preview never needs offline consent. There are still no layout enums.

Linux retains kernel in-place deduplication by default (or with `--compact`), and
never takes the APFS replacement path. Explicit v0.5 `--prepare`/`--dedupe` remain
sharing-only. `--prepare` candidates are never installed by those older modes.

## Reference-level contract

Use the original main clone as a same-repository donor. Only same-path nonempty
regular files tracked in both donor and target are eligible; compare every current
raw byte and EOF. Do not restore HEAD, normalize through filters, copy ignored
files, replace symlinks, break hardlinks or refresh/rebuild indexes. Staged,
unstaged and conflicted index entries survive unchanged. Replaced inodes and ctime
necessarily differ; Git may refresh its stat cache on the next status.

For APFS, after explicit consent create a private mode-0700 recovery directory
under the common Git directory, `twigs/compact/<id>` (amended; first beside the
original clone), verifying owner/mode and refusing inherited ACLs before
writing candidate bytes. Probe native clone/swap and busy-unlink capabilities
using only fresh owned probe files before moving anything. Unsupported primitives
refuse compaction, not silently copy full data or unlink busy originals. Keep a
durable NUL journal identifying target, temporary path, original/candidate inode
identities and each prepare/swap/retire boundary. Recovery data is never a global
workspace registry or an automatic cleanup sweep. Git metadata on another
filesystem refuses compaction before anything moves. Every operation uses open
directory handles, so the directory keeps working when adoption moves main and
its Git directory. Once every target is compacted, or the capability probe fails
before any move, remove it: originals are retired by then, and refused candidates
were never installed.

After relocation and Git-link repair, open each current donor/target through
no-symlink directory-descriptor walks. Verify same device, distinct single-linked
regular inodes and current byte equality. Capture supported target metadata,
including mode, owner/group, mtime, creation time, BSD flags, xattrs and ACLs.
Refuse unsupported/special metadata rather than lose it. Bound metadata reads.

Create a fresh native CoW candidate from the donor descriptor. Apply the target's
metadata, not the donor's, using descriptor-based metadata copying without data.
Apple's copyfile may retain an earlier donor creation time; explicitly restore
target creation time through ATTR_CMN_CRTIME on the private candidate. Access
times can change from reading. Verify candidate data and metadata against the target again and refuse changed
file identities/state. Sync candidate and journal before fd-anchored atomic
RENAME_SWAP. The old target remains in the private recovery directory; its pathname
is never briefly absent. Verify installed/displaced identities, bytes and metadata.

Only after successful verification, close Twig's own old-file descriptor and
retire the displaced original with `unlinkat(AT_NODELETEBUSY)`. Verify that the
private entry is still the expected original inode before retiring it. An open
old descriptor must prevent retirement. This guard is not a proof against all
writable mappings or new writers; the offline contract remains mandatory.

A detected inconsistency after a swap stops compaction and retains both files.
Do not swap back blindly over possible new writes. Earlier successfully compacted
files remain compacted; layout moves remain completed. Report failure and the
journal location explicitly, not a successful all-or-nothing transaction. A crash
at an intent/completion boundary leaves paths and identities for manual recovery;
no automatic unsafe resume is shipped.

Successful compaction deliberately retires verified old versions: keeping full
originals or their CoW backups forever keeps old extents referenced and prevents
reclamation. This irreversible boundary is covered by the explicit compaction
confirmation. APFS snapshots or unrelated references may still retain old blocks;
never delete snapshots or infer actual free-space savings from logical sizes.

## Safety and limits

No process killing, privilege escalation, lsof-based claim of exclusive ownership,
agent lifecycle hooks, hardlinks, Git resets, filter execution, index rewriting,
external telemetry or live replacement. A paused process may still hold writable
descriptors/mappings: owners must close them. Skip immutable/append-only, dataless
or compressed files and metadata too large or not faithfully reproducible in the
initial slice. Git registration/environment protections and same-filesystem
relocation constraints remain in force. Directory discovery assumes a trusted
local repository; descriptor-anchored file application prevents following target
symlinks while operating, not every adversarial namespace mutation.

## Validation

Retain disposable fixture repositories under ignored target directories. Cover
clone certification/private writes, target-specific modes/timestamps/xattrs/ACLs,
dirty/staged bytes, odd paths (non-UTF8 where the filesystem permits them),
hardlinks/symlinks, metadata refusal,
no-consent refusal before moves, write-free previews and repeated adoption.
Hold an original file descriptor open: retirement must fail, data and recovery
copies must remain, and failure must not claim compaction success. Inject a failure
after swap to demonstrate retained originals and useful recovery records. Check
index bytes and registration/HEAD before and after; verify no hooks or filters run.
Linux remains covered by its existing native in-place tests; do not claim a Mac
probe establishes Linux behavior or publish performance/physical savings figures
from correctness tests.

## Primitive references and alternatives

Apple documents [clonefile](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/man/man2/clonefile.2)
creating new files, [RENAME_SWAP](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/man/man2/rename.2)
exchanging names, and [busy unlink](https://github.com/apple-oss-distributions/xnu/blob/f6217f891ac0bb64f3d375211650a4c1ff8ca1ea/bsd/man/man2/unlink.2)
refusing open files. [copyfile metadata](https://github.com/apple-oss-distributions/copyfile/blob/9f91eb6ced021952278816cdc76ad68da8631ccb/copyfile.3)
includes ACLs, xattrs and POSIX fields but has flag/privilege exceptions that require
verification. `exchangedata` explicitly does not support APFS. None is a kernel
compare-and-share operation equivalent to Linux FIDEDUPERANGE.

Doing nothing leaves Mac adoption as relocation only. Always replacing live files
would violate existing write guarantees. Rebuilding from HEAD loses current work.
Keeping indefinite full backups avoids retirement but fails the reclamation goal.
A background dedup daemon or process-manager integration expands ownership far
beyond the requested migration. The offline user-authorized boundary is the small,
honest first implementation, with unsupported cases left untouched.
