#!/usr/bin/env python3
"""Assemble the git-twig npm package, which carries every platform's binary.

Release:  npm-package.py VERSION --dist DIR --out DIR   (all five archives)
Testing:  npm-package.py VERSION --binary linux-x64=PATH --out DIR
"""
import argparse
import io
import json
import pathlib
import re
import shutil
import tarfile
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCE = ROOT / "npm" / "git-twig"
# npm's process.platform-process.arch, and the release target built for it.
PLATFORMS = {
    "darwin-arm64": "aarch64-apple-darwin",
    "darwin-x64": "x86_64-apple-darwin",
    "linux-arm64": "aarch64-unknown-linux-gnu",
    "linux-x64": "x86_64-unknown-linux-gnu",
    "win32-x64": "x86_64-pc-windows-msvc",
}


def executable(platform: str) -> str:
    return "git-twig.exe" if platform.startswith("win32") else "git-twig"


def from_archive(version: str, platform: str, dist: pathlib.Path) -> bytes:
    target = PLATFORMS[platform]
    name = executable(platform)
    if platform.startswith("win32"):
        with zipfile.ZipFile(dist / f"git-twig-v{version}-{target}.zip") as archive:
            return archive.read(name)
    with tarfile.open(dist / f"git-twig-v{version}-{target}.tar.gz") as archive:
        member = archive.extractfile(f"./{name}")
        if member is None:
            raise SystemExit(f"{name} is not a regular file in the {target} archive")
        return member.read()


def manifest(version: str) -> dict:
    return {
        "name": "git-twig",
        "version": version,
        "description": "Lightweight Git worktrees for agents, using native copy-on-write files",
        "license": "MIT",
        "homepage": "https://twig.leostera.dev",
        "repository": {"type": "git", "url": "git+https://github.com/leostera/git-twig.git"},
        "bugs": "https://github.com/leostera/git-twig/issues",
        "keywords": ["git", "worktree", "copy-on-write", "agents", "cli"],
        "bin": {"git-twig": "bin/git-twig.js"},
        "scripts": {"postinstall": "node install.js"},
        "engines": {"node": ">=18"},
        "os": ["darwin", "linux", "win32"],
        "cpu": ["arm64", "x64"],
        "files": ["bin", "lib", "binaries", "install.js", "README.md", "LICENSE"],
    }


def assemble(version: str, binaries: dict, out: pathlib.Path) -> pathlib.Path:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit("expected a stable semantic version")
    package = out / "git-twig"
    if package.exists():
        raise SystemExit(f"{package} already exists")
    shutil.copytree(SOURCE, package)
    shutil.copy(ROOT / "LICENSE", package / "LICENSE")
    for platform, data in binaries.items():
        path = package / "binaries" / platform / executable(platform)
        path.parent.mkdir(parents=True)
        path.write_bytes(data)
        path.chmod(0o755)
    (package / "package.json").write_text(json.dumps(manifest(version), indent=2) + "\n")
    return package


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version")
    parser.add_argument("--out", type=pathlib.Path, required=True)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--dist", type=pathlib.Path, help="release archives; all platforms required")
    source.add_argument("--binary", action="append", metavar="PLATFORM=PATH", help="a local binary, for tests")
    args = parser.parse_args()
    if args.dist:
        binaries = {platform: from_archive(args.version, platform, args.dist) for platform in PLATFORMS}
    else:
        binaries = {}
        for item in args.binary:
            platform, _, path = item.partition("=")
            if platform not in PLATFORMS or not path:
                raise SystemExit(f"expected PLATFORM=PATH with PLATFORM one of {', '.join(PLATFORMS)}")
            binaries[platform] = pathlib.Path(path).read_bytes()
    print(assemble(args.version, binaries, args.out))


if __name__ == "__main__":
    main()
