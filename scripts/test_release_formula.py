import hashlib
import pathlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("release-formula.py")
TARGETS = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
]


def formula(directory: pathlib.Path) -> str:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "1.2.3", str(directory)],
        check=True, capture_output=True, text=True,
    ).stdout


class HomebrewFormula(unittest.TestCase):
    def test_every_release_archive_has_a_stanza_with_its_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            for target in TARGETS:
                pathlib.Path(directory, f"git-twig-v1.2.3-{target}.tar.gz").write_bytes(target.encode())
            text = formula(pathlib.Path(directory))
        for target in TARGETS:
            self.assertIn(f"releases/download/v1.2.3/git-twig-v1.2.3-{target}.tar.gz", text)
            self.assertIn(hashlib.sha256(target.encode()).hexdigest(), text)
        linux = text[text.index("on_linux do"):]
        self.assertLess(linux.index("on_intel do"), linux.index("x86_64-unknown-linux-gnu"))
        self.assertLess(linux.index("on_arm do"), linux.index("aarch64-unknown-linux-gnu"))

    def test_caveats_explain_how_to_onboard_an_agent(self):
        with tempfile.TemporaryDirectory() as directory:
            for target in TARGETS:
                pathlib.Path(directory, f"git-twig-v1.2.3-{target}.tar.gz").write_bytes(b"x")
            text = formula(pathlib.Path(directory))
        self.assertIn("def caveats", text)
        self.assertIn("git twig skill", text)


if __name__ == "__main__":
    unittest.main()
