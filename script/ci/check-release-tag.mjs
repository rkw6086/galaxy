#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { checkVersions, compareVersions, stageVersion } from '../release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;

export function tagVersion(eventName, ref, after) {
  if (eventName !== 'push' || !ref?.startsWith('refs/tags/') || !after || /^0+$/.test(after)) {
    throw new Error('Signing is allowed only for a pushed version tag.');
  }
  const version = ref.slice('refs/tags/'.length);
  if (!VERSION.test(version)) throw new Error('Use a bare stable Semantic Version tag, such as 3.1.0.');
  return version;
}

export function checkHistory(runs, version, sha) {
  for (const run of runs) {
    // Gitea leaves head_branch empty for tags; path retains the actual triggering ref.
    const tagPath = `macos-release.yml@refs/tags/${version}`;
    if (run.event !== 'push' || !(run.path === tagPath || run.path === `.gitea/workflows/${tagPath}`)) continue;
    if (run.head_sha && run.head_sha !== sha) {
      throw new Error(`Tag ${version} was previously used for a different commit. Create a new version.`);
    }
  }
}

async function main() {
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const version = tagVersion(process.env.GITHUB_EVENT_NAME, process.env.GITHUB_REF, event.after);
  const sourceVersion = checkVersions();
  if (compareVersions(version, sourceVersion) < 0) throw new Error('Release tags cannot lower the source package version.');
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
  if (git('status', '--porcelain', '--untracked-files=all')) throw new Error('Release validation requires a clean source checkout.');
  const sha = git('rev-parse', 'HEAD');
  if (git('rev-parse', `refs/tags/${version}^{commit}`) !== sha) throw new Error('Checkout does not match the version tag.');
  if (process.env.GITHUB_SHA && git('rev-parse', `${process.env.GITHUB_SHA}^{commit}`) !== sha) {
    throw new Error('The triggering commit does not match the checkout.');
  }
  git('merge-base', '--is-ancestor', sha, 'refs/remotes/origin/master');
  const server = new URL(process.env.GITEA_SERVER_URL);
  if (server.protocol !== 'https:' || server.username || server.password || server.search || server.hash || server.pathname !== '/') {
    throw new Error('Gitea must use an HTTPS server URL without credentials, a path, or query parameters.');
  }
  const repository = process.env.GITEA_REPOSITORY;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository) || repository.split('/').some((p) => p === '.' || p === '..')) {
    throw new Error('Invalid repository.');
  }
  const token = process.env.GITEA_TOKEN;
  if (!token) throw new Error('Missing workflow token.');
  const api = async (suffix, allowMissing = false) => {
    const response = await fetch(new URL(`/api/v1/repos/${repository}${suffix}`, server), {
      headers: { Authorization: `token ${token}`, Accept: 'application/json' },
      redirect: 'error', signal: AbortSignal.timeout(30_000),
    });
    if (response.status === 404 && allowMissing) return null;
    if (!response.ok) throw new Error(`Release preflight API returned HTTP ${response.status}.`);
    return response.json();
  };
  const tag = await api(`/tags/${encodeURIComponent(version)}`);
  if (tag.commit?.sha !== sha) throw new Error('Remote tag was moved after this run started.');
  for (let page = 1; page <= 100; page += 1) {
    const data = await api(`/actions/workflows/macos-release.yml/runs?limit=100&page=${page}`);
    const runs = Array.isArray(data) ? data : (data.workflow_runs ?? data.runs);
    if (!Array.isArray(runs)) throw new Error('Cannot verify version tag history.');
    checkHistory(runs, version, sha);
    if (runs.length < 100) break;
    if (page === 100) throw new Error('Version history exceeds the safe preflight limit.');
  }
  const existing = await api(`/releases/tags/${encodeURIComponent(version)}`, true);
  if (existing && !existing.draft) {
    fs.appendFileSync(process.env.GITHUB_OUTPUT, 'release=false\n');
    console.log(`Galaxy ${version} is already released. Signing is skipped.`);
    return;
  }
  // Release assets may be pruned, but immutable version tags preserve history.
  for (const name of git('tag', '--list').split('\n')) {
    if (name === version) continue;
    const previous = name.startsWith('v') ? name.slice(1) : name;
    if (VERSION.test(previous) && compareVersions(version, previous) <= 0) {
      throw new Error(`Use a version newer than existing tag ${name}.`);
    }
  }
  for (let page = 1; page <= 100; page += 1) {
    const releases = await api(`/releases?limit=100&page=${page}`);
    if (!Array.isArray(releases)) throw new Error('Cannot verify published release versions.');
    for (const release of releases) {
      const previous = release.tag_name;
      if (!release.draft && !release.prerelease && VERSION.test(previous ?? '')
          && compareVersions(version, previous) <= 0) {
        throw new Error(`Use a version newer than published release ${previous}.`);
      }
    }
    if (releases.length < 100) break;
    if (page === 100) throw new Error('Published release history exceeds the safe preflight limit.');
  }
  // The tag selects the bundle version; source history and the tag stay intact.
  stageVersion(version, sha);
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `release=true\nversion=${version}\nsha=${sha}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV, `GALAXY_RELEASE_VERSION=${version}\nGALAXY_RELEASE_SHA=${sha}\nRELEASE_TAG=${version}\nGALAXY_VERSION_STAGED=1\n`);
  console.log(`Validated new Galaxy ${version} release at ${sha}.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
