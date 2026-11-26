# git-twig

One publishable package, git-twig, with a thin CLI and materialization in its library.
The workspace also contains twig-bench, which is unpublished development tooling.
Integration: direct to main, after local tests. Do not publish or tag releases without confirmation.
Use Twig for sibling working checkouts. Fast-forward tested, coherent increments into
main promptly; a local commit is not integrated work. Promote experiments through a
fresh concurrency branch, and keep explicitly rejected prototypes out of main.
For releases, read .agents/skills/git-twig-release/SKILL.md. Keep maintainer process
out of the user README and separate from the bundled worktree-use skill.
Never claim full Git worktree compatibility; unsupported operations are delegated.
Run cargo fmt --all --check, cargo clippy --workspace --all-targets -- -D warnings,
and cargo test --workspace before committing.
Benchmarks use release binaries, compare actual CLI operations, retain versioned JSON,
and generate charts from those files. Never claim exclusive APFS allocation from du.
Keep user source and existing destinations untouched. Never hardlink files to source.
Synthetic test/benchmark fixtures are retained under target/ until explicitly removed.
