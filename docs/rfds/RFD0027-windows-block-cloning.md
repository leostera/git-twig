# RFD0027 — Windows: ReFS block cloning with the same contract

- Status: implementation snapshot
- Author: leostera
- Date: 2026-11-19

## Decision and motivation

Twig runs on Windows with the same contract it has on macOS and Linux: share
bytes through the filesystem's copy-on-write, verify every shared byte against
the target, and let stock Git do everything else. The goal is parity, not a
reduced Windows mode.

Windows has copy-on-write on ReFS, including the Dev Drive volumes Microsoft
recommends for source code: `FSCTL_DUPLICATE_EXTENTS_TO_FILE` makes two files
share clusters, and fails rather than copying. That is the counterpart of APFS
`clonefile` and Linux `FICLONE`. NTFS has no such operation, so on NTFS Twig
delegates to stock Git, as it does on ext4.

This builds on two community contributions. Issue #4 and its
`windows-block-cloning` branch (mateo-m) showed block cloning on ReFS and Dev
Drive, with a test for files that end inside a cluster. PR #3 (InventingMaya)
made the workspace build on Windows, added a Windows CI job and release archive,
and fixed line-ending assumptions in the repository's own checks.

## Contract on Windows

- **Cloning.** A donor file is opened with `FILE_FLAG_OPEN_REPARSE_POINT`, so a
  path swapped for a symlink or junction is opened as itself and refused as not
  a regular file. The destination is created exclusively and never replaces an
  entry. ReFS clones only between files with equal sparse and integrity
  settings, so the destination copies both before cloning. Regions are cluster
  aligned: the last one rounds up past the end of both files, as Windows
  documents, and requests stay below 4 GiB each.
- **Verification is unchanged.** Every clone is hashed against its blob before
  installation, by the same parallel raw hashing as on Unix.
- **Early delegation.** Twig asks the destination volume for
  `FILE_SUPPORTS_BLOCK_REFCOUNTING` before staging anything. On NTFS the
  fallback costs one volume query, and says why: "volume does not support block
  cloning (ReFS or a Dev Drive is required)".
- **One volume.** Staging, destination and donor must share a volume serial
  number, the Windows counterpart of comparing device numbers. The check
  follows links, as `fs::metadata` does on Unix: a volume mounted at a folder
  (common for Dev Drives in a VHDX) is the volume inside the folder, not the
  one holding the mount point. Asking without following the mount point sent
  every worktree beside such a repository to stock Git.
- **Paths.** `fs::canonicalize` returns verbatim `\\?\C:\...` paths, which Git
  for Windows does not accept. Twig resolves them, then uses the ordinary form
  when it names the same file, and refuses a path with no ordinary form. Paths
  Git prints as `C:/a/b` are shown as `C:\a\b`.
- **Index stat data.** Git for Windows records the creation time as ctime, the
  last write time as mtime, and zero for device, inode, owner and group. Twig
  records verified files the same way, so the first `git status` in a new
  worktree does not reread them (RFD0024).
- **Modes.** Windows has no executable bit and Git for Windows sets
  `core.filemode=false`. Staging uses the repository's setting there instead of
  forcing `core.filemode=true`.

## Adoption on Windows

- **Layout moves** use `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` or
  `MOVEFILE_COPY_ALLOWED`: the filesystem refuses an occupied destination
  atomically and never copies across volumes, like `renameat2(RENAME_NOREPLACE)`.
  Checkout identity is the volume serial number and file index.
- **Windows will not move a directory while any file in it is open**, whatever
  its sharing mode, nor while it is a process's current directory. Twig leaves
  its own working directory first, and releases the registration lock, which
  lives in main's Git directory, before moving. A Twig registering meanwhile
  holds that lock file open, so moving main fails and every move is undone. An
  editor, terminal or build inside a worktree has the same effect; the error
  says so.
- **In-place deduplication.** Linux relies on `FIDEDUPERANGE`, which compares
  and shares under the kernel's own lock. ReFS has no such call. Twig opens
  both files denying other writers and deletion, compares every byte under
  those handles, then block-clones the donor's clusters over the target, which
  keeps its file identity. Windows refuses such a handle while another program
  has the file open for writing or mapped writable, so those files are skipped,
  and nothing can write either file between comparison and sharing. The target's
  last write time is pinned for the handle, so Git's stat data stays valid.
  Both files must have equal sparse and integrity settings.
- **Walking paths** opens one component at a time without following reparse
  points, holding each directory open and denying its deletion, so no name on
  the way can be swapped for a junction meanwhile. It is the counterpart of
  `openat(O_NOFOLLOW)`.
- **Private scratch** for `--prepare` and the recovery journal inherit the ACL
  of the directory holding them; Windows has no mode bits to check.

## Changes that apply everywhere

- **`core.symlinks=false` no longer falls back.** Git writes each symlink entry
  as a plain file holding its target; Twig still shares every other file.
  Twig recreates symlinks itself only on Unix with `core.symlinks` enabled. On
  Windows Git writes every symlink, choosing file or directory links as it
  always does. Git for Windows sets `core.symlinks=false` by default, so this
  matters most there.
- **`core.eol=native` means CRLF in Git for Windows.** Twig treated `native` as
  LF everywhere, and would have left LF where stock Git writes CRLF for `text`
  paths with `core.autocrlf=false`. The differential checkout tests found this
  on the first Windows run.

## Not covered yet

- `core.autocrlf=true`, the Git for Windows default, converts every text file
  on checkout. Twig shares only files whose checkout is the identity, so such
  repositories share binary files only. Comparing donors with Git's converted
  bytes would lift this on every platform; it is separate work.
- Offline compaction (`--compact` on macOS) has no Windows counterpart, as it
  has none on Linux: in-place deduplication takes its place.
- Paths Windows cannot represent (control characters, reserved names, trailing
  dots) are left to Git, which refuses them.

## Validation

The full suite runs on Windows 11 with Git 2.56: on a Dev Drive with
`TWIG_TEST_REQUIRE_NATIVE=1`, and on NTFS where every case delegates.
Tests that observe staging or a native fallback reason now ask the volume
itself whether it can clone, rather than asking Twig. Adoption tests run on
Windows too, with Windows cases for open files blocking a move and rolling it
back, and for deduplication skipping files open for writing, mapped writable,
reached through a junction or hardlinked.
