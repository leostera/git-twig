#!/usr/bin/env python3
"""Generate a stable Homebrew formula from locally built release archives."""
import hashlib
import pathlib
import re
import sys

version, directory = sys.argv[1], pathlib.Path(sys.argv[2])
if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
    raise SystemExit("expected a stable semantic version")
targets = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
]
stanzas = {}
for target in targets:
    name = f"git-twig-v{version}-{target}.tar.gz"
    checksum = hashlib.sha256((directory / name).read_bytes()).hexdigest()
    stanzas[target] = f'url "https://github.com/leostera/git-twig/releases/download/v{version}/{name}"\n      sha256 "{checksum}"'
print(f'''class GitTwig < Formula
  desc "Lightweight Git worktrees using native copy-on-write files"
  homepage "https://github.com/leostera/git-twig"
  version "{version}"
  license "MIT"
  head "https://github.com/leostera/git-twig.git", branch: "main"

  depends_on "git"
  depends_on "rust" => :build if build.head?

  on_macos do
    on_arm do
      {stanzas["aarch64-apple-darwin"]}
    end
    on_intel do
      {stanzas["x86_64-apple-darwin"]}
    end
  end

  on_linux do
    on_intel do
      {stanzas["x86_64-unknown-linux-gnu"]}
    end
    on_arm do
      {stanzas["aarch64-unknown-linux-gnu"]}
    end
  end

  def install
    if build.head?
      system "cargo", "install", "--locked", "--path", "crates/git-twig", "--root", prefix
      man1.install "crates/git-twig/man/git-twig.1"
    else
      bin.install "git-twig"
      man1.install "git-twig.1"
    end
  end

  def caveats
    <<~EOS
      To onboard your coding agent, ask it to run `git twig skill` and follow it.
      It explains when and how to create worktrees with Twig.
    EOS
  end

  test do
    assert_match "git-twig", shell_output("#{{bin}}/git-twig --version")
  end
end
''')
