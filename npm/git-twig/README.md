# git-twig

Lightweight Git worktrees for agents. Twig creates ordinary linked worktrees
that share unchanged files with the original checkout through native
copy-on-write, using up to 99% less disk space.

```sh
npm install -g git-twig
git -C main twig new -b feat/x ../feat-x HEAD
git twig skill   # instructions for your coding agent
```

To onboard your coding agent, ask it to run `git twig skill` and follow it.
The skill explains when and how to create worktrees with Twig, and to ask
before cleaning up.

Native cloning needs macOS/APFS, Linux on Btrfs or reflink-enabled XFS, or
Windows on ReFS or a Dev Drive. Elsewhere Twig uses stock Git. Requires Git
2.40+. This package ships prebuilt binaries for macOS (arm64, x64), Linux
(x64, arm64, glibc) and Windows (x64); nothing is downloaded at install time.

Documentation, benchmarks and other installers: https://github.com/leostera/git-twig
