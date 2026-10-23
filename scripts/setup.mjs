#!/usr/bin/env node
// Gets a fresh checkout running in one step: checks the tools you need,
// installs dependencies, builds the cleanup helper and starts the app.
//
//   npm run setup                 everything, then start the app
//   npm run setup -- --no-start   everything except starting the app
//   npm run setup -- --no-worker  skip the cleanup helper (cleanup then needs a local server or cloud API)

import { execFileSync, spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = new Set(process.argv.slice(2));
const isWindows = process.platform === "win32";

const step = (message) => console.log(`\n\x1b[1m▸ ${message}\x1b[0m`);
const fail = (message) => {
  console.error(`\n\x1b[31msetup: ${message}\x1b[0m`);
  process.exit(1);
};

/** Runs a command in the repo (or `cwd`), stopping setup if it fails. */
function run(command, commandArgs, cwd = root) {
  const result = spawnSync(command, commandArgs, { cwd, stdio: "inherit", shell: isWindows });
  if (result.status !== 0) fail(`\`${command} ${commandArgs.join(" ")}\` failed`);
}

/** Returns a tool's version line, or null if it isn't installed. */
function version(command, flag = "--version") {
  try {
    return execFileSync(command, [flag], { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"], shell: isWindows })
      .split("\n")[0]
      .trim();
  } catch {
    return null;
  }
}

step("Checking tools");
const missing = [];
const nodeMajor = Number(process.versions.node.split(".")[0]);
if (nodeMajor < 22) missing.push(`Node 22 or newer (you have ${process.versions.node}): https://nodejs.org`);
if (!version("cargo")) missing.push("Rust: https://rustup.rs");
if (!version("cmake")) {
  missing.push(
    process.platform === "darwin"
      ? "CMake: brew install cmake"
      : isWindows
        ? "CMake: winget install Kitware.CMake"
        : "CMake and the system libraries in CONTRIBUTING.md (Linux section)",
  );
}
if (missing.length) fail(`install these, then run \`npm run setup\` again:\n  - ${missing.join("\n  - ")}`);
console.log(`node ${process.versions.node}, ${version("cargo")}, ${version("cmake")}`);

step("Installing dependencies");
run("npm", ["ci"]);

if (!args.has("--no-worker")) {
  step("Building the cleanup helper (the first build takes a few minutes)");
  run("cargo", ["build", "--release"], join(root, "cleanup-worker"));
}

if (args.has("--no-start")) {
  console.log("\nAll set. Run `npm run app` to start WordFlow.");
} else {
  step("Starting WordFlow (the first Rust build takes a few minutes)");
  run("npm", ["run", "app"]);
}
