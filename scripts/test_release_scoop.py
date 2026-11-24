import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "release_scoop", pathlib.Path(__file__).with_name("release-scoop.py")
)
release_scoop = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_scoop)


class ScoopManifest(unittest.TestCase):
    def test_points_at_the_release_zip_with_its_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            zip_path = pathlib.Path(directory, "git-twig-v1.2.3-x86_64-pc-windows-msvc.zip")
            zip_path.write_bytes(b"zip bytes")
            manifest = json.loads(release_scoop.manifest("1.2.3", pathlib.Path(directory)))
        build = manifest["architecture"]["64bit"]
        self.assertEqual(manifest["version"], "1.2.3")
        self.assertEqual(
            build["url"],
            "https://github.com/leostera/git-twig/releases/download/v1.2.3/git-twig-v1.2.3-x86_64-pc-windows-msvc.zip",
        )
        self.assertEqual(build["hash"], hashlib.sha256(b"zip bytes").hexdigest())
        self.assertEqual(manifest["bin"], "git-twig.exe")

    def test_notes_explain_how_to_onboard_an_agent(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory, "git-twig-v1.2.3-x86_64-pc-windows-msvc.zip").write_bytes(b"zip")
            manifest = json.loads(release_scoop.manifest("1.2.3", pathlib.Path(directory)))
        self.assertIn("git twig skill", " ".join(manifest["notes"]))

    def test_refuses_prereleases_and_missing_zips(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(SystemExit):
                release_scoop.manifest("1.2.3-rc.1", pathlib.Path(directory))
            with self.assertRaises(FileNotFoundError):
                release_scoop.manifest("1.2.3", pathlib.Path(directory))


if __name__ == "__main__":
    unittest.main()
