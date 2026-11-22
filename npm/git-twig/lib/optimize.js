"use strict";
// Replace the Node launcher with the native binary, so later runs never start
// Node. install.js does this at install time; the launcher does it on its first
// run, for installs that skip scripts. Nothing is downloaded.
const fs = require("node:fs");
const path = require("node:path");
const { execFileSync } = require("node:child_process");
const { binaryPath } = require("./platform.js");

const launcher = path.join(__dirname, "..", "bin", "git-twig.js");

function optimize() {
  // npm's Windows launchers always invoke the script through Node.
  if (process.platform === "win32") return false;
  const binary = binaryPath();
  fs.chmodSync(binary, 0o755);
  const { version } = require("../package.json");
  const reported = execFileSync(binary, ["--version"], { encoding: "utf8" }).trim();
  if (reported !== `git-twig ${version}`) {
    throw new Error(`${binary} reports "${reported}", expected git-twig ${version}`);
  }
  const temporary = `${launcher}.${process.pid}.tmp`;
  try {
    // Copy rather than link: package managers that share files between
    // projects must not see this one change.
    fs.copyFileSync(binary, temporary);
    fs.chmodSync(temporary, 0o755);
    // A rename replaces the directory entry; concurrent runs each rename a
    // complete copy, and the old launcher file itself is never written.
    fs.renameSync(temporary, launcher);
  } finally {
    fs.rmSync(temporary, { force: true });
  }
  return true;
}

module.exports = { optimize };
