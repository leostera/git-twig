# git twig 🌱

With twig, your agents can create cheap lightweight worktrees and use up to 97% less space on disk. Imagine all the worktrees you could now make!

## Install

```sh
brew tap leostera/git-twig https://github.com/leostera/git-twig
brew install leostera/git-twig/git-twig
```

Or install from crates.io with Rust 1.95+ and Git 2.40+:

```sh
cargo install git-twig --locked
```

On Windows, use `cargo install` or the `.zip` attached to each release.

For local development: `cargo install --path crates/git-twig --locked`.

## Use

With your source checkout in `main/`:

```sh
git -C main twig new -b feat/x ../feat-x HEAD
git -C main twig list
```

Version 0.5.0 can reuse matching files from nearby worktrees for other revisions
too. Git writes changed or new files, and converted files whose bytes differ.
Short help works outside a checkout: `git twig -h` or `git twig new --help`.

For coding agents: `git twig skill` prints the bundled skill. Give those
instructions to your agent so it chooses the right checkout, branch and location,
uses Twig for creation, and asks before cleanup. Twig does not manage agent sessions
or intercept an app's built-in worktree creation; no client-specific hook is needed
for agent-directed CLI use.

Native CoW is verified on macOS/APFS, Linux Btrfs/reflink-enabled XFS,
including the tested OverlayFS layouts below, and Windows ReFS, including Dev Drive.
Creates ordinary linked worktrees from committed files; local changes and ignored
files aren't copied. Unsupported filesystems (including typical ext4), options and
checkout semantics use `git worktree`, as does NTFS on Windows. Set `TWIG_TRACE=1`
to see backend selection.

The selected donor, destination and Git metadata must share a clone-capable filesystem for
native CoW. Twig verifies cloned files against the requested commit; writes are
private, never hardlinked to source. Staging lives under the common Git directory
in `twigs/staging` and is removed after success, or on SIGINT/SIGTERM/SIGHUP before
registration. Other failed or interrupted attempts retain staging and may leave a
partial registered worktree; inspect before retrying.

### Organize existing worktrees (development main)

Keep agent checkouts beside the original clone, rather than nested inside it:

```sh
git twig adopt --dry-run --worktrees-dir=/path/to/repo
git twig adopt --worktrees-dir=/path/to/repo
```

Twig previews `/path/to/repo/{main,feat-x,...}` and asks before moving anything.
Omit the directory flag to choose a root interactively; it is remembered per
repository. Pass a worktree path to adopt just that checkout. After adoption,
`git twig new -b feat/x` uses the saved root automatically.

Coordinate active agents/editors first: their stored paths are not updated.
Moves preserve branches and staged, dirty, untracked and ignored files, stay on
one filesystem and never overwrite destinations. A recovery journal is kept only
when a failure needs manual recovery.
Layout moves also require a successful atomic no-overwrite directory-rename
probe; unsupported drivers are refused before any worktree moves. Submodule
relocation is not yet supported. Windows cannot move a directory while a
program has a file open in it or uses it as its current directory: close
editors, terminals and builds in those worktrees first, or Twig undoes every move.
Linux, and Windows on ReFS, can subsequently deduplicate matching files in place;
on Windows, files another program has open for writing or mapped are skipped.
On macOS, interactive adoption also offers **offline
compaction**: save work and close agents, editors, builds and servers using these
worktrees first. It replaces matching files with verified CoW clones, preserves
supported target metadata and staged/dirty contents, then retires verified old
copies. Inodes change, so the next Git status may refresh its stat cache.

```sh
git twig adopt --compact --dry-run --worktrees-dir=/path/to/repo
# After reviewing the plan and saving/closing every donor/target worktree:
git twig adopt --compact --worktrees-dir=/path/to/repo
```

For already-approved automation, macOS requires `--compact --offline --yes`;
`--yes` alone remains move-only. `--move-only` explicitly disables sharing on
both platforms. Busy originals cannot be retired; failure retains copies and a
journal for recovery. Never restart agents blindly after a compaction failure.
Logical matching bytes are not guaranteed physical space reclaimed, and neither
snapshots nor unrelated backups are deleted. This is not live APFS deduplication.

This layout flow is unreleased; published v0.5.0 has the earlier sharing-only
`adopt`. Check `git twig adopt -h` for `--worktrees-dir` and `--compact` before
following these flows.

## Filesystem support

| Platform | Filesystem | CoW / fallback | Verification |
|---|---|---|---|
| macOS | APFS | Native `clonefile` | Tests, CI and allocation benchmarks |
| Linux | Btrfs | Native `FICLONE` | Tests, CI and allocation benchmarks |
| Linux | XFS with `reflink=1` | Native `FICLONE` | Tests, CI and allocation benchmarks |
| Linux | Typical ext4 | Stock Git fallback; no reflink savings | Full checkout test matrix |
| Linux | tmpfs | Stock Git fallback; no reflink savings | Checkout matrix and mixed-mount isolation |
| Linux | exFAT / VFAT | Stock Git fallback; filesystem limitations apply | Portable checkout subset, not full POSIX semantics |
| Linux | XFS with `reflink=0` | Stock Git fallback expected | Not separately verified |
| Linux | OverlayFS over ext4 | Stock Git fallback | Checkout matrix and lower-layer preservation tests |
| Linux | OverlayFS over reflink-enabled XFS / Btrfs | Native `FICLONE` in tested layouts | Checkout matrix, copy-up and private-write tests |
| Linux | NTFS (`ntfs3`, ACL-enabled) | Stock Git fallback; no reflink savings | Checkout matrix; UTF-8 name restrictions apply |
| Linux | NTFS (`ntfs-3g`, permissions enabled) | Stock Git fallback; no reflink savings | Checkout matrix; UTF-8 name restrictions apply |
| Linux | OpenZFS 2.2.2 (tested guest) | Synced-source `FICLONE`; dirty data may delegate | 80 checkout tests with Git 2.43; minimum-Git environment incomplete |
| Linux | NFSv4.2 over tested XFS/ZFS server | Clone probes accepted | Full checkout suites budget-stopped; not verified support |
| Linux | bcachefs | Exploratory; no support claim | Not verified |
| Linux | F2FS | Stock Git fallback expected: F2FS has no reflinks | Not separately verified |
| Windows 11 | ReFS / Dev Drive | Native block cloning (`FSCTL_DUPLICATE_EXTENTS_TO_FILE`); each file keeps its last partial cluster private | Full test suite, native required; allocation benchmark |
| Windows 11 | NTFS | Stock Git fallback; no savings | Full test suite |

Linux uses the actual clone operation, not a filesystem-name allowlist. Clone
refusals before registration fall back to stock Git; failures after registration
retain partial state for inspection. An accepted clone does not guarantee useful
disk savings, especially for tiny files. ReFS copies the final cluster of any
file that ends inside one, so on a Dev Drive (4 KiB clusters) each file costs at
least 4 KiB: a rust-lang/cargo worktree took 11.9 MiB with Twig against 18.8 MiB
with Git. Verification covers the tested setups,
not every kernel, mount option or checkout policy. OverlayFS coverage includes
fresh upper-layer repositories and repositories seeded in lowerdir before mount;
it is not an allocation benchmark. See [Linux test setup](bench/linux.md).
Checkout policy coverage below is separate from filesystem capability.

## Checkout policies

Version 0.4.0 supports common LF policies such as `text=auto eol=lf`.
They compare private clones to raw target blobs and let Git write exceptional
files, retaining sharing for the rest. Version 0.5.0 also chooses a donor
across revisions and lets Git write builtin ident/encoding exceptions. Unreleased
builds also share converted files (`core.autocrlf=true`, `eol=crlf`, `ident`,
`working-tree-encoding`) when the donor holds exactly the bytes Git writes,
read from `git archive`. External
filters and sparse checkout still delegate. Submodules are Git-written empty
directories, never copied or recursively initialized from a donor.
See the [changelog](CHANGELOG.md) for release details.

With `TWIG_TRACE=1`, native creation reports verified-clone and Git-written path
counts. Delegation reports its specific cause; an attribute refusal names the
path, attribute, and value (for example `filter="unspecified" on "file.txt"`).
Do not override repository attributes with `-text` just to force the fast path:
that changes Git checkout semantics rather than proving the bytes are correct.

## Benchmarks

### v0.5 checkout coverage

Share unchanged files across revisions, builtin conversions and submodule checkouts.
These synthetic workloads measure a frozen pre-release candidate, not the published
binary; faster creation is not guaranteed. They do not measure experimental `adopt`.

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

Other filesystems: [APFS (case-sensitive)](bench/results/sha256-c4989db26497/macos-apfs-case-sensitive/2026-11-17/checkout-v05/comparison.svg) · [XFS](bench/results/sha256-a73d956e3301/linux-xfs/2026-11-17/checkout-v05/comparison.svg) · [Btrfs](bench/results/sha256-a73d956e3301/linux-btrfs/2026-11-17/checkout-v05/comparison.svg).
Speed varies by workload. [Full results and limitations](bench/preview.md).

<!-- twig-preview:end -->

<details>
<summary>v0.4 real-repository benchmarks</summary>

<!-- twig-bench:start -->

<!-- Generated from bench/results/readme.json by twig-readme-table. Do not edit numbers. -->

| Repo | `git worktree` | `git twig` | Improvement |
|---|---|---|---|
| [Workers starter (10 sequential)](bench/results/sha256-428608b8bf13/macos-apfs-mixed/2026-11-17/final/workers-starter-host.json) | 619ms / 592 KiB | 4.41s / 308 KiB | 7.13× slower / **1.9× less space** |
| [git-twig (10 sequential)](bench/results/sha256-428608b8bf13/macos-apfs-mixed/2026-11-17/final/twig-host.json) | 575ms / 5.2 MiB | 4.35s / 508 KiB | 7.57× slower / **10.6× less space** |
| [Axo](bench/results/sha256-428608b8bf13/macos-apfs-mixed/2026-11-17/final/axo-host.json) | 543ms / 30.8 MiB | 1.12s / 1.2 MiB | 2.06× slower / **26.7× less space** |
| [Linux kernel](bench/results/sha256-428608b8bf13/macos-apfs-mixed/2026-11-17/final/linux-images.json) | 40.96s / 1.8 GiB | 48.58s / 44.7 MiB | 1.19× slower / **41.9× less space** |
| [OpenClaw](bench/results/sha256-428608b8bf13/macos-apfs-mixed/2026-11-17/final/openclaw-host.json) | 8.92s / 919 MiB | 15.71s / 31.1 MiB | 1.76× slower / **29.5× less space** |
| [Pulumi docs](bench/results/multi-build/mixed/2026-11-17/issue5/after-host.json) | 4.68s / 1.2 GiB | 7.40s / 7.9 MiB | 1.58× slower / **155.8× less space** |

Each cell is **creation + first `git status` time / additional disk space**. Batch sizes differ; time is batch creation plus each new worktree's first status, run one at a time. † Includes failed Git attempts. [Setup and raw results](bench/results/readme-details.md).

<!-- twig-bench:end -->

</details>

For Linux CLI time/allocation measurements, see [Linux benchmarks](bench/linux.md).

## Develop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Linux CI tests the host filesystem plus reflink-enabled XFS and Btrfs images,
requiring native CoW on both. Locally on a supported Linux filesystem, run
`TWIG_TEST_REQUIRE_NATIVE=1 cargo test -p git-twig`. Tests and benchmarks retain
fixtures under `target/`; disk use grows. See the
[design contract](docs/rfds/RFD0001-verified-cow-worktrees.md),
[Linux correctness boundaries](docs/rfds/RFD0012-linux-correctness-and-measurement.md),
[changelog](CHANGELOG.md), and [release operations](docs/releases.md).

[MIT](LICENSE)
