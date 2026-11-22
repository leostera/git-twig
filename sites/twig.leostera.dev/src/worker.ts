import { Hono } from "hono";
import skill from "../../../crates/git-twig/skills/git-twig/SKILL.md?raw";

export const onboarding = `# Twig — git-worktrees for agents

Twig creates ordinary linked Git worktrees using native copy-on-write.
Native cloning needs macOS/APFS, Linux on Btrfs or reflink-enabled XFS, or
Windows on ReFS or a Dev Drive. Elsewhere, including NTFS, Twig uses stock Git.
This is not a sandbox; writes remain private but Git metadata is shared.

## Install

Check whether git twig --version, git twig skill and git twig new --help work.
These instructions require v0.6.0 or newer for Windows support and adopt layouts;
add is no longer a Twig command. Otherwise,
choose one installation method. Git 2.40+ is required.

With Rust 1.95+:

\`\`\`sh
cargo install git-twig --locked
git twig --version
\`\`\`

Or with Homebrew:

\`\`\`sh
brew tap leostera/git-twig https://github.com/leostera/git-twig
brew install leostera/git-twig/git-twig
git twig --version
\`\`\`

On Windows, with Scoop:

\`\`\`powershell
scoop bucket add git-twig https://github.com/leostera/git-twig
scoop install git-twig/git-twig
git twig --version
\`\`\`

Or use cargo install (it needs the Visual Studio C++ build tools), or the
git-twig-v0.6.0-x86_64-pc-windows-msvc.zip attached to the GitHub release: check
it against the release's SHA256SUMS, then put git-twig.exe on PATH. On Windows,
use git twig -h for help; git twig --help looks for HTML docs that do not exist.

Version 0.6.0 is published on crates.io and as GitHub release binaries. Use the
published crate, Homebrew tap, Scoop bucket or release archive; do not invent a curl-to-shell installer.

For an older release, upgrade with the same manager: cargo install git-twig --locked,
brew update followed by brew upgrade leostera/git-twig/git-twig, or scoop update
followed by scoop update git-twig. For an existing
Homebrew HEAD install, use brew upgrade --fetch-HEAD leostera/git-twig/git-twig.
Do not leave multiple versions shadowing each other in PATH. Verify that
git twig --version reports at least 0.6.0 and git twig new --help works.

## Use

Read the bundled skill with git twig skill. Its contents follow.

${skill}
`;

const app = new Hono<{ Bindings: { ASSETS: { fetch: typeof fetch } } }>();
app.get("/llms.txt", (c) => {
  c.header("Cache-Control", "public, max-age=300");
  c.header("X-Content-Type-Options", "nosniff");
  return c.text(onboarding);
});
app.all("*", (c) => c.env.ASSETS.fetch(c.req.raw));
export default app;
