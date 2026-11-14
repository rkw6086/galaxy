import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import * as fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';

import { copyWormholeHelpers, inspectWormholeHelpers, verifyWormholeHelpers } from './bundled-wormhole-helpers.mjs';

const BUNDLE_ARGS = ['bundle', '--release', '--format', 'osx', '--bin', 'galaxy-oss', '--package', 'galaxy', '--features', 'release_bundle,extern_plist'];
const CACHE_VERSION = 2;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const sha = (value) => createHash('sha256').update(value).digest('hex');

async function runCommand(command, args, { cwd, env, onLog, capture = true }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, env, shell: false, stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '';
    const consume = (stream, collect, emit) => {
      let pending = '';
      stream.on('data', (chunk) => {
        const text = chunk.toString();
        if (collect) output += text;
        const lines = (pending + text).split(/\r\n|\r|\n/);
        pending = lines.pop();
        for (const line of lines) if (emit && line.trim()) onLog?.(line);
      });
      stream.on('end', () => { if (emit && pending.trim()) onLog?.(pending); });
    };
    const metadata = command === 'cargo' && args[0] === 'metadata';
    consume(child.stdout, capture, !metadata);
    consume(child.stderr, false, true);
    child.on('error', reject);
    child.on('close', (code) => code === 0 ? resolve(output.trim()) : reject(new Error(`${command} ${args.join(' ')} failed (exit ${code})`)));
  });
}

async function processStart(pid) {
  try { return await runCommand('ps', ['-o', 'lstart=', '-p', String(pid)], { capture: true }); }
  catch { return null; }
}

async function acquireLock(cacheDir) {
  await fs.mkdir(cacheDir, { recursive: true });
  const lock = path.join(cacheDir, 'prepare.lock');
  const start = await processStart(process.pid);
  if (!start) throw new Error('Cannot establish process identity for Galaxy build lock');
  const owner = JSON.stringify({ pid: process.pid, start, token: randomUUID() });
  while (true) {
    try {
      // A symlink publishes the complete owner record in a single atomic operation.
      await fs.symlink(owner, lock);
      return async () => {
        if (await fs.readlink(lock) !== owner) throw new Error('Galaxy build lock ownership changed');
        await fs.unlink(lock);
      };
    } catch (error) { if (error.code !== 'EEXIST') throw error; }
    let other;
    try { other = JSON.parse(await fs.readlink(lock)); }
    catch (error) {
      if (error.code === 'ENOENT') continue;
      throw new Error(`Invalid Galaxy build lock at ${lock}; remove it manually after confirming no build is running`, { cause: error });
    }
    if (!Number.isSafeInteger(other.pid) || other.pid <= 0 || typeof other.start !== 'string') {
      throw new Error(`Invalid Galaxy build lock at ${lock}; remove it manually after confirming no build is running`);
    }
    const current = await processStart(other.pid);
    if (current === other.start) { await sleep(1000); continue; }
    if (!current) {
      try { process.kill(other.pid, 0); await sleep(1000); continue; }
      catch (error) { if (error.code !== 'ESRCH') throw error; }
    }
    // Never reclaim a lock automatically: another waiter could race a new owner.
    throw new Error(`Stale Galaxy build lock at ${lock}; remove it manually after confirming no build is running`);
  }
}

async function optionalHash(file) {
  try { return sha(await fs.readFile(file)); }
  catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

async function cargoConfigs(root, env, home) {
  const result = {};
  let dir = path.resolve(root);
  while (true) {
    for (const name of ['config', 'config.toml']) {
      const file = path.join(dir, '.cargo', name);
      result[file] = await optionalHash(file);
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
    dir = parent;
  }
  const cargoHome = env.CARGO_HOME || path.join(home, '.cargo');
  for (const name of ['config', 'config.toml']) {
    const file = path.resolve(cargoHome, name);
    result[file] = await optionalHash(file);
  }
  return result;
}

// These two UI packages contribute only their own launch shims, not Cargo build inputs.
// Strip their bin directories from both the key and subprocess environment.
function buildEnvironment(root, env) {
  const bins = new Set(['build-and-install-to-applications', 'build-and-deploy-hermes'].map((name) => path.join(root, 'script', name, 'node_modules', '.bin')));
  return { ...env, PATH: (env.PATH || '').split(path.delimiter).filter((entry) => !bins.has(path.resolve(entry)) && !(entry.startsWith('/var/folders/') && /^yarn--[0-9]+-[0-9]+\.[0-9]+$/.test(path.basename(entry)))).join(path.delimiter) };
}

// Hash build-affecting values without persisting secrets. Yarn/npm package metadata,
// lifecycle context, and upload credentials differ between the two entrypoints.
function environmentDigest(env) {
  const ignored = new Set(['PWD', 'OLDPWD', 'SHLVL', '_', 'TERM', 'TERM_PROGRAM', 'TERM_PROGRAM_VERSION', 'INIT_CWD', 'HERMES_PASS', 'HERMES_USER', 'npm_config_argv', 'npm_lifecycle_event', 'npm_lifecycle_script', 'npm_execpath', 'npm_node_execpath']);
  return sha(JSON.stringify(Object.entries(env).filter(([key]) => !ignored.has(key) && !key.startsWith('npm_package_')).sort(([a], [b]) => a.localeCompare(b))));
}


async function tree(root) {
  const entries = [];
  async function visit(relative) {
    const current = path.join(root, relative);
    const stat = await fs.lstat(current);
    const base = { path: relative, mode: stat.mode & 0o7777 };
    if (stat.isSymbolicLink()) entries.push({ ...base, type: 'link', target: await fs.readlink(current) });
    else if (stat.isFile()) entries.push({ ...base, type: 'file', size: stat.size, hash: sha(await fs.readFile(current)) });
    else if (stat.isDirectory()) {
      entries.push({ ...base, type: 'dir' });
      for (const name of (await fs.readdir(current)).sort()) await visit(path.join(relative, name));
    } else throw new Error(`Unsupported bundle entry: ${current}`);
  }
  await visit('');
  return sha(JSON.stringify(entries));
}

async function validateBuiltApp(app) {
  try {
    const directory = await fs.lstat(app);
    if (!directory.isDirectory() || directory.isSymbolicLink()) throw new Error('app path is not a regular directory');
    for (const relative of ['Contents/Info.plist', 'Contents/MacOS/galaxy-oss']) {
      const entry = await fs.lstat(path.join(app, relative));
      if (!entry.isFile() || entry.isSymbolicLink() || entry.size === 0) throw new Error(`${relative} is missing or not a nonempty regular file`);
      if (relative.endsWith('/galaxy-oss') && (entry.mode & 0o111) === 0) throw new Error('Galaxy binary is not executable');
    }
  } catch (error) {
    throw new Error(`cargo-bundle did not produce a complete Galaxy.app at ${app}: ${error.message}`, { cause: error });
  }
}

async function identity(root, env, run, home, platform, arch, bundleArgs) {
  const cmd = (name, args) => run(name, args, { cwd: root, env });
  const [commit, rustc, cargo, bundle, clang, sdkPath, sdkVersion, sdkBuild, developerDir, ignoredResources, configs] = await Promise.all([
    cmd('git', ['rev-parse', '--verify', 'HEAD']),
    cmd('rustc', ['--version', '--verbose']),
    cmd('cargo', ['--version', '--verbose']),
    cmd('cargo', ['bundle', '--version']),
    cmd('clang', ['--version']),
    cmd('xcrun', ['--show-sdk-path']),
    cmd('xcrun', ['--show-sdk-version']),
    cmd('xcrun', ['--show-sdk-build-version']),
    cmd('xcode-select', ['-p']),
    cmd('git', ['ls-files', '--others', '--ignored', '--exclude-standard', '--', 'app/assets/onboarding', 'app/channels/oss/icon/no-padding']),
    cargoConfigs(root, env, home),
  ]);
  // Overrides and generated resource caches may refer to mutable inputs outside Git.
  // A build using these inputs is safe to perform, but is not reusable.
  const external = Object.keys(env).some((key) => /^(GALAXY_WORMHOLE_HELPERS_DIR|SETTINGS_SCHEMA_CACHE|CARGO_TARGET_DIR|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CC|CXX|AR|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|GIT_RELEASE_TAG|GALAXY_GIT_COMMIT|GALAXY_GIT_DIRTY|GITHUB_SHA|CI_COMMIT_SHA|SDKROOT|DEVELOPER_DIR)$/.test(key));
  const externalConfig = Object.entries(configs).some(([file, hash]) => hash && !file.startsWith(path.join(root, '.cargo') + path.sep));
  return { commit: commit.trim(), rustc, cargo, bundle, clang, sdkPath, sdkVersion, sdkBuild, developerDir, platform, arch, command: bundleArgs, environment: environmentDigest(env), configs, external: external || externalConfig || Boolean(ignoredResources.trim()) };
}

/**
 * Prepare a release bundle. observer receives (stepIndex, {status, detail?}) and
 * log(stepIndex, line), or may be { updateStep, appendLog } as in the Ink apps.
 * Returns { appPath, reused, identity, release }. appPath is a private copy:
 * callers may sign or install it, and must call release() in a finally block.
 * options.run is for standalone tests; normal callers should omit options.
 */
export async function prepareGalaxyBuild(workspaceRoot, observer = {}, options = {}) {
  const root = path.resolve(workspaceRoot);
  const cache = path.join(root, 'target', 'galaxy-build-cache');
  const run = options.run || runCommand;
  const env = buildEnvironment(root, options.env || process.env);
  const update = typeof observer === 'function' ? observer : observer.updateStep?.bind(observer) || (() => {});
  const log = typeof observer === 'function' ? (() => {}) : observer.appendLog?.bind(observer) || observer.log?.bind(observer) || (() => {});
  const command = (name, args, step) => run(name, args, { cwd: root, env, capture: !(name === 'cargo' && args[0] === 'bundle' && args.includes('--release')), onLog: (line) => log(step, line) });
  update(0, { status: 'running', detail: 'Acquiring build lock' });
  const unlock = await acquireLock(cache);
  try {
    update(0, { status: 'running', detail: undefined });
    const changes = await command('git', ['status', '--porcelain', '--untracked-files=all'], 0);
    if (changes.trim()) {
      for (const line of changes.split('\n')) log(0, line);
      throw new Error('Working directory has uncommitted changes. Commit or stash them before building.');
    }
    update(0, { status: 'done' });
    update(1, { status: 'running' });
    const branch = (await command('git', ['symbolic-ref', '--quiet', '--short', 'HEAD'], 1)).trim();
    if (!branch) throw new Error('Cannot build from a detached HEAD');
    await command('git', ['pull', '--ff-only', 'origin', branch], 1);
    if ((await command('git', ['status', '--porcelain', '--untracked-files=all'], 1)).trim()) throw new Error('Working directory changed after pull');
    const pulledCommit = (await command('git', ['rev-parse', '--verify', 'HEAD'], 1)).trim();
    if (!/^[0-9a-f]{40,64}$/i.test(pulledCommit)) throw new Error('Cannot verify pulled Git commit');
    update(1, { status: 'done', detail: branch });

    update(2, { status: 'running' });
    const metadata = JSON.parse(await command('cargo', ['metadata', '--format-version', '1', '--no-deps'], 2));
    if (!path.isAbsolute(metadata.target_directory)) throw new Error('Cargo metadata did not supply an absolute target directory');
    const targetDir = metadata.target_directory;
    // Explicit --target takes precedence over Cargo build.target and CARGO_BUILD_TARGET.
    const rustcVersion = await command('rustc', ['--version', '--verbose'], 2);
    const host = rustcVersion.match(/^host:\s*(\S+)\s*$/m)?.[1];
    if (!host || !/^[a-zA-Z0-9_-]+-apple-darwin$/.test(host)) throw new Error('Cannot determine native macOS Rust host');
    const bundleArgs = [...BUNDLE_ARGS, '--target', host];
    // cargo-bundle only copies its Cargo.toml resource list. Wormhole's
    // prebuilt Linux helper archives must be staged explicitly for both the
    // install and deploy entrypoints; neither command rebuilds those helpers.
    const helperSource = path.resolve(root, env.GALAXY_WORMHOLE_HELPERS_DIR || 'resources/wormhole-helpers');
    const helpers = await inspectWormholeHelpers(helperSource);
    const bundledHelpers = (app) => path.join(app, 'Contents', 'Resources', 'wormhole-helpers');
    // Unknown tool versions fail closed for reuse, while still permitting a normal build.
    let key;
    try { key = await identity(root, env, run, options.home || os.homedir(), options.platform || process.platform, options.arch || process.arch, bundleArgs); }
    catch (error) { log(2, `Cannot verify cache identity: ${error.message}`); }
    const snapshot = path.join(cache, 'Galaxy.app');
    const manifestPath = path.join(cache, 'manifest.json');
    let reused = false;
    if (key && key.commit !== pulledCommit) key = undefined;
    if (key && !key.external) {
      key.targetDir = targetDir;
      try {
        const manifest = JSON.parse(await fs.readFile(manifestPath, 'utf8'));
        reused = manifest.version === CACHE_VERSION && JSON.stringify(manifest.identity) === JSON.stringify(key) && manifest.tree === await tree(snapshot);
        if (reused) await verifyWormholeHelpers(helpers, bundledHelpers(snapshot));
      } catch { reused = false; /* Missing or damaged cache is a miss. */ }
    }
    if (!reused) {
      const builtApp = path.join(targetDir, host, 'release', 'bundle', 'osx', 'Galaxy.app');
      // Prevent cargo-bundle from leaving us with an old app after an incomplete build.
      await fs.rm(builtApp, { recursive: true, force: true });
      await command('cargo', bundleArgs, 2);
      // Resource staging must never manufacture an app after a successful
      // command that failed to produce the actual bundle.
      await validateBuiltApp(builtApp);
      await copyWormholeHelpers(helpers, bundledHelpers(builtApp));
      const digest = await tree(builtApp);
      if ((await command('git', ['status', '--porcelain', '--untracked-files=all'], 2)).trim() ||
          (await command('git', ['rev-parse', '--verify', 'HEAD'], 2)).trim() !== pulledCommit) {
        throw new Error('Checkout changed during bundle preparation');
      }
      const staged = path.join(cache, `stage-${randomUUID()}`);
      try {
        await fs.cp(builtApp, staged, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true, force: false });
        if (await tree(staged) !== digest) throw new Error('Bundled app changed while caching');
        await fs.rm(manifestPath, { force: true });
        await fs.rm(snapshot, { recursive: true, force: true });
        await fs.rename(staged, snapshot);
        if (key && !key.external) {
          const tempManifest = path.join(cache, `manifest-${randomUUID()}.json`);
          await fs.writeFile(tempManifest, JSON.stringify({ version: CACHE_VERSION, identity: key, tree: digest }));
          await fs.rename(tempManifest, manifestPath);
        }
      } finally { await fs.rm(staged, { recursive: true, force: true }); }
    }
    const leaseDir = path.join(cache, `lease-${randomUUID()}`);
    const appPath = path.join(leaseDir, 'Galaxy.app');
    try {
      await fs.mkdir(leaseDir);
      await fs.cp(snapshot, appPath, { recursive: true, preserveTimestamps: true, verbatimSymlinks: true, force: false });
      if (await tree(appPath) !== await tree(snapshot)) throw new Error('Bundled app changed while copying');
      await verifyWormholeHelpers(helpers, bundledHelpers(appPath));
      if ((await command('git', ['status', '--porcelain', '--untracked-files=all'], 2)).trim() ||
          (await command('git', ['rev-parse', '--verify', 'HEAD'], 2)).trim() !== pulledCommit) {
        throw new Error('Checkout changed during bundle preparation');
      }
    } catch (error) { await fs.rm(leaseDir, { recursive: true, force: true }); throw error; }
    update(2, { status: 'done', detail: reused ? 'Reused verified bundle' : 'Built release bundle' });
    return { appPath, reused, identity: key, release: async () => fs.rm(leaseDir, { recursive: true, force: true }) };
  } finally { await unlock(); }
}
