# Releases

Version 0.5.0 is published on [crates.io](https://crates.io/crates/git-twig) and
[GitHub](https://github.com/leostera/git-twig/releases/tag/v0.5.0).
Crates.io publication is manual and requires owner approval.
The [maintainer release skill](skills/git-twig-release/SKILL.md) covers the complete
prepare, publish, verify and deploy sequence, including recovery boundaries.

Before publishing:
1. Run all local/CI checks. Verify benchmark provenance: frozen candidate results
   must not be relabeled as measurements of a different release binary. Add the next
   [changelog](../CHANGELOG.md) entry for implemented and verified changes;
   do not describe planned fixes as shipped.
2. Confirm the git-twig crate name/version is available on crates.io.
3. Review `cargo package -p git-twig --list`; include the license and bundled skill,
   never local fixtures. Run `cargo publish -p git-twig --dry-run --locked`.
4. Bump the workspace version in Cargo.toml.
5. Configure GitHub's **release** environment with required reviewers. Ensure the
   release bot may update Formula/git-twig.rb on main, or adapt it to open a PR.
6. Review GitHub Actions permissions and the generated formula.

With owner approval, push a vVERSION tag matching Cargo.toml. The release workflow
builds macOS arm64/x86_64 and Linux x86_64 archives (including `git-twig.1`), a
Windows x86_64 zip (`git-twig.exe` with a static C runtime, no man page; the
build fails if it imports `VCRUNTIME140.dll`), SHA256SUMS, a stable Homebrew
formula for macOS and Linux, and a Scoop manifest for Windows. It generates notes/formula from the immutable tag, creates a GitHub release with
that tag, and commits Formula/git-twig.rb and bucket/git-twig.json on current main. SHA256SUMS uses archive
basenames so a download can be checked with `sha256sum -c SHA256SUMS`. Native CoW supports macOS/APFS, Linux on Btrfs or
reflink-enabled XFS, and Windows ReFS including Dev Drive; unsupported filesystems
(including NTFS) delegate to Git. The Windows binary is not Authenticode-signed. macOS binaries are
not signed/notarized yet. Linux prebuilt archives are built on Ubuntu 22.04
(glibc 2.35); source installs remain available for other libc environments.

To repair an existing Linux archive without changing its source tag, dispatch
`rebuild-linux.yml`. It checks older-glibc CLI output, requires the release
environment approval, and preserves superseded assets under unique names rather
than deleting them. The replacement checksum list and Homebrew formula are
published together.

Homebrew needs the explicit remote because the repository is not named
homebrew-git-twig:

```sh
brew tap leostera/git-twig https://github.com/leostera/git-twig
brew install leostera/git-twig/git-twig
```

Use --HEAD only when intentionally installing development source. Both source
and stable Homebrew installs include the manual for `git twig --help`. Cargo does not install manuals;
its users can use `git twig -h` or `git-twig --help`.

Crates.io publishing is a separate owner-approved action:
publish only `git-twig` with `cargo publish -p git-twig --locked`. Its library and
CLI are packaged together; there is no separate core crate or publication order.
Use cargo login locally, never paste credentials into chat. Do not publish
`twig-bench`.

Workflow YAML and formula generation can be checked locally; actual hosted builds,
downloaded binary installs and registry publication still require end-to-end
validation after repository publication.
