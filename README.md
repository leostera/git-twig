# git twig 🌱

Make as many Git worktrees as you like: `git twig new` creates an ordinary
linked worktree that shares unchanged files with one you already have, so a
new checkout of Pulumi's docs adds 12 MiB instead of 1.2 GiB. Every shared byte
is verified against the commit first, and edits stay private to the worktree
that makes them.

It is built for coding agents that each want their own checkout, and works on
macOS (APFS), Linux (XFS, Btrfs) and Windows (ReFS and Dev Drive). Where the
filesystem cannot share files, Twig runs `git worktree add` for you.

## Install

macOS and Linux, with [Homebrew](https://brew.sh):

```sh
brew tap leostera/git-twig https://github.com/leostera/git-twig
brew install leostera/git-twig/git-twig
```

Windows, with [Scoop](https://scoop.sh):

```powershell
scoop bucket add git-twig https://github.com/leostera/git-twig
scoop install git-twig/git-twig
```

Anywhere with Node, from npm (one package, no download at install time):

```sh
npm install -g git-twig
```

Or from crates.io with Rust 1.95+ (on Windows, with the Visual Studio C++ build
tools): `cargo install git-twig --locked`. Release archives with SHA-256 sums
are on the [releases page](https://github.com/leostera/git-twig/releases).

Twig needs Git 2.40 or later. On Windows, prefer Scoop to npm: npm's launcher
adds about 0.4 s to each command there.

## Use

From your main checkout:

```sh
git twig new -b feat/x ../feat-x HEAD
git twig list
git twig remove ../feat-x
```

`git twig new` takes the same arguments as `git worktree add`, and the result
is a normal worktree: Git, editors and build tools see nothing unusual. Local
changes and ignored files are not copied, just as with Git. Run `git twig -h`
for every command. On Windows, use `-h`: `--help` asks Git for HTML docs it
doesn't ship.

For coding agents, `git twig skill` prints a skill to give your agent. It tells
the agent to pick the right checkout, branch and location, to create worktrees
with Twig, and to ask before cleaning up.

## How much it saves

<!-- twig-bench:start -->

<!-- Generated from bench/results/readme.json by twig-readme-table. Do not edit numbers. -->

**Additional disk space**

| Repo | Linux · XFS | Linux · Btrfs | Windows · Dev Drive |
|---|---|---|---|
| Workers starter (10 in a row) | 584 KiB → 304 KiB, **1.9× less** | 256 KiB → 224 KiB, **1.1× less** | 0 KiB → 0 KiB, same disk usage |
| git-twig (10 in a row) | 5.4 MiB → 680 KiB, **8.1× less** | 5.2 MiB → 1.4 MiB, **3.7× less** | 4.0 MiB → 1.1 MiB, **3.5× less** |
| Axo | 31.2 MiB → 1.8 MiB, **17.8× less** | 31.4 MiB → 4.0 MiB, **7.9× less** | 27.3 MiB → 6.0 MiB, **4.6× less** |
| OpenClaw | 925 MiB → 46.2 MiB, **20.0× less** | 953 MiB → 112 MiB, **8.5× less** | 820 MiB → 197 MiB, **4.2× less** |
| Pulumi docs | 1.2 GiB → 11.8 MiB, **105.2× less** | 1.2 GiB → 23.1 MiB, **54.3× less** | 1.2 GiB → 50.9 MiB, **24.0× less** |
| Linux kernel | 1.8 GiB → 70.6 MiB, **26.8× less** | 1.8 GiB → 171 MiB, **11.1× less** | not measured |

**Time until the worktree is ready**

| Repo | Linux · XFS | Linux · Btrfs | Windows · Dev Drive |
|---|---|---|---|
| Workers starter (10 in a row) | 162ms → 966ms | 168ms → 1.07s | 3.47s → 28.06s |
| git-twig (10 in a row) | 259ms → 1.15s | 265ms → 1.24s | 3.80s → 29.25s |
| Axo | 379ms → 810ms | 381ms → 822ms | 1.75s → 7.79s |
| OpenClaw | 6.84s → 12.70s | 8.15s → 12.47s | 32.07s → 1m43s |
| Pulumi docs | 4.94s → 2.80s | 5.93s → 3.40s | 13.58s → 24.01s |
| Linux kernel | 10.92s → 15.60s | 12.52s → 12.77s | not measured |

`git worktree` → `git twig`. Space is the disk the new worktrees add; time is creation plus the first `git status`, when a worktree is ready to use. Workers starter and git-twig rows are 10 worktrees created in a row, the others one. Medians of three paired rounds on fresh filesystem images. † Includes failed Git attempts. [Setup, every metric and raw results](bench/results/readme-details.md).

<!-- twig-bench:end -->

Savings grow with repository size, because the files Twig shares are the bulk
of a checkout while Git's own metadata stays per worktree. Linux XFS shares the
most; Btrfs and Windows ReFS keep more per worktree (ReFS, for example, keeps
the last partial cluster of every file private). Twig often takes longer to
create a worktree than Git does, because it verifies every shared file before
handing it over; on Linux, Pulumi is faster with Twig.

## Organize existing worktrees

`git twig adopt` moves an existing clone and its worktrees into one sibling
layout, so agent checkouts sit beside the main one rather than inside it:

```sh
git twig adopt --dry-run --worktrees-dir=/path/to/repo
git twig adopt --worktrees-dir=/path/to/repo
```

It previews `/path/to/repo/{main,feat-x,...}` and asks before moving anything.
Without the flag it asks for a root and remembers it, and afterwards
`git twig new -b feat/x` puts new worktrees there. Moves keep branches and
staged, dirty, untracked and ignored files, never overwrite anything and stay
on one filesystem; submodule layouts are refused.

Close agents, editors, terminals and builds in those worktrees first: their
stored paths are not updated, and on Windows a directory cannot move while a
program has a file open in it (Twig then undoes every move).

After moving, Linux and Windows ReFS share matching files in place. On macOS,
`--compact` does the same offline, once you have saved and closed everything
using those worktrees:

```sh
git twig adopt --compact --dry-run --worktrees-dir=/path/to/repo
git twig adopt --compact --worktrees-dir=/path/to/repo
```

Compaction replaces matching files with verified clones, keeps their contents
and supported metadata, and retires the old copies only after verification. A
failure keeps both copies and a recovery journal; do not restart agents
blindly after one. Automation needs `--compact --offline --yes`; `--move-only`
turns sharing off.

## Where it shares files

| Platform | Filesystem | What Twig does |
|---|---|---|
| macOS | APFS | Shares files (`clonefile`) |
| Linux | XFS with `reflink=1`, Btrfs | Shares files (`FICLONE`), also under OverlayFS in the tested layouts |
| Windows 11 | ReFS, Dev Drive | Shares files (block cloning) |
| Linux | ext4, tmpfs, exFAT, NTFS, F2FS | Runs `git worktree add` |
| Windows 11 | NTFS | Runs `git worktree add` |

Twig tries the clone itself rather than trusting the filesystem's name, so
other reflink-capable filesystems may work; OpenZFS 2.2 passes the checkout
tests but is not verified, and NFS and bcachefs are untested. The source
checkout, the new worktree and the Git directory must be on the same
filesystem. On Windows, paths of 248 characters or more need
`core.longpaths=true`, as they do for Git. See [Linux test
setup](bench/linux.md) for the setups covered.

Most checkout settings keep sharing, including `text=auto eol=lf`,
`core.autocrlf=true` (the Git for Windows default), `eol=crlf`, `ident` and
`working-tree-encoding`: Twig keeps a file only when it holds exactly the bytes
Git would write, and lets Git write the rest. External filters and sparse
checkouts use `git worktree add`. Submodules stay empty directories, as with
Git.

Set `TWIG_TRACE=1` to see which backend Twig chose and why it delegated, for
example `filter="unspecified" on "file.txt"`. Do not set `-text` just to get
sharing: that changes what Git checks out.

## Safety

Twig writes the new worktree in private staging under the Git directory,
verifies every shared file against the commit, then registers it. Files are
cloned, never hardlinked, so writing to one never changes another checkout.
Staging is removed after success, or on SIGINT, SIGTERM or SIGHUP before
registration. Other failed or interrupted attempts keep staging and may leave a
partial registered worktree: inspect it before retrying. Twig does not
implement every `git worktree` option; it hands unsupported ones to Git.

## Benchmarks

Every number above comes from committed raw runs. [Methodology](bench/real-repositories.md)
· [Linux setup](bench/linux.md) · [Checkout-policy workloads](bench/preview.md).

## Develop

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Linux CI tests the host filesystem plus reflink-enabled XFS and Btrfs images
and requires native sharing on both; Windows CI tests NTFS and a ReFS Dev
Drive. Locally on a supported filesystem, `TWIG_TEST_REQUIRE_NATIVE=1 cargo
test -p git-twig` makes native coverage mandatory. Tests and benchmarks keep
fixtures under `target/`. See the [design
contract](docs/rfds/RFD0001-verified-cow-worktrees.md), the
[changelog](CHANGELOG.md) and [release operations](docs/releases.md).

[MIT](LICENSE)
