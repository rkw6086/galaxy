import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import * as fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { afterEach, test } from "node:test";
import { fileURLToPath } from "node:url";

const project = fileURLToPath(new URL("../", import.meta.url));
const fixtures = [];

afterEach(async () => {
  for (const root of fixtures.splice(0)) {
    await fs.rm(root, { recursive: true, force: true });
  }
});

async function fixture() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "galaxy-install-preflight-"));
  fixtures.push(root);
  const app = path.join(root, "script/build-and-install-to-applications");
  const lib = path.join(root, "script/lib");
  const bin = path.join(root, "bin");
  await fs.mkdir(path.join(app, "src"), { recursive: true });
  await fs.mkdir(lib, { recursive: true });
  await fs.mkdir(bin);
  await fs.copyFile(path.join(project, "src/index.tsx"), path.join(app, "src/index.tsx"));
  await fs.copyFile(path.join(project, "package.json"), path.join(app, "package.json"));
  await fs.copyFile(path.join(project, "../lib/prepare-galaxy-build.mjs"), path.join(lib, "prepare-galaxy-build.mjs"));
  await fs.copyFile(path.join(project, "../lib/bundled-wormhole-helpers.mjs"), path.join(lib, "bundled-wormhole-helpers.mjs"));
  await fs.symlink(path.join(project, "node_modules"), path.join(app, "node_modules"), "dir");
  // A regression must never progress into a real Git, build, or install operation.
  const gitSentinel = path.join(root, "unexpected-git-command");
  await fs.writeFile(path.join(bin, "git"), "#!/bin/sh\ntouch unexpected-git-command\nexit 97\n", { mode: 0o755 });
  const run = () => {
    const result = spawnSync(process.execPath, [path.join(project, "node_modules/tsx/dist/cli.mjs"), path.join(app, "src/index.tsx")], {
      cwd: root,
      env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH || ""}`, CI: "true", FORCE_COLOR: "0" },
      encoding: "utf8",
      timeout: 15000,
    });
    assert.equal(result.error, undefined);
    assert.equal(result.status, 1);
    assert.match(result.stdout, /Installation failed/);
    return result;
  };
  return { root, lib, run, gitSentinel };
}

test("cache initialization errors are visible before any normal step starts", async () => {
  const { root, run, gitSentinel } = await fixture();
  await fs.mkdir(path.join(root, "target"));
  await fs.writeFile(path.join(root, "target/galaxy-build-cache"), "not a directory");
  const result = run();
  assert.match(result.stderr, /Installation failed:.*EEXIST/);
  assert.match(result.stderr, /galaxy-build-cache/);
  await assert.rejects(fs.access(gitSentinel));
});

test("stale locks report the exact lock path and retain the lock for manual recovery", async () => {
  const { root, run, gitSentinel } = await fixture();
  const lock = path.join(root, "target/galaxy-build-cache/prepare.lock");
  await fs.mkdir(path.dirname(lock), { recursive: true });
  await fs.symlink(JSON.stringify({ pid: 99999999, start: "old process", token: "abandoned" }), lock);
  const result = run();
  assert.match(result.stderr, /Installation failed: Stale Galaxy build lock/);
  assert.ok(result.stderr.includes(lock));
  assert.ok((await fs.lstat(lock)).isSymbolicLink());
  await assert.rejects(fs.access(gitSentinel));
});

test("failures without an active step still appear in the final output", async () => {
  const { lib, run, gitSentinel } = await fixture();
  await fs.writeFile(path.join(lib, "prepare-galaxy-build.mjs"), `
    export async function prepareGalaxyBuild() {
      throw new Error("fixture early failure");
    }
  `);
  const result = run();
  assert.match(result.stderr, /Installation failed: fixture early failure/);
  assert.match(result.stdout, /Installation failed: fixture early failure/);
  await assert.rejects(fs.access(gitSentinel));
});

test("cleanup failures are reported without hiding the original failure", async () => {
  const { root, lib, run, gitSentinel } = await fixture();
  await fs.writeFile(path.join(lib, "prepare-galaxy-build.mjs"), `
    export async function prepareGalaxyBuild(root) {
      return {
        appPath: root + "/missing/Galaxy.app",
        release: async () => { throw new Error("fixture cleanup failure"); },
      };
    }
  `);
  const result = run();
  assert.match(result.stderr, /Installation failed: Built app not found/);
  assert.match(result.stderr, /Failed to remove temporary build files: fixture cleanup failure/);
  assert.match(result.stdout, /Installation failed:\s*Built app not found/);
  await assert.rejects(fs.access(gitSentinel));
  await assert.rejects(fs.access(path.join(root, "target")));
});
