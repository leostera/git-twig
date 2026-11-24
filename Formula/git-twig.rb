class GitTwig < Formula
  desc "Lightweight Git worktrees using native copy-on-write files"
  homepage "https://github.com/leostera/git-twig"
  version "0.6.0"
  license "MIT"
  head "https://github.com/leostera/git-twig.git", branch: "main"

  depends_on "git"
  depends_on "rust" => :build if build.head?

  on_macos do
    on_arm do
      url "https://github.com/leostera/git-twig/releases/download/v0.6.0/git-twig-v0.6.0-aarch64-apple-darwin.tar.gz"
      sha256 "3bc6ea5e9d5c2b3ec1f414e043c54f64bada292dfd1a1ada271375496803637e"
    end
    on_intel do
      url "https://github.com/leostera/git-twig/releases/download/v0.6.0/git-twig-v0.6.0-x86_64-apple-darwin.tar.gz"
      sha256 "ae53b6610b6f712354ddd39b03a292cf7a966e22975453451bb5e03a34fd4743"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/leostera/git-twig/releases/download/v0.6.0/git-twig-v0.6.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "c9fb6aaac9b3e8b33afcd185f0cdf1c0803ab724f716c82c4f7a1925029b8965"
    end
    on_arm do
      url "https://github.com/leostera/git-twig/releases/download/v0.6.0/git-twig-v0.6.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "dd4a6f83755d6cdd1027c21a2dc1e70ac643e114af751523c63252d57dfa9cec"
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
    assert_match "git-twig", shell_output("#{bin}/git-twig --version")
  end
end

