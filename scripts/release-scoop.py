#!/usr/bin/env python3
"""Generate the Scoop manifest (bucket/git-twig.json) from the Windows release zip."""
import hashlib
import json
import pathlib
import re
import sys


def manifest(version: str, directory: pathlib.Path) -> str:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise SystemExit("expected a stable semantic version")
    name = f"git-twig-v{version}-x86_64-pc-windows-msvc.zip"
    checksum = hashlib.sha256((directory / name).read_bytes()).hexdigest()
    return json.dumps(
        {
            "version": version,
            "description": "Lightweight Git worktrees using native copy-on-write files",
            "homepage": "https://github.com/leostera/git-twig",
            "license": "MIT",
            "notes": [
                "To onboard your coding agent, ask it to run `git twig skill` and follow it.",
                "Needs Git 2.40+ on PATH. Native cloning needs a ReFS volume or Dev Drive; elsewhere Twig uses stock Git.",
            ],
            "architecture": {
                "64bit": {
                    "url": f"https://github.com/leostera/git-twig/releases/download/v{version}/{name}",
                    "hash": checksum,
                }
            },
            "bin": "git-twig.exe",
        },
        indent=4,
    ) + "\n"


if __name__ == "__main__":
    print(manifest(sys.argv[1], pathlib.Path(sys.argv[2])), end="")
