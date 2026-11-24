import importlib.util
import io
import shutil
import subprocess
import json
import pathlib
import stat
import tarfile
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("npm_package", pathlib.Path(__file__).with_name("npm-package.py"))
npm_package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(npm_package)


def release_archives(directory: pathlib.Path, version: str):
    for platform, target in npm_package.PLATFORMS.items():
        data = f"{platform} binary".encode()
        if platform.startswith("win32"):
            with zipfile.ZipFile(directory / f"git-twig-v{version}-{target}.zip", "w") as archive:
                archive.writestr("git-twig.exe", data)
                archive.writestr("README.md", "readme")
        else:
            with tarfile.open(directory / f"git-twig-v{version}-{target}.tar.gz", "w:gz") as archive:
                for name, content in (("./git-twig", data), ("./README.md", b"readme")):
                    info = tarfile.TarInfo(name)
                    info.size = len(content)
                    archive.addfile(info, io.BytesIO(content))


class NpmPackage(unittest.TestCase):
    def test_carries_every_platform_binary_from_the_release_archives(self):
        with tempfile.TemporaryDirectory() as directory:
            dist, out = pathlib.Path(directory, "dist"), pathlib.Path(directory, "out")
            dist.mkdir()
            release_archives(dist, "1.2.3")
            binaries = {p: npm_package.from_archive("1.2.3", p, dist) for p in npm_package.PLATFORMS}
            package = npm_package.assemble("1.2.3", binaries, out)
            for platform in npm_package.PLATFORMS:
                binary = package / "binaries" / platform / npm_package.executable(platform)
                self.assertEqual(binary.read_bytes(), f"{platform} binary".encode())
                self.assertTrue(binary.stat().st_mode & stat.S_IXUSR)
            manifest = json.loads((package / "package.json").read_text())
            self.assertEqual(manifest["name"], "git-twig")
            self.assertEqual(manifest["version"], "1.2.3")
            self.assertEqual(manifest["bin"], {"git-twig": "bin/git-twig.js"})
            self.assertNotIn("dependencies", manifest)
            self.assertNotIn("optionalDependencies", manifest)
            for file in ("bin/git-twig.js", "lib/platform.js", "lib/optimize.js", "install.js", "README.md", "LICENSE"):
                self.assertTrue((package / file).is_file(), file)

    def test_platform_lists_agree_with_the_launcher(self):
        source = (npm_package.SOURCE / "lib" / "platform.js").read_text()
        for platform in npm_package.PLATFORMS:
            self.assertIn(f'"{platform}"', source)

    @unittest.skipUnless(shutil.which("node"), "needs Node")
    def test_install_says_how_to_onboard_an_agent_even_when_optimizing_fails(self):
        source = pathlib.Path(__file__).parents[1] / "npm" / "git-twig"
        with tempfile.TemporaryDirectory() as directory:
            package = pathlib.Path(directory, "git-twig")
            shutil.copytree(source, package)  # no binaries: optimizing fails
            result = subprocess.run(["node", "install.js"], cwd=package, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("git twig skill", result.stdout + result.stderr)

    def test_a_missing_release_archive_is_an_error(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(FileNotFoundError):
                npm_package.from_archive("1.2.3", "linux-arm64", pathlib.Path(directory))

    def test_refuses_prereleases_and_reused_output(self):
        with tempfile.TemporaryDirectory() as directory:
            out = pathlib.Path(directory)
            with self.assertRaises(SystemExit):
                npm_package.assemble("1.2.3-rc.1", {}, out)
            npm_package.assemble("1.2.3", {}, out)
            with self.assertRaises(SystemExit):
                npm_package.assemble("1.2.3", {}, out)


if __name__ == "__main__":
    unittest.main()
