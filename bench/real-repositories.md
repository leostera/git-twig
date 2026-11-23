# Real repositories: CLI creation and filesystem allocation

## Frozen release refresh

`bench/real.ts` orchestrates a new-only campaign with the existing Rust worker.
It supports `linux-xfs`, `linux-btrfs`, `macos-apfs-case-sensitive` and
`macos-host`. Inputs are a local JSON catalog with `schemaVersion: 1` and a
`repositories` array. Each repository declares `label`, absolute `path`, full
`revision`, `worktrees`, `imageGiB`, and `hostTiming` (false for the kernel).

```sh
bun bench/real.ts --target linux-xfs \
  --candidate /frozen/published/git-twig --candidate-commit RELEASE_COMMIT \
  --binary-dir /frozen/bin --inputs /local/inputs.json \
  --environment 'lab and backing storage description'
```

The v0.5 refresh compares the published release binary with stock Git, using
three paired rounds, concurrency one and warmup enabled. Workers starter and
git-twig create ten worktrees per cohort; Axo, kernel, OpenClaw and Pulumi create
one. Repository pins remain those of the v0.4 published comparisons. Inputs and
release assets are prepared before measurement; no application/dependency code
is run. Source objects are read-only inputs, not copied ignored build trees.

The default limits are two hours per campaign, thirty minutes per repository
job, 128 GiB minimum free host space and 160 GiB maximum observed host growth.
They are configurable and recorded in each manifest. Run only one campaign per
backing disk; no competing builds or tests. Host timing and image allocation are
separate campaigns, never interchangeable observations. Kernel timing requires a
case-sensitive filesystem image, not the normal case-insensitive host volume.

Results use exclusive RFD0020 build/target/date/run directories. Pending jobs and
failures persist, binaries and harness must remain frozen, and failed measurements
are never retried. Summary JSON retains unstable allocation, negative deltas,
operation percentiles, first-status latency and harness CPU headroom. Unstable
allocation is not certified comparative space evidence. Small samples do not
establish population tail latency. These commands never update the website.

Verify the retained release capture and regenerate its reports in memory:

```sh
bun bench/real-report.ts --check bench/results/v0.5.0-real.json
```

The capture records original result hashes, release identities, exact input pins,
sequential intervals, incomplete jobs and qualification/interruption evidence.
The reporter checks complete paired groups and reproduces JSON/CSV byte-for-byte.
Partial results do not become complete comparisons. Raw manifests are never
rewritten after an abrupt interruption or a corrected post-processing gate.

### Long-window allocation protocol

New allocation campaigns use filesystem sync followed by half-second allocation
samples: at least 20 observations (10 seconds), with the final 11 identical
observations spanning five seconds. The limit is 240 samples (120 seconds) per
snapshot. All four snapshots in every paired cohort must settle. The raw header
records this policy; the parser independently checks the observation window.

An unstable attempt remains immutable and is not eligible for publication. With
explicit operator authorization, retry the **entire three-round paired repository
job** on fresh disposable images, not selected backends or individual cohorts.
Select the first fully qualifying attempt, never the fastest or smallest result.
Keep previous attempts and distinguish their shorter sampling protocol. Stop on
correctness failures rather than retrying them into apparent success.

## Standalone worker

This suite measures a cohort of real CLI processes, not an internal copier.
Run on macOS from this repository root, with permission to mount and delete
disposable disk images. Original repository paths are read-only inputs.

```sh
cargo build --release --workspace
target/release/twig-real-bench \
  --repo small=/absolute/small/repo \
  --repo large=/absolute/large/repo \
  --concurrency 100 --repeats 3 \
  --output target/real-results/100-way.json
target/release/twig-real-plot target/real-results/100-way.json
```

Each image-backed backend/repeat gets a fresh sparse APFS image (8 GiB by default;
configure capacity with `--image-gib`). Use `--filesystem case-sensitive-image`
for trees with case-colliding paths, such as the Linux kernel. Never benchmark such
a tree on case-insensitive APFS and silently accept an incomplete checkout. A seed clone uses
shared Git objects through alternates into the original repository, then checks
out the pinned commit inside the image. The seed and all created worktrees share
the image's APFS filesystem; history remains external and identical for both.
No source hooks/global Git config are inherited. Ignored/untracked dependencies
and build artifacts are not copied.

A barrier starts all requested CLI workers together. Atomic `.creation.json`
checkpoints outside each image retain completed CLI outcomes with verification
explicitly pending; only final cohort records prove completed verification.
The person may request a completion-oriented run without a time cutoff; disk
headroom and the approved scratch-space ceiling still apply. The harness records cohort
wall time, per-attempt create latency, exit/error details and native-backend
selection. It does not retry failed registrations. First status and full
byte/symlink/executable verification happen after all create workers join.
Per-operation first-status latency is recorded separately; the aggregate
status-and-verification time includes verification and is not pure status time.

## Fixed-count sequential runs

Use `--worktrees 10 --concurrency 1 --repeats 1` for ten creates per backend,
one at a time. Without `--worktrees`, the total defaults to concurrency as before.
Creation timestamps in each operation prove whether intervals overlap. A returned
CLI is checkpointed immediately; independent verification follows after all creates.

## Direct host timing

Use `--filesystem host` to measure normal host-APFS creation without the disk-image
layer. It retains all owned fixture directories (including warmups), never deletes
them, never forces sync, and reports space as unmeasured. Use `--revision label=OID`
after `--repo` to match the exact pinned commits from an earlier image run.
Compare both backends under the same mode; do not present host timings and image
space as if they came from one cohort. Schema v2 records the mode; the report also
supports v1 image results.

## Space method

Sum neither logical sizes nor st_blocks. After sync, sample statvfs allocation on
the isolated image until stable (or mark unstable). Subtract pre-cohort allocated
space from allocation after creates and after status. This measures the entire
filesystem delta, including working files, indexes, staging and APFS metadata.
No unrelated user's apps should write to this image; Spotlight indexing is
disabled using a root .metadata_never_index marker.

Then change one byte in one regular file per successful worktree, measure further
allocation and verify the seed file remains unchanged. This demonstrates write
isolation and records a small-edit allocation budget without claiming every file's
exclusive block ownership is observable. Negative deltas are retained, not clamped.
The host allocation of the image is not the primary metric. macOS images are
sparse bundles: on the shared Mac Studio, single-file sparse and read-write
images took 2.5-3.9 ms per small-file write against 0.08 ms for a sparse bundle
and the host volume, which inflated OpenClaw creation from 17.5 s (v0.5) to
190-250 s with stock Git. Allocation inside the image does not depend on it.

Warmup worktrees and their owned staging paths are removed before the initial
space snapshot. After saving each cohort, detach and delete its image. On failure,
inspect logs and hdiutil info for any remaining mounted image; never force-detach
an unrelated volume. Empty fixture directories may remain under target/real-images.

## Interpretation

A fast cohort with failed operations did not create the requested number of
worktrees successfully. Report success counts, errors and content verification
alongside speed and allocation. Small committed trees with huge ignored output
are not large checkout workloads. Source commit/tree IDs, file counts and logical
bytes distinguish those cases.

Backend order alternates by repeat. Caches remain warm; filesystem image layering
is controlled but differs from the normal host APFS volume. Capture raw results
before making claims. Empty fixtures use an empty initial commit, not an unborn HEAD. With no regular
file, the private-write probe is not applicable and its allocation remains
unmeasured; `privateWriteTested` records that distinction.

Private-repository results stay in target/ by default;
only publish with separate permission. Keep versioned private copies when needed.
