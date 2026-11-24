"use strict";
// Install the native binary as the git-twig command. If this fails, or install
// scripts are disabled, the Node launcher does it on its first run instead, so
// installation never fails because of this step.
try {
  require("./lib/optimize.js").optimize();
} catch (error) {
  console.warn(`git-twig: using the Node launcher for now (${error.message})`);
}
// npm hides install script output unless --foreground-scripts; the package
// README says the same for everyone else.
console.log("git-twig: to onboard your coding agent, ask it to run `git twig skill` and follow it.");
