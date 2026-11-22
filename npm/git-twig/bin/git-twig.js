#!/usr/bin/env node
"use strict";
// Runs the platform's binary. On macOS and Linux it first replaces itself with
// that binary, so only the first run starts Node; on Windows, npm's launcher
// always runs this file through Node.
const fs = require("node:fs");
const { spawnSync } = require("node:child_process");
const { binaryPath } = require("../lib/platform.js");

let binary;
try {
  binary = binaryPath();
} catch (error) {
  console.error(`git-twig: ${error.message}`);
  process.exit(1);
}
try {
  require("../lib/optimize.js").optimize();
} catch {
  // A read-only install keeps this launcher; it still works.
}
function run() {
  return spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
}
let result = run();
if (result.error?.code === "EACCES" && process.platform !== "win32") {
  fs.chmodSync(binary, 0o755);
  result = run();
}
if (result.error) {
  console.error(`git-twig: cannot run ${binary}: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) process.kill(process.pid, result.signal);
process.exit(result.status ?? 1);
