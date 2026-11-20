#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const stableVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const remotes = ['origin'];

export function parseVersion(value) {
  if (typeof value !== 'string' || !stableVersion.test(value)) {
    throw new Error('Use a bare stable version such as 3.1.0, without a v prefix or suffix.');
  }
  return value.split('.').map(BigInt);
}

export function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return a[i] < b[i] ? -1 : 1;
  }
  return 0;
}

function manifestVersion(text) {
  const block = text.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[|$(?![\s\S]))/m)?.[1];
  const version = block?.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) throw new Error('app/Cargo.toml is missing package.version.');
  return version;
}

function galaxyLockBlock(text) {
  const blocks = text.split(/(?=^\[\[package\]\]$)/m);
  const matches = blocks.filter((block) => /^name = "galaxy"$/m.test(block) && !/^source = /m.test(block));
  if (matches.length !== 1) throw new Error('Cargo.lock must contain exactly one local galaxy package.');
  return matches[0];
}

export function checkVersions({ repo = root, expected } = {}) {
  const version = manifestVersion(fs.readFileSync(path.join(repo, 'app/Cargo.toml'), 'utf8'));
  parseVersion(version);
  const lock = galaxyLockBlock(fs.readFileSync(path.join(repo, 'Cargo.lock'), 'utf8'));
  const locked = lock.match(/^version = "([^"]+)"$/m)?.[1];
  if (locked !== version) throw new Error(`Cargo.lock has ${locked}; app/Cargo.toml has ${version}. Run release.mjs prepare.`);
  if (expected !== undefined && version !== expected) throw new Error(`Package version ${version} does not match ${expected}.`);
  return version;
}

export function changelog(version, { repo = root } = {}) {
  parseVersion(version);
  const lines = fs.readFileSync(path.join(repo, 'CHANGELOG.md'), 'utf8').split(/\r?\n/);
  const indices = lines.flatMap((line, index) => line === `## [${version}]` ? [index] : []);
  if (indices.length !== 1) throw new Error(`Add exactly one ## [${version}] section to CHANGELOG.md.`);
  const start = indices[0] + 1;
  const next = lines.findIndex((line, index) => index >= start && line.startsWith('## '));
  const notes = lines.slice(start, next < 0 ? undefined : next).join('\n').trim();
  if (!notes || !/^\s*[-*] \S/m.test(notes)) throw new Error(`Release ${version} needs changelog notes.`);
  return notes;
}

function git(repo, ...args) {
  return execFileSync('git', args, { cwd: repo, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

// Derive the only two CI version edits from the immutable tagged source.
export function versionFiles(manifest, lock, version) {
  parseVersion(version);
  const current = manifestVersion(manifest);
  parseVersion(current);
  const block = galaxyLockBlock(lock);
  if (block.match(/^version = "([^"]+)"$/m)?.[1] !== current) {
    throw new Error('Source package versions do not match.');
  }
  if (compareVersions(version, current) < 0) throw new Error('Release versions cannot move backwards.');
  return new Map([
    ['app/Cargo.toml', manifest.replace(
      /(^\[package\]\s*\n[\s\S]*?^version\s*=\s*")[^"]+("\s*$)/m,
      (_, before, after) => `${before}${version}${after}`,
    )],
    ['Cargo.lock', lock.replace(block, block.replace(/^version = "[^"]+"$/m, `version = "${version}"`))],
  ]);
}

function stagingSource(version, sha, repo) {
  parseVersion(version);
  if (!/^(?:[0-9a-f]{40}|[0-9a-f]{64})$/.test(sha ?? '') || git(repo, 'rev-parse', 'HEAD') !== sha) {
    throw new Error('Version staging requires the exact validated source commit.');
  }
  if (process.env.CI !== 'true' || process.env.GITHUB_EVENT_NAME !== 'push'
      || process.env.GITHUB_REF !== `refs/tags/${version}`
      || git(repo, 'rev-parse', `refs/tags/${version}^{commit}`) !== sha) {
    throw new Error('CI version staging is restricted to the validated pushed version tag.');
  }
  const source = (file) => execFileSync('git', ['show', `${sha}:${file}`], {
    cwd: repo, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  });
  return versionFiles(source('app/Cargo.toml'), source('Cargo.lock'), version);
}

export function verifyStagedVersion(version, sha, { repo = root } = {}) {
  const expected = stagingSource(version, sha, repo);
  // Index changes, renames, modes, and new files are never part of version staging.
  const status = execFileSync('git', ['status', '--porcelain=v1', '--untracked-files=all', '-z'], {
    cwd: repo, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
  });
  for (const entry of status.split('\0').filter(Boolean)) {
    if (!entry.startsWith(' M ') || !expected.has(entry.slice(3))) {
      throw new Error('Checkout has changes outside the two controlled CI version edits.');
    }
  }
  for (const [file, content] of expected) {
    const target = path.join(repo, file);
    const stat = fs.lstatSync(target);
    if (!stat.isFile() || (stat.mode & 0o111) !== 0 || fs.readFileSync(target, 'utf8') !== content) {
      throw new Error(`Unexpected CI version contents or file mode: ${file}.`);
    }
  }
  return checkVersions({ repo, expected: version });
}

export function stageVersion(version, sha, { repo = root } = {}) {
  if (git(repo, 'status', '--porcelain', '--untracked-files=all')) {
    throw new Error('CI version staging requires an unchanged source checkout.');
  }
  const expected = stagingSource(version, sha, repo);
  for (const [file, content] of expected) {
    const target = path.join(repo, file);
    if (fs.readFileSync(target, 'utf8') === content) continue;
    const temporary = `${target}.galaxy-version-tmp`;
    fs.writeFileSync(temporary, content, { flag: 'wx', mode: fs.statSync(target).mode });
    fs.renameSync(temporary, target);
  }
  return verifyStagedVersion(version, sha, { repo });
}

function prepare(target) {
  const current = checkVersions();
  let version = target;
  if (['major', 'minor', 'patch'].includes(target)) {
    const parts = parseVersion(current);
    const index = ['major', 'minor', 'patch'].indexOf(target);
    parts[index] += 1n;
    for (let i = index + 1; i < parts.length; i += 1) parts[i] = 0n;
    version = parts.join('.');
  }
  parseVersion(version);
  if (compareVersions(version, current) < 0) throw new Error('Release versions cannot move backwards.');
  const expected = versionFiles(fs.readFileSync(path.join(root, 'app/Cargo.toml'), 'utf8'),
    fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8'), version);
  for (const [relative, text] of expected) {
    const file = path.join(root, relative);
    if (fs.readFileSync(file, 'utf8') === text) continue;
    const temporary = `${file}.galaxy-version-tmp`;
    fs.writeFileSync(temporary, text, { flag: 'wx', mode: fs.statSync(file).mode });
    fs.renameSync(temporary, file);
  }
  return checkVersions({ expected: version });
}

function tag(requested, push) {
  const current = checkVersions();
  const version = requested ?? current;
  parseVersion(version);
  if (compareVersions(version, current) < 0) throw new Error('Release versions cannot move backwards.');
  // Codex attachments are user-provided files, never release build inputs.
  if (git(root, 'status', '--porcelain', '--untracked-files=normal', '--', '.', ':(exclude).codex-attachments')) {
    throw new Error('Commit all release changes before tagging.');
  }
  if (git(root, 'branch', '--show-current') !== 'master') throw new Error('Create release tags from master.');
  const head = git(root, 'rev-parse', 'HEAD');
  const tags = new Set(git(root, 'tag', '--list').split('\n'));
  for (const remote of remotes) {
    const remoteHead = git(root, 'ls-remote', remote, 'refs/heads/master').split(/\s+/)[0];
    if (head !== remoteHead) throw new Error(`Push master to ${remote} before tagging.`);
    for (const line of git(root, 'ls-remote', '--tags', remote).split('\n')) {
      const name = line.split('\t')[1]?.replace(/^refs\/tags\//, '').replace(/\^\{\}$/, '');
      if (name) tags.add(name);
    }
  }
  for (const existing of tags) {
    if (existing === version) throw new Error(`Tag ${version} already exists. Tags are never overwritten.`);
    const normalized = existing.startsWith('v') ? existing.slice(1) : existing;
    if (stableVersion.test(normalized) && compareVersions(version, normalized) <= 0) {
      throw new Error(`Use a version newer than existing tag ${existing}.`);
    }
  }
  git(root, 'tag', '-a', version, '-m', `Galaxy ${version}`);
  if (push) {
    // Gitea owns distribution of this immutable tag to the external mirrors.
    git(root, 'push', 'origin', `refs/tags/${version}`);
  }
  return version;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [command = 'check', argument, option, ...extra] = process.argv.slice(2);
    if (extra.length) throw new Error('Too many release arguments.');
    if (command === 'check') {
      if (option) throw new Error('Usage: node script/release.mjs check [X.Y.Z]');
      console.log(`Galaxy ${checkVersions({ expected: argument })}: package versions match.`);
    } else if (command === 'prepare') {
      if (!argument || option) throw new Error('Usage: node script/release.mjs prepare major|minor|patch|X.Y.Z');
      console.log(`Prepared Galaxy ${prepare(argument)}. Build, commit, and push origin before tagging.`);
    } else if (command === 'stage' || command === 'verify-staged') {
      if (!argument || !option) throw new Error(`Usage: node script/release.mjs ${command} X.Y.Z SOURCE_SHA`);
      const version = command === 'stage' ? stageVersion(argument, option) : verifyStagedVersion(argument, option);
      console.log(`Galaxy ${version}: controlled CI version edits verified.`);
    } else if (command === 'tag') {
      if ((option && option !== '--push') || (argument === '--push' && option)) {
        throw new Error('Usage: node script/release.mjs tag [X.Y.Z] [--push]');
      }
      const push = argument === '--push' || option === '--push';
      const version = tag(argument === '--push' ? undefined : argument, push);
      console.log(push ? `Pushed Galaxy ${version} to Gitea; sync jobs will update GitHub and Samsung and trigger the GitHub builds.` : `Created ${version}. Push this tag to origin to release.`);
    } else throw new Error('Use check, prepare, tag, stage, or verify-staged.');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
