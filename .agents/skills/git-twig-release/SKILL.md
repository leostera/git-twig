---
name: git-twig-release
description: >-
  Release git-twig end to end: prepare a stable version and changelog, integrate tested code, publish the immutable tag, crate, GitHub archives/checksums, Homebrew formula, Scoop manifest and npm package, deploy the website, and verify public artifacts. Use when shipping git-twig or finishing/recovering its release pipeline.
license: MIT
---

# Release git-twig

This skill is a procedure, not permission to publish. Obtain explicit owner approval
for main/tag pushes, crates.io publication, the GitHub release/environment gate,
Homebrew and Scoop publication, npm publication, website deployment and
verification uploads. Respect the
harness's confirmation mechanism. Never approve your own confirmation prompt.
Credentials already being available does not authorize publication.

Read `$MAIN/AGENTS.md`, `$MAIN/docs/releases.md`, current CI/release
workflows and version helpers before acting. Repository instructions win over
examples here. This is a maintainer skill, separate from `git twig skill`, which
teaches ordinary worktree use.

## 1. Establish scope and exact source

- Choose the version with the owner. Set `VERSION=X.Y.Z`, `TAG=v$VERSION`, and
  `MAIN` to the repository's real main clone (normally
  `$HOME/Developer/github.com/leostera/git-twig/main`).
- Inspect dirty state, worktree registrations and fresh origin/main. Preserve
  unrelated work. Use a short-lived sibling concurrency worktree, created using
  a verified Twig binary; do not silently upgrade the user's installed CLI.
- Do not ship experimental branches wholesale. Confirm which changes are intended
  to ship. Do not invent fixes, filesystem support or agent integrations.
- Check existing tags/releases and registry availability. `cargo info
  git-twig@$VERSION --registry crates-io` updates the index. A missing version is
  different from an authentication/network failure; raw crates.io API requests
  can be refused even while Cargo works.
- Confirm the GitHub release environment's required reviewers remain enabled.
  Do not weaken protections to make a release finish.

## 2. Prepare the version and public documentation

Update workspace Cargo.toml and both workspace package entries in Cargo.lock.
Only `git-twig` is publishable; `twig-bench` stays unpublished.

Move verified changes from Unreleased into a dated `[X.Y.Z]` changelog entry and
add its release link. Leave an empty Unreleased heading for subsequent changes.
Update README usage, manual, bundled user skill, adoption guide, release docs and
website onboarding/version requirements together.

Keep the root README user-facing: benefits, installation, examples, benchmarks
and license. Operational checklists belong in this skill or release docs.

Preserve raw benchmark data and original observations. Frozen candidate results
are not measurements of a different release binary. Keep source/toolchain
provenance and failed/unstable groups; do not pool different machines/builds.
The website shows **stock Git versus latest Twig only**, without cross-revision
or historical-version views. Full archived data can retain other comparisons.

Regenerate derived data rather than editing cells:
```sh
cd "$MAIN"
python3 bench/publish_preview.py
python3 bench/publish_preview.py --check
cargo run -p twig-bench --bin twig-readme-table --locked -- --check
python3 scripts/check-release-version.py "$VERSION"
python3 scripts/release-notes.py "$VERSION"
```

Run commands in the actual preparation checkout while editing; the example MAIN
paths above apply after integration. Do not accidentally test a different checkout.

## 3. Validate, inspect the package, integrate

Run formatting, strict workspace Clippy, workspace tests, Rust 1.95 tests,
release builds, benchmark Python/Bun tests, website typecheck/unit/build and
desktop/mobile browser tests. Follow the current CI's minimum-Git and native
filesystem gates; clone support alone does not certify Linux deduplication.

Inspect `cargo package -p git-twig --list`: license, README, manual, bundled user
skill and required sources must be present; generated repositories, images,
binaries, credentials and local fixtures must not be packaged.
`--allow-dirty` is acceptable for an explicitly preliminary dry-run only.

Commit the coherent preparation, fetch/rebase in the concurrency worktree,
fast-forward clean main and push it. Do not park finished work on a local branch.
Wait for the **exact integrated commit's** complete CI success.

From clean synced main, run:
```sh
cargo publish -p git-twig --dry-run --locked
```
This must package and verify successfully without `--allow-dirty`.

## 4. Freeze the tag and build archives

Check all of these before tagging:
```sh
cd "$MAIN"
git fetch origin
test "$(git branch --show-current)" = main
test "$(git rev-parse --git-dir)" = "$(git rev-parse --git-common-dir)"
test -z "$(git status --porcelain)"
test "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)"
python3 scripts/check-release-version.py "$VERSION"
```

Record the commit, then create an **annotated** tag and push that exact ref:
```sh
git tag -a "$TAG" -m "git-twig $TAG"
git push origin "refs/tags/$TAG"
git rev-parse "$TAG^{commit}"
```

Never move/recreate a published tag. Avoid unrelated main changes until the crate
is published from this commit.

The tag workflow builds macOS arm64/x86_64 and Linux x86_64. Linux archives must
keep the Ubuntu 22.04/glibc 2.35 build baseline; newer host glibc can create unusable
downloads even when source tests pass. Notes and formula must be generated from
the immutable tag, not whichever main commit exists when publication runs.

Find the run for this exact tag commit, not an older successful release run.
Wait for all build jobs to succeed. Download its workflow artifacts while the
publish job waits for the release environment reviewer.

## 5. Verify the actual built binaries

Inspect all archives for binary, MIT license, README, changelog and manual.
Record SHA-256 values. Test versions/help and native creation/adoption against
the **archive binary**, not just a locally compiled equivalent.

To reuse the integration suite without changing tracked test code:
1. Build the tagged worktree test with `cargo test -p git-twig --test worktrees
   --no-run --locked --message-format=json` into a **dedicated ignored
   CARGO_TARGET_DIR**.
2. Record the JSON compiler-artifact entry's worktrees executable.
3. Replace only that dedicated build directory's generated `debug/git-twig`
   with the archive binary, then execute the recorded test runner directly.
   Do not run Cargo again before the test: it may rebuild the CLI.
4. Use `TWIG_TEST_REQUIRE_NATIVE=1` and appropriate owned fixture roots. On
   independently provisioned XFS/Btrfs, include ignored dedupe tests.
   Preserve logs and distinguish environment-specific early returns from coverage.

On Apple Silicon, x86_64 checks under Rosetta are **not native Intel hardware**
validation. Linux Docker tests on a Mac are **not remote bare-metal Linux** tests.
Also run the published Linux executable inside Ubuntu 22.04 to catch glibc loader
errors. Keep fixtures/images under ignored target paths; cleanup requires approval.

## 6. Publish the crate, then finish GitHub publication

While main still equals the tag commit:
```sh
cd "$MAIN"
test -z "$(git status --porcelain)"
test "$(git rev-parse HEAD)" = "$(git rev-parse "$TAG^{commit}")"
cargo publish -p git-twig --locked
cargo info "git-twig@$VERSION" --registry crates-io
```

Use existing secure credentials or the person's secure login mechanism. Never
print Cargo credentials/tokens, ask for them in chat, or publish `twig-bench`.

Install the registry crate into an explicitly isolated `--root`, with the exact
version and `--locked`; verify CLI behavior. Inspect its registry source's
.cargo_vcs_info.json: the source commit must equal the tag and not be dirty.
Do not alter the user's installed binary or PATH without separate approval.

With owner approval, satisfy the GitHub release environment reviewer gate.
Publication is not done while that job is waiting. The workflow should create a
non-draft stable release with `--verify-tag --title "$GITHUB_REF_NAME"`, upload
the three archives, the Windows zip, SHA256SUMS and generated formula, then
update current main's Formula/git-twig.rb and bucket/git-twig.json (Scoop) without
force-pushing.

Do not manually create a second release if the workflow owns publication.
Check release name, tagName, stable/latest status, asset inventory and tag commit.

## 7. Verify public downloads and Homebrew

Download the **published** release assets separately from workflow artifacts.
Verify SHA256SUMS and compare their archive hashes to the binaries already tested.
Checksum entries must be basenames so `sha256sum -c SHA256SUMS` works after download.

Fetch/fast-forward main after the formula bot commit. Confirm the main formula
and released formula asset match, and each platform URL/hash matches its archive.
A formula bot push may not trigger CI; inspect its exact diff.

Homebrew resolves formulas through a tap; fetching a loose .rb path outside a tap
can fail even when the formula is valid. Use the explicit repository URL:
```sh
HOMEBREW_NO_AUTO_UPDATE=1 brew tap leostera/git-twig https://github.com/leostera/git-twig
HOMEBREW_NO_AUTO_UPDATE=1 brew fetch --formula leostera/git-twig/git-twig
```
Inspect/preserve any existing tap edits before updating it. Tap/fetch changes
package-manager metadata/cache, not the installed CLI. Do not silently upgrade it.

For npm, the `npm` job waits on the release environment after the GitHub release;
approve it only with owner approval. Then, in an isolated prefix, run
`npm install -g --prefix <temporary> git-twig@VERSION`, check `npm view
git-twig@VERSION dist.attestations` shows provenance, and run `git twig --version`
from that prefix. Each binary in the package must hash equal to its archive's.

On Windows, confirm bucket/git-twig.json's URL and hash match the published zip,
then install through an isolated Scoop (`install.ps1 -ScoopDir <temporary>`),
`scoop bucket add git-twig https://github.com/leostera/git-twig` and
`scoop install git-twig/git-twig`. Check `git twig --version` through the shim and
one native `git twig new` on a Dev Drive. Afterwards remove the temporary Scoop,
its user PATH entry and the SCOOP variable it set.

## 8. Deploy and test the production website

Deploy only from the real, clean, synced main clone, never a feature/experiment
worktree. Recheck branch, git-dir/common-dir, dirty state and HEAD=origin/main.
Follow a configured CI deployment if one exists; otherwise use the current
manual site scripts.

If Cloudflare has multiple accounts, cf deploy cannot choose non-interactively.
Verify the **leostera** account and existing `twig-site` / `twig.leostera.dev`
target, then explicitly set CLOUDFLARE_ACCOUNT_ID. Account IDs are identifiers;
API tokens must remain private. Do not deploy to another account or invent a
replacement production worker.

Run site checks/build from main and deploy:
```sh
CLOUDFLARE_ACCOUNT_ID="$VERIFIED_ACCOUNT_ID" \
  bun run --cwd sites/twig.leostera.dev deploy
```

Record the deployed Worker version. Verify HTTP and HTTPS roots return HTML
without unwanted redirects, and /llms.txt returns plain-text onboarding for the
published version. Run browser tests against the **live URL**, with no local
webServer accidentally satisfying them. Check desktop/mobile layout, clipboard
onboarding, filesystem/metric controls and stock-Git/latest-Twig-only rows.

## 9. Finish and recover safely

Publish a compact verification JSON and selected non-sensitive logs as additional
release assets. Record tag/commit, CI IDs, public archive hashes, package source
identity, actual native/Rosetta/container coverage, Homebrew check and live site
version/tests. Never upload credentials, unrelated account lists or giant fixtures.

Report actual completion for every channel: tag, crate, GitHub binaries/checksums,
Homebrew and website. State any missing checks; do not call a local commit,
workflow start, queued crate or website build a completed release.

If anything fails, preserve the first failure and diagnose it. Never silently
retry failed benchmark points, replace old measurements, remove user worktrees,
delete previous assets or rewrite release history. If a release already exists
and only formula/site work failed, finish those steps without blindly rerunning
release creation. For a Linux archive repair, use the reviewed rebuild-linux
workflow and preserve superseded assets; never mutate the tag.

Maintainer-only follow-ups, including this skill, can land after the tag without
moving it. Keep them on main and out of the user README. Install/update the
canonical skill in the owner's global skills directory only when requested.
