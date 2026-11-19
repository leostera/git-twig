# RFD0028 — Share converted files against Git's own checkout bytes

- Status: implementation snapshot
- Author: leostera
- Date: 2026-11-19

## Decision and motivation

Share files that checkout converts, not just files it copies verbatim. Until
now a path under `core.autocrlf=true`, `eol=crlf`, legacy `crlf`, `ident` or
`working-tree-encoding` was always written by Git. Git for Windows sets
`core.autocrlf=true` by default, so on Windows Twig shared almost nothing but
binary files: 1 of 52 files in a small repository, against 52 of 52 with the
setting off (RFD0027).

A donor checked out under the same configuration already holds the bytes Git
would write. Twig now clones it and keeps the clone only if it equals those
bytes exactly.

## Oracle

The bytes come from Git, never from a reimplementation of its conversion.
`git archive` writes each file through `convert_to_working_tree`, the function
checkout uses: end-of-line conversion with `text=auto` detection, `ident` and
`working-tree-encoding`, with attributes from the archived tree plus
`info/attributes` and `core.attributesFile`, the same set
`check-attr --cached` reports. Twig runs one
`git archive --format=tar <commit>` with the staging configuration and streams
it: each wanted entry is compared with its private clone, everything else is
skipped. With few converted paths they are named as literal pathspecs.

`git cat-file --batch --filters` would be the natural oracle, but it refuses
`<rev>:<path>` input with "missing path" in Git 2.56; one `cat-file --filters`
process per file would cost milliseconds each on Windows.

## Contract

- A converted clone is kept only if its length and every byte equal the
  archived entry, followed by the end of both. Otherwise Git writes the path, as
  before.
- `export-subst` and `export-ignore` make archives differ from checkouts. Paths
  with either attribute are never compared; Git writes them.
- External `filter` drivers still delegate the whole creation (they could
  differ between runs, and must run once).
- Automatic-binary candidates (RFD0015) keep their raw comparison, and fall
  back to the archive when the blob turns out to be text.
- Partial clones skip the archive: it would fetch each absent blob on its own.
- No stat data is recorded for converted files (RFD0024). Git refreshes them
  by content while recording the staged index, as for any file under a
  conversion policy.

## Validation

`converted_donor_bytes_are_shared_when_they_equal_git_checkout` checks
`core.autocrlf=true`, `text eol=crlf`, legacy `crlf`, `ident` and
`working-tree-encoding` against stock `git worktree add`: every byte, Git
status, the number shared, a stale donor file written by Git, and an
`export-subst` path never compared. The existing differential line-ending
matrix still passes unchanged. On Windows with Git for Windows defaults, a
50-file repository checked out with CRLF now shares all 50 files.
