# v0.5 candidate checkout measurements

## Question

Can the development candidate retain file sharing across revisions, builtin
conversions and submodule checkouts without changing Git's output? What does that
cost in creation time and additional filesystem allocation?

These are synthetic coverage workloads, not representative whole applications.
They do not measure `adopt`, exclusive bytes reclaimed, or the published v0.5.0 binary.

## Summary

<!-- twig-preview:start -->

| Workload · APFS | Stock Git | Twig v0.4 | v0.5 candidate |
|---|---|---|---|
| Cross-revision | 139ms / 128.09 MiB | 181ms / 128.09 MiB | 884ms / 16.08 MiB |
| Builtin conversions | 145ms / 128.11 MiB | 372ms / 128.12 MiB | 923ms / 88.00 KiB |
| Auto CRLF | 392ms / 128.09 MiB | 803ms / 128.09 MiB | 614ms / 88.00 KiB |
| Submodules | 138ms / 128.09 MiB | 267ms / 128.09 MiB | 900ms / 72.00 KiB |
| Unchanged bytes | 134ms / 128.07 MiB | 853ms / 68.00 KiB | 880ms / 68.00 KiB |
| 4,096 small files | 1.11s / 36.41 MiB | 2.07s / 3.31 MiB | 2.08s / 3.38 MiB |

Each cell is **creation time / additional disk allocation**. Medians of 3
two-worktree batches, one create at a time, on warm-cache filesystem images.
Synthetic workloads, not real-repository or universal speed claims. Allocation
includes Git metadata after first status; it is not exclusive file space reclaimed.

Across all levels: 992 certified creates; 2 failed points and 1 unstable observation retained.

Other filesystems: [APFS (case-sensitive)](results/sha256-c4989db26497/macos-apfs-case-sensitive/2026-11-17/checkout-v05/comparison.svg) · [XFS](results/sha256-a73d956e3301/linux-xfs/2026-11-17/checkout-v05/comparison.svg) · [Btrfs](results/sha256-a73d956e3301/linux-btrfs/2026-11-17/checkout-v05/comparison.svg).
Speed varies by workload. [Full results and limitations](#setup-and-measurements).

<!-- twig-preview:end -->

## Setup and measurements

The [source index](results/preview-sources.json) selects one frozen four-target
campaign at candidate source `dc7f720`, against stock Git and a source-built
v0.4.0 tag (`9f86536`). Both Twig versions use Rust 1.95 release builds.
Each snapshot records executable hashes, Git version, hardware, kernel, mount
layout, fixture revisions and commands. Source bundles accompany the archives.

APFS measurements use default and case-sensitive 8-GiB images on a shared Mac.
Linux measurements use 8-GiB XFS/Btrfs loopback images in a privileged Linux lab
container, backed by an ext4 host disk. They are not bare-device measurements.
Do not compare absolute times across these different machines.

The README table shows three paired rounds of two sequential creates (one in
flight) for six fixed workloads. The website compares only stock Git and the
latest candidate, omitting the cross-revision workload and older Twig lines. Time is the median **batch creation
wall time**, excluding first status. Space is the median filesystem allocation
delta **after first status**, including worktree indexes and metadata. Disk ranges
remain in the generated data and appear on value tooltips. Small or negative
allocator deltas are observations, not evidence of negative file size or exclusive
physical space reclaimed. The saved CSVs retain latency percentiles, throughput,
allocation ranges, clone counts and all concurrency levels.

## Correctness and limitations

Every certified cohort checks raw bytes, modes, links, source preservation and
private writes. The publisher rechecks operation outcomes and frozen identities.
Only complete, stable, three-backend groups are compared; a failed or unstable
point is never substituted with zero. Campaign-level failed/unstable counts remain
visible even when the selected sequential groups are valid.

Higher-concurrency stock-Git failures and incomplete groups remain in snapshots;
inspect their failure records before drawing scaling conclusions. The warm-cache,
shared-host samples are small and do not establish tail-latency guarantees,
saturation capacity, universal speedups or adoption savings.

Earlier Mac runs at candidate `b4f9c7b` used a different toolchain/baseline setup
and remain separately archived under
[their controller index](results/multi-build/macos-apfs/2026-11-17/v05-controller/index.json).
They are not pooled into these medians. Historical real-repository measurements
for v0.4 remain [separate](results/readme-details.md).

## Reproduce the presentation

```sh
python3 bench/publish_preview.py
python3 bench/publish_preview.py --check
```

This derives `results/preview.json` and the marked summary table on this page
from immutable snapshots; it does not rerun benchmarks or rewrite raw evidence.
The website consumes that same generated JSON. See [the runner](README.md) to
reproduce the measurements rather than only the presentation.
