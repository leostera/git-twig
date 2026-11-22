"use strict";
// The package carries one binary per supported platform, in binaries/<platform>.
const fs = require("node:fs");
const path = require("node:path");

const platforms = ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64"];

function binaryPath(platform = process.platform, arch = process.arch) {
  const key = `${platform}-${arch}`;
  if (!platforms.includes(key)) {
    throw new Error(
      `git-twig has no prebuilt binary for ${key}. ` +
        "Install from source instead: cargo install git-twig --locked",
    );
  }
  const binary = path.join(
    __dirname,
    "..",
    "binaries",
    key,
    platform === "win32" ? "git-twig.exe" : "git-twig",
  );
  if (!fs.existsSync(binary)) throw new Error(`${binary} is missing; reinstall git-twig`);
  return binary;
}

module.exports = { binaryPath, platforms };
