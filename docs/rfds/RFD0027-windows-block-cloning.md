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
  number, the Windows counterpart of comparing device numbers.
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
- `adopt`, `adopt --prepare` and `adopt --dedupe` still refuse on Windows.
- Paths Windows cannot represent (control characters, reserved names, trailing
  dots) are left to Git, which refuses them.

## Validation

The full suite runs on Windows 11 with Git 2.56: on a Dev Drive with
`TWIG_TEST_REQUIRE_NATIVE=1`, and on NTFS where every case delegates.
Tests that observe staging or a native fallback reason now ask the volume
itself whether it can clone, rather than asking Twig.
