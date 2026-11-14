import assert from 'node:assert/strict';
import { afterEach, test } from 'node:test';
import * as fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { gzipSync } from 'node:zlib';
import { prepareGalaxyBuild } from './prepare-galaxy-build.mjs';
import { copyWormholeHelpers, inspectWormholeHelpers, verifyWormholeHelpers } from './bundled-wormhole-helpers.mjs';

const roots = [];
afterEach(async () => {
  for (const root of roots.splice(0)) await fs.rm(root, { recursive: true, force: true });
});

async function fixture() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'galaxy-prep-test-'));
  roots.push(root);
  for (const architecture of ['linux-x86_64', 'linux-aarch64']) {
    const helpers = path.join(root, 'resources/wormhole-helpers', architecture);
    await fs.mkdir(helpers, { recursive: true });
    await fs.writeFile(path.join(helpers, 'galaxy-wormhole.tar.gz'), gzipSync(`prebuilt ${architecture}`));
    await fs.writeFile(path.join(helpers, 'galaxy-wormhole.version'), `${'a'.repeat(64)}\n`);
  }
  const state = { commit: 'a'.repeat(40), branch: 'main', status: '', rustc: 'rustc 1\nhost: aarch64-apple-darwin', cargo: 'cargo 1', bundle: 'cargo-bundle 1', clang: 'clang 1', sdkPath: '/sdk/one', sdkVersion: '14.0', sdkBuild: '22A', developerDir: '/Library/Developer/CommandLineTools', ignoredResources: '', builds: 0, fail: false, commands: [] };
  const stateTarget = path.join(root, 'target');
  const app = () => path.join(state.targetDir, 'aarch64-apple-darwin/release/bundle/osx/Galaxy.app');
  state.targetDir = stateTarget;
  const run = async (name, args, { onLog, env } = {}) => {
    state.commands.push([name, ...args]);
    if (env && name === 'cargo' && args[0] === 'bundle' && args.includes('--release')) state.buildPath = env.PATH;
    const key = [name, ...args].join(' ');
    if (key === 'git status --porcelain --untracked-files=all') return state.status;
    if (key === 'git symbolic-ref --quiet --short HEAD') {
      if (!state.branch) throw new Error('detached HEAD');
      return state.branch;
    }
    if (key.startsWith('git pull --ff-only origin ')) { onLog?.('Already up to date.'); return ''; }
    if (key === 'git rev-parse --verify HEAD') return state.commit;
    if (key === 'rustc --version --verbose') return state.rustc;
    if (key === 'cargo --version --verbose') return state.cargo;
    if (key === 'cargo bundle --version') return state.bundle;
    if (key === 'cargo metadata --format-version 1 --no-deps') return JSON.stringify({ target_directory: state.targetDir });
    if (key === 'clang --version') return state.clang;
    if (key === 'xcrun --show-sdk-path') { if (state.failSdk) throw new Error('SDK probe failed'); return state.sdkPath; }
    if (key === 'xcrun --show-sdk-version') return state.sdkVersion;
    if (key === 'xcrun --show-sdk-build-version') return state.sdkBuild;
    if (key === 'xcode-select -p') return state.developerDir;
    if (key.startsWith('git ls-files --others --ignored --exclude-standard --')) return state.ignoredResources;
    if (key.startsWith('cargo bundle --release ')) {
      state.builds++;
      if (state.changeCommitOnBuild) state.commit = 'c'.repeat(40);
      if (state.fail) throw new Error('bundle failed');
      if (state.noBundleOutput) return '';
      await fs.mkdir(path.join(app(), 'Contents/MacOS'), { recursive: true });
      await fs.writeFile(path.join(app(), 'Contents/Info.plist'), '<plist><dict/></plist>');
      await fs.writeFile(path.join(app(), 'Contents/MacOS/galaxy-oss'), `built ${state.builds}`);
      await fs.chmod(path.join(app(), 'Contents/MacOS/galaxy-oss'), 0o755);
      await fs.symlink('MacOS/galaxy-oss', path.join(app(), 'Contents/link'));
      return '';
    }
    throw new Error(`Unexpected command: ${key}`);
  };
  const options = { run, env: { PATH: '/usr/bin' }, home: path.join(root, 'home'), platform: 'darwin', arch: 'arm64' };
  const events = [];
  const observer = { updateStep: (index, event) => events.push([index, event]), appendLog: (index, line) => events.push([index, line]) };
  async function prepare() { return prepareGalaxyBuild(root, observer, options); }
  return { root, state, get app() { return app(); }, options, observer, events, prepare };
}

async function withRelease(result) {
  const text = await fs.readFile(path.join(result.appPath, 'Contents/MacOS/galaxy-oss'), 'utf8');
  await result.release();
  await assert.rejects(fs.access(result.appPath));
  return text;
}

test('first run distrusts an existing bundle; exact identity reuses verified snapshot, not signed output', async () => {
  const f = await fixture();
  await fs.mkdir(path.join(f.app, 'Contents/MacOS'), { recursive: true });
  await fs.writeFile(path.join(f.app, 'Contents/MacOS/galaxy-oss'), 'old');
  const first = await f.prepare();
  assert.equal(first.reused, false);
  assert.equal(await withRelease(first), 'built 1');
  await fs.writeFile(path.join(f.app, 'Contents/MacOS/galaxy-oss'), 'signed and mutated');
  const second = await f.prepare();
  assert.equal(second.reused, true);
  assert.equal(await withRelease(second), 'built 1');
  assert.equal(f.state.builds, 1);
  assert.deepEqual(f.state.commands.find((cmd) => cmd[0] === 'cargo' && cmd[1] === 'bundle' && cmd.includes('--release')), ['cargo', 'bundle', '--release', '--format', 'osx', '--bin', 'galaxy-oss', '--package', 'galaxy', '--features', 'release_bundle,extern_plist', '--target', 'aarch64-apple-darwin']);
  assert.deepEqual(f.events.filter(([index, event]) => index === 2 && event.status === 'done').map(([, event]) => event.detail), ['Built release bundle', 'Reused verified bundle']);
});

test('commit, environment, config and toolchain changes each invalidate reuse', async () => {
  const f = await fixture();
  await withRelease(await f.prepare());
  const mutations = [
    () => { f.state.commit = 'b'.repeat(40); },
    () => { f.options.env.BUILD_PROFILE = 'other'; },
    async () => { await fs.mkdir(path.join(f.root, '.cargo')); await fs.writeFile(path.join(f.root, '.cargo/config.toml'), '[env]\nEXAMPLE = "1"\n'); },
    () => { f.state.rustc = 'rustc 2\nhost: aarch64-apple-darwin'; },
    () => { f.state.cargo = 'cargo 2'; },
    () => { f.state.bundle = 'cargo-bundle 2'; },
    () => { f.state.sdkVersion = '15.0'; },
    () => { f.state.sdkBuild = '23B'; },
    () => { f.state.developerDir = '/Applications/Xcode.app/Contents/Developer'; },
    () => { f.options.arch = 'x64'; },
  ];
  for (const mutate of mutations) {
    await mutate();
    const result = await f.prepare();
    assert.equal(result.reused, false);
    await result.release();
  }
  assert.equal(f.state.builds, mutations.length + 1);
});

test('missing, changed, or extra snapshot entries force a rebuild', async () => {
  const f = await fixture();
  await withRelease(await f.prepare());
  const cached = path.join(f.root, 'target/galaxy-build-cache/Galaxy.app/Contents');
  for (const mutation of [
    () => fs.rm(path.join(cached, 'MacOS/galaxy-oss')),
    () => fs.writeFile(path.join(cached, 'MacOS/galaxy-oss'), 'tampered'),
    () => fs.writeFile(path.join(cached, 'extra'), 'added'),
  ]) {
    await mutation();
    const result = await f.prepare();
    assert.equal(result.reused, false);
    await result.release();
  }
  assert.equal(f.state.builds, 4);
});

test('failed build does not record a successful cache identity or trust stale output', async () => {
  const f = await fixture();
  f.state.fail = true;
  await assert.rejects(f.prepare(), /bundle failed/);
  await assert.rejects(fs.access(path.join(f.root, 'target/galaxy-build-cache/manifest.json')));
  f.state.fail = false;
  const result = await f.prepare();
  assert.equal(result.reused, false);
  await result.release();
  assert.equal(f.state.builds, 2);
});

test('dirty and detached checkouts abort before pulling or bundling', async () => {
  const f = await fixture();
  f.state.status = '?? change';
  await assert.rejects(f.prepare(), /uncommitted changes/);
  assert.equal(f.state.commands.some((cmd) => cmd[1] === 'pull'), false);
  f.state.status = '';
  f.state.branch = '';
  await assert.rejects(f.prepare(), /detached HEAD/);
  assert.equal(f.state.builds, 0);
});

test('external config and mutable overrides conservatively disable reuse', async () => {
  const f = await fixture();
  await fs.mkdir(path.join(f.options.home, '.cargo'), { recursive: true });
  await fs.writeFile(path.join(f.options.home, '.cargo/config.toml'), '[build]\nrustflags = []\n');
  await withRelease(await f.prepare());
  const second = await f.prepare();
  assert.equal(second.reused, false);
  await second.release();
  f.options.env.GALAXY_WORMHOLE_HELPERS_DIR = path.join(f.root, 'resources/wormhole-helpers');
  const third = await f.prepare();
  assert.equal(third.reused, false);
  await third.release();
});

test('install and deploy Yarn metadata and upload credentials share the cache', async () => {
  const f = await fixture();
  const installerBin = path.join(f.root, 'script/build-and-install-to-applications/node_modules/.bin');
  const deployBin = path.join(f.root, 'script/build-and-deploy-hermes/node_modules/.bin');
  const yarnOne = '/var/folders/ab/cdef/T/yarn--1772886695123-0.1948674944443123';
  const yarnTwo = '/var/folders/ab/cdef/T/yarn--1772886702989-0.9834001432421421';
  Object.assign(f.options.env, { PATH: `${yarnOne}${path.delimiter}${installerBin}${path.delimiter}/usr/bin`, npm_package_name: 'build-and-install-to-applications', npm_package_scripts_start: 'tsx src/index.tsx', npm_lifecycle_script: 'tsx src/index.tsx', npm_lifecycle_event: 'start', INIT_CWD: '/install', npm_config_argv: '{\"original\":[\"install\"]}' });
  await withRelease(await f.prepare());
  assert.equal(f.state.buildPath, '/usr/bin');
  Object.assign(f.options.env, { PATH: `${yarnTwo}${path.delimiter}${deployBin}${path.delimiter}/usr/bin`, npm_package_name: 'build-and-deploy-hermes', npm_package_scripts_start: 'tsx src/index.tsx', INIT_CWD: '/deploy', npm_config_argv: '{\"original\":[\"deploy\"]}', HERMES_PASS: 'secret', HERMES_USER: 'tester' });
  const result = await f.prepare();
  assert.equal(result.reused, true);
  await result.release();
  assert.equal(f.state.builds, 1);
  f.options.env.PATH = `${yarnTwo}${path.delimiter}${deployBin}${path.delimiter}/usr/local/custom-tools${path.delimiter}/usr/bin`;
  const changedTools = await f.prepare();
  assert.equal(changedTools.reused, false);
  assert.equal(f.state.buildPath, '/usr/local/custom-tools:/usr/bin');
  await changedTools.release();
});

test('spawned Cargo metadata excludes warning stderr and hides JSON stdout from logs', async () => {
  const f = await fixture();
  const bin = path.join(f.root, 'fake-bin');
  await fs.mkdir(bin);
  const program = `#!/usr/bin/env node
import * as fs from 'node:fs';
import path from 'node:path';
const command = path.basename(process.argv[1]);
const args = process.argv.slice(2);
if (command === 'git') {
  if (args[0] === 'symbolic-ref') console.log('main');
  if (args[0] === 'rev-parse') console.log('a'.repeat(40));
} else if (command === 'cargo' && args[0] === 'metadata') {
  console.error('warning: metadata stderr');
  console.log(JSON.stringify({ target_directory: path.join(process.cwd(), 'target') }));
} else if (command === 'cargo' && args[0] === 'bundle' && args.includes('--release')) {
  const app = path.join(process.cwd(), 'target/aarch64-apple-darwin/release/bundle/osx/Galaxy.app');
  fs.mkdirSync(path.join(app, 'Contents/MacOS'), { recursive: true });
  fs.writeFileSync(path.join(app, 'Contents/Info.plist'), '<plist><dict/></plist>');
  fs.writeFileSync(path.join(app, 'Contents/MacOS/galaxy-oss'), 'fake', { mode: 0o755 });
} else if (command === 'rustc') console.log('rustc 1\\nhost: aarch64-apple-darwin');
else console.log('fixed version');
`;
  for (const name of ['git', 'cargo', 'rustc', 'clang', 'xcrun', 'xcode-select']) {
    const executable = path.join(bin, name);
    await fs.writeFile(executable, program);
    await fs.chmod(executable, 0o755);
  }
  const events = [];
  const observer = { updateStep: () => {}, appendLog: (_, line) => events.push(line) };
  const result = await prepareGalaxyBuild(f.root, observer, { env: { PATH: `${bin}${path.delimiter}${process.env.PATH}` }, home: f.options.home });
  assert.equal(result.reused, false);
  assert.ok(events.includes('warning: metadata stderr'));
  assert.equal(events.some((line) => line.includes('target_directory')), false);
  await result.release();
});

test('Cargo metadata target directory controls bundle path and cache identity', async () => {
  const f = await fixture();
  await withRelease(await f.prepare());
  f.state.targetDir = path.join(f.root, 'custom-target');
  const result = await f.prepare();
  assert.equal(result.reused, false);
  assert.equal(await withRelease(result), 'built 2');
  assert.equal(f.state.builds, 2);
});

test('Cargo target configuration and environment are overridden with native host', async () => {
  const f = await fixture();
  await fs.mkdir(path.join(f.root, '.cargo'));
  await fs.writeFile(path.join(f.root, '.cargo/config.toml'), 'build.target = "x86_64-apple-darwin"\n');
  f.options.env.CARGO_BUILD_TARGET = 'x86_64-apple-darwin';
  const result = await f.prepare();
  assert.equal(result.reused, false);
  assert.ok(f.state.commands.some((args) => args[0] === 'cargo' && args.includes('--release') && args.at(-2) === '--target' && args.at(-1) === 'aarch64-apple-darwin'));
  await result.release();
});

test('cached app is rejected and private lease removed if clean HEAD changes at final verification', async () => {
  const f = await fixture();
  await withRelease(await f.prepare());
  const originalRun = f.options.run;
  let headReads = 0;
  f.options.run = async (name, args, options) => {
    if (name === 'git' && args[0] === 'rev-parse') {
      headReads++;
      if (headReads === 3) f.state.commit = 'b'.repeat(40);
    }
    return originalRun(name, args, options);
  };
  await assert.rejects(f.prepare(), /Checkout changed during bundle preparation/);
  assert.equal(f.state.builds, 1);
  const cache = path.join(f.root, 'target/galaxy-build-cache');
  assert.equal((await fs.readdir(cache)).some((entry) => entry.startsWith('lease-')), false);
});

test('failed SDK identity probe still rejects changed HEAD after bundling', async () => {
  const f = await fixture();
  f.state.failSdk = true;
  f.state.changeCommitOnBuild = true;
  await assert.rejects(f.prepare(), /Checkout changed during bundle preparation/);
  await assert.rejects(fs.access(path.join(f.root, 'target/galaxy-build-cache/manifest.json')));
});

test('dead owner lock fails safely with manual removal guidance', async () => {
  const f = await fixture();
  const lock = path.join(f.root, 'target/galaxy-build-cache/prepare.lock');
  await fs.mkdir(path.dirname(lock), { recursive: true });
  await fs.symlink(JSON.stringify({ pid: 99999999, start: 'old process', token: 'orphan' }), lock);
  await assert.rejects(f.prepare(), /Stale Galaxy build lock.*remove it manually/);
  assert.equal(f.state.builds, 0);
  assert.ok((await fs.lstat(lock)).isSymbolicLink());
});

test('healthy lock owner survives longer than a short wait', async () => {
  const f = await fixture();
  const lock = path.join(f.root, 'target/galaxy-build-cache/prepare.lock');
  const firstRun = f.options.run;
  let finish;
  const pending = new Promise((resolve) => { finish = resolve; });
  let started;
  const building = new Promise((resolve) => { started = resolve; });
  f.options.run = async (name, args, options) => {
    if (name === 'cargo' && args[0] === 'bundle' && args.includes('--release')) { started(); await pending; }
    return firstRun(name, args, options);
  };
  const first = f.prepare();
  await building;
  const second = f.prepare();
  await new Promise((resolve) => setTimeout(resolve, 1200));
  assert.equal(f.state.builds, 0);
  assert.ok((await fs.lstat(lock)).isSymbolicLink());
  finish();
  const [a, b] = await Promise.all([first, second]);
  assert.equal(f.state.builds, 1);
  await a.release();
  await b.release();
});

test('ignored mutable bundle resources disable reuse', async () => {
  const f = await fixture();
  f.state.ignoredResources = 'app/assets/onboarding/ignored-file';
  await withRelease(await f.prepare());
  const result = await f.prepare();
  assert.equal(result.reused, false);
  await result.release();
});

test('concurrent requests serialize preparation and preserve private signed leases', async () => {
  const f = await fixture();
  const [a, b] = await Promise.all([f.prepare(), f.prepare()]);
  assert.equal(f.state.builds, 1);
  await fs.writeFile(path.join(a.appPath, 'Contents/MacOS/galaxy-oss'), 'signed');
  assert.equal(await withRelease(b), 'built 1');
  await a.release();
});

test('fresh and reused private bundles include both prebuilt Wormhole helpers without compiling them', async () => {
  const f = await fixture();
  for (const reused of [false, true]) {
    const result = await f.prepare();
    assert.equal(result.reused, reused);
    for (const architecture of ['linux-x86_64', 'linux-aarch64']) {
      for (const filename of ['galaxy-wormhole.tar.gz', 'galaxy-wormhole.version']) {
        assert.deepEqual(
          await fs.readFile(path.join(result.appPath, 'Contents/Resources/wormhole-helpers', architecture, filename)),
          await fs.readFile(path.join(f.root, 'resources/wormhole-helpers', architecture, filename)),
        );
      }
    }
    await result.release();
  }
  assert.equal(f.state.builds, 1);
  assert.equal(f.state.commands.some(([command, ...args]) => command === 'cargo' && args[0] === 'build'), false);
});

test('missing or invalid prebuilt helpers fail before Cargo bundling', async () => {
  for (const filename of ['galaxy-wormhole.tar.gz', 'galaxy-wormhole.version']) {
    const f = await fixture();
    await fs.rm(path.join(f.root, 'resources/wormhole-helpers/linux-aarch64', filename));
    await assert.rejects(f.prepare(), /Prebuilt Wormhole helper linux-aarch64 is missing or invalid.*will not compile/);
    assert.equal(f.state.builds, 0);
  }
  for (const [filename, content] of [
    ['galaxy-wormhole.tar.gz', 'not a gzip archive, even though this file is long enough'],
    ['galaxy-wormhole.version', 'invalid version'],
  ]) {
    const f = await fixture();
    await fs.writeFile(path.join(f.root, 'resources/wormhole-helpers/linux-x86_64', filename), content);
    await assert.rejects(f.prepare(), /Prebuilt Wormhole helper linux-x86_64 is missing or invalid/);
    assert.equal(f.state.builds, 0);
  }
});

test('legacy or incomplete resource caches are rebuilt rather than reused', async () => {
  const f = await fixture();
  await withRelease(await f.prepare());
  const manifestPath = path.join(f.root, 'target/galaxy-build-cache/manifest.json');
  const manifest = JSON.parse(await fs.readFile(manifestPath, 'utf8'));
  manifest.version = 1;
  await fs.writeFile(manifestPath, JSON.stringify(manifest));
  const legacy = await f.prepare();
  assert.equal(legacy.reused, false);
  await legacy.release();
  await fs.rm(path.join(f.root, 'target/galaxy-build-cache/Galaxy.app/Contents/Resources/wormhole-helpers/linux-aarch64/galaxy-wormhole.tar.gz'));
  const result = await f.prepare();
  assert.equal(result.reused, false);
  await result.release();
  assert.equal(f.state.builds, 3);
});

test('a successful cargo-bundle command without an app cannot manufacture a helper-only bundle', async () => {
  const f = await fixture();
  f.state.noBundleOutput = true;
  await assert.rejects(f.prepare(), /cargo-bundle did not produce a complete Galaxy\.app/);
  await assert.rejects(fs.access(f.app));
  await assert.rejects(fs.access(path.join(f.root, 'target/galaxy-build-cache/Galaxy.app')));
  await assert.rejects(fs.access(path.join(f.root, 'target/galaxy-build-cache/manifest.json')));
});

test('same-sized archive changes with an unchanged version marker fail exact helper verification', async () => {
  const f = await fixture();
  const helpers = await inspectWormholeHelpers(path.join(f.root, 'resources/wormhole-helpers'));
  const destination = path.join(f.root, 'copied-helpers');
  await copyWormholeHelpers(helpers, destination);
  const archive = path.join(destination, 'linux-aarch64/galaxy-wormhole.tar.gz');
  const changed = await fs.readFile(archive);
  changed[changed.length - 1] ^= 1;
  await fs.writeFile(archive, changed);
  assert.equal((await fs.stat(archive)).size, helpers[1].archiveBytes);
  await assert.rejects(verifyWormholeHelpers(helpers, destination), /does not match its prebuilt source/);
});
