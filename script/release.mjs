#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const stableVersion = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const remotes = ['origin', 'samsung'];

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
  const manifestPath = path.join(root, 'app/Cargo.toml');
  const lockPath = path.join(root, 'Cargo.lock');
  const manifest = fs.readFileSync(manifestPath, 'utf8');
  const lock = fs.readFileSync(lockPath, 'utf8');
  const updatedManifest = manifest.replace(
    /(^\[package\]\s*\n[\s\S]*?^version\s*=\s*")[^"]+("\s*$)/m,
    (_, before, after) => `${before}${version}${after}`,
  );
  const block = galaxyLockBlock(lock);
  const updatedLock = lock.replace(block, block.replace(/^version = "[^"]+"$/m, `version = "${version}"`));
  for (const [file, text] of [[manifestPath, updatedManifest], [lockPath, updatedLock]]) {
    if (fs.readFileSync(file, 'utf8') === text) continue;
    const temporary = `${file}.galaxy-version-tmp`;
    fs.writeFileSync(temporary, text, { flag: 'wx', mode: fs.statSync(file).mode });
    fs.renameSync(temporary, file);
  }
  return checkVersions({ expected: version });
}

function tag(requested, push) {
  const version = checkVersions({ expected: requested });
  changelog(version);
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
    // Mirror the tag first, then start the release workflow on Gitea.
    git(root, 'push', 'samsung', `refs/tags/${version}`);
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
      console.log(`Prepared Galaxy ${prepare(argument)}. Add changelog notes, build, commit, and push both remotes before tagging.`);
    } else if (command === 'tag') {
      if ((option && option !== '--push') || (argument === '--push' && option)) {
        throw new Error('Usage: node script/release.mjs tag [X.Y.Z] [--push]');
      }
      const push = argument === '--push' || option === '--push';
      const version = tag(argument === '--push' ? undefined : argument, push);
      console.log(push ? `Pushed Galaxy ${version} to both remotes; Gitea will build the signed release.` : `Created ${version}. Push this tag to samsung and origin to release.`);
    } else throw new Error('Use check, prepare, or tag.');
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
