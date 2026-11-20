#!/usr/bin/env node
// Validate GitHub-owned build inputs before any platform receives signing keys.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { checkVersions, compareVersions, changelog } from '../release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const COMMIT = /^[0-9a-f]{40}$/;
const MARKER = /<!-- galaxy-beta:v1 commit=([0-9a-f]{40}) -->/;
const STAGE = /^beta-stage-([0-9a-f]{40})-[0-9a-f]{16}$/;
const CUTOVER = /<!-- galaxy-beta-cutover:v1 previous=([0-9a-f]{40}|none) release=([1-9][0-9]{0,18}|none) orphan=([0-9a-f]{40}|none) -->/g;
const git = (...args) => execFileSync('git', args, {
  cwd: root, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024,
  timeout: 60_000,
  stdio: ['ignore', 'pipe', 'pipe'],
}).trim();

function names(channel, version, sha) {
  const identity = channel === 'beta' ? `beta_${sha}` : version;
  return new Set([
    `Galaxy_${identity}_aarch64.dmg`, `Galaxy_${identity}_x86_64-setup.exe`,
    `Galaxy_${identity}_amd64.deb`, `Galaxy_${identity}_linux_x86_64.tar.gz`, 'SHA256SUMS',
  ]);
}

async function main() {
  if (process.env.GITHUB_SERVER_URL !== 'https://github.com'
      || process.env.GITHUB_REPOSITORY !== 'rkw6086/galaxy'
      || (process.env.GITHUB_API_URL && process.env.GITHUB_API_URL !== 'https://api.github.com')) {
    throw new Error('This build workflow is restricted to rkw6086/galaxy on GitHub.');
  }
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const eventName = process.env.GITHUB_EVENT_NAME;
  const ref = process.env.GITHUB_REF;
  let channel;
  if (eventName === 'pull_request' || (eventName === 'push' && ref === 'refs/heads/master')) channel = 'ci';
  else if (['schedule', 'workflow_dispatch'].includes(eventName) && ref === 'refs/heads/master') channel = 'beta';
  else if (eventName === 'push' && ref?.startsWith('refs/tags/') && !event.deleted && !/^0+$/.test(event.after ?? '')) channel = 'release';
  else throw new Error('Unsupported Galaxy build event or ref.');
  const sourceVersion = checkVersions();
  const sha = git('rev-parse', 'HEAD');
  if (!COMMIT.test(sha) || git('status', '--porcelain', '--untracked-files=all')) {
    throw new Error('Build preparation requires an unchanged source checkout.');
  }
  if (channel === 'ci' && process.env.GITHUB_SHA !== sha) throw new Error('CI checkout differs from the triggering commit.');
  const token = process.env.GITHUB_TOKEN;
  const api = async (suffix, missing = false) => {
    const headers = { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2026-03-10' };
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(`https://api.github.com/repos/rkw6086/galaxy${suffix}`, {
      headers, redirect: 'error', signal: AbortSignal.timeout(30_000),
    });
    if (response.status === 404 && missing) return null;
    if (!response.ok) throw new Error(`GitHub build preflight API returned HTTP ${response.status}.`);
    return response.json();
  };
  const tagCommit = async (tag, missing = false) => {
    const value = await api(`/git/ref/tags/${encodeURIComponent(tag)}`, missing);
    if (!value) return null;
    let object = value.object;
    for (let depth = 0; depth < 8 && object?.type === 'tag'; depth += 1) object = (await api(`/git/tags/${object.sha}`)).object;
    if (object?.type !== 'commit' || !COMMIT.test(object.sha)) throw new Error('Tag does not identify a bounded commit object.');
    return object.sha;
  };
  let releaseHistory;
  const allReleases = async () => {
    if (releaseHistory) return releaseHistory;
    releaseHistory = [];
    for (let page = 1; page <= 100; page += 1) {
      const values = await api(`/releases?per_page=100&page=${page}`);
      if (!Array.isArray(values)) throw new Error('Cannot inspect release history.');
      releaseHistory.push(...values);
      if (values.length < 100) return releaseHistory;
      if (page === 100) throw new Error('Release history exceeds its safe bound.');
    }
    return releaseHistory;
  };
  let version = sourceVersion;
  let previous = '';
  let previousReleaseId = '';
  let baseline = '';
  let notes = '';
  if (channel !== 'ci') {
    if (!token || /\s/.test(token)) throw new Error('Missing GitHub workflow token.');
    const master = await api('/branches/master');
    const primaryRef = channel === 'beta' ? 'refs/heads/master' : ref;
    const primaryRows = git('-c', 'credential.helper=', 'ls-remote',
      'https://git.ryserve.net/public/galaxy.git', primaryRef, `${primaryRef}^{}`)
      .split('\n').filter(Boolean).map((line) => line.split('\t'));
    const primary = primaryRows.find((row) => row[1] === `${primaryRef}^{}`)?.[0]
      ?? primaryRows.find((row) => row[1] === primaryRef)?.[0];
    if (primary !== sha) throw new Error('Gitea primary ref differs from this source; wait for repository sync before signing.');
    if (channel === 'beta') {
      if (master.commit?.sha !== sha || git('rev-parse', 'refs/remotes/origin/master') !== sha) {
        throw new Error('Beta checkout must match current GitHub master.');
      }
      const current = await api('/releases/tags/beta', true);
      if (current) {
        previous = current.body?.match(MARKER)?.[1] ?? '';
        if (!Number.isSafeInteger(current.id) || current.id <= 0) throw new Error('Existing beta release identity is invalid.');
        previousReleaseId = String(current.id);
        if (!previous || current.draft || !current.prerelease || current.target_commitish !== previous) {
          throw new Error('Existing GitHub beta is not an owned rolling Galaxy release.');
        }
        const expected = names('beta', version, sha);
        const complete = current.assets?.length === expected.size && current.assets.every((asset) =>
          expected.delete(asset.name) && Number.isSafeInteger(asset.size) && asset.size > 0 && asset.size <= 2 * 1024 ** 3)
          && expected.size === 0;
        if (eventName === 'schedule' && previous === sha && complete && await tagCommit('beta') === sha) {
          fs.appendFileSync(process.env.GITHUB_OUTPUT, 'build=false\nchannel=beta\n');
          console.log(`Published beta already contains ${sha}; no build is needed.`);
          return;
        }
      } else {
        // An interrupted promotion may have removed the old release record but
        // left its ref. Ownership must come from a published, bounded staging
        // record that persisted the exact cutover before removing anything.
        // The publisher re-downloads and hashes the selected proof's assets.
        // If no ref remains, this metadata also restores the notes baseline;
        // there is no existing alias to consume in that case.
        const orphan = await tagCommit('beta', true);
        {
          const proofs = [];
          for (const stage of await allReleases()) {
            const stageTag = stage.tag_name?.match(STAGE);
            const sources = [...(stage.body ?? '').matchAll(new RegExp(MARKER.source, 'g'))];
            const cutovers = [...(stage.body ?? '').matchAll(CUTOVER)];
            if (!stageTag || stage.draft || !stage.prerelease || sources.length !== 1 || cutovers.length !== 1
                || stageTag[1] !== sources[0][1] || stage.target_commitish !== sources[0][1]) continue;
            const [, prior, priorId, recordedOrphan] = cutovers[0];
            if ((prior === 'none') !== (priorId === 'none') || !Number.isSafeInteger(stage.id) || stage.id <= 0
                || (priorId !== 'none' && !Number.isSafeInteger(Number(priorId)))
                || (orphan && ![stageTag[1], recordedOrphan].includes(orphan))) continue;
            const expected = names('beta', version, stageTag[1]);
            if (stage.assets?.length !== expected.size || !stage.assets.every((asset) =>
              expected.delete(asset.name) && Number.isSafeInteger(asset.size) && asset.size > 0
                && asset.size <= (asset.name === 'SHA256SUMS' ? 8192 : 2 * 1024 ** 3)) || expected.size) continue;
            try { git('merge-base', '--is-ancestor', stageTag[1], sha); } catch { continue; }
            proofs.push({ stage, prior: prior === 'none' ? '' : prior, priorId: priorId === 'none' ? '' : priorId });
            if (proofs.length > 20) throw new Error('Too many interrupted beta cutover proofs; inspect release state.');
          }
          if ((orphan && !proofs.length) || (proofs.length && new Set(proofs.map((proof) => `${proof.prior}:${proof.priorId}`)).size !== 1)) {
            throw new Error('Interrupted beta state has no unambiguous owned cutover proof; left unchanged.');
          }
          const proof = proofs.sort((a, b) => b.stage.id - a.stage.id)[0];
          if (proof) {
            if (await tagCommit(proof.stage.tag_name) !== proof.stage.target_commitish) throw new Error('Recovery staging ref changed.');
            previous = proof.prior;
            previousReleaseId = proof.priorId;
            console.log('Recovered beta preparation metadata from a published interrupted cutover.');
          }
        }
      }
      baseline = previous;
      if (!baseline) {
        // Use the last published Gitea beta for migration notes, never failed runs.
        try {
          const response = await fetch('https://git.ryserve.net/api/v1/repos/public/galaxy/releases/tags/beta', {
            redirect: 'error', signal: AbortSignal.timeout(5_000),
          });
          if (response.ok) {
            const old = await response.json();
            const commit = old.body?.match(MARKER)?.[1];
            if (!old.draft && old.prerelease && COMMIT.test(commit ?? '')) baseline = commit;
          }
        } catch { /* First GitHub beta can use the latest stable source instead. */ }
      }
    } else {
      version = ref.slice('refs/tags/'.length);
      if (!VERSION.test(version) || compareVersions(version, sourceVersion) < 0) throw new Error('Use a bare, nondecreasing stable version tag.');
      if (git('rev-parse', `${ref}^{commit}`) !== sha || await tagCommit(version) !== sha
          || git('rev-parse', `${process.env.GITHUB_SHA}^{commit}`) !== sha) throw new Error('Release tag or triggering source changed.');
      git('merge-base', '--is-ancestor', sha, 'refs/remotes/origin/master');
      const current = await api(`/releases/tags/${version}`, true);
      if (current && !current.draft) {
        fs.appendFileSync(process.env.GITHUB_OUTPUT, 'build=false\nchannel=release\n');
        console.log(`Galaxy ${version} is already published; no signing is needed.`);
        return;
      }
      for (const tag of git('tag', '--list').split('\n')) {
        const numeric = tag.startsWith('v') ? tag.slice(1) : tag;
        if (tag !== version && VERSION.test(numeric) && compareVersions(version, numeric) <= 0) {
          throw new Error(`Version must be newer than retained tag ${tag}.`);
        }
      }
      for (let page = 1; page <= 100; page += 1) {
        const data = await api(`/actions/workflows/build.yml/runs?event=push&per_page=100&page=${page}`);
        if (!Array.isArray(data.workflow_runs)) throw new Error('Cannot verify release workflow history.');
        for (const run of data.workflow_runs) {
          if (run.head_branch === version && run.head_sha && run.head_sha !== sha) throw new Error('This version tag was used for another source; create a new version.');
        }
        if (data.workflow_runs.length < 100) break;
        if (page === 100) throw new Error('Release workflow history exceeds its safe bound.');
      }
      try { notes = changelog(version); } catch { /* Generate bounded commit notes below. */ }
    }
    const releases = await allReleases();
    const stable = releases.filter((release) => !release.draft && !release.prerelease && VERSION.test(release.tag_name ?? ''))
      .sort((a, b) => compareVersions(b.tag_name, a.tag_name));
    if (channel === 'release' && stable.some((release) => compareVersions(release.tag_name, version) >= 0)) throw new Error('A newer or equal stable release is already published.');
    if (!baseline) baseline = stable[0]?.body?.match(/<!-- galaxy-release:v1 tag=[0-9.]+ commit=([0-9a-f]{40}) -->/)?.[1] ?? '';
    if (!notes) {
      let range = sha;
      if (baseline) {
        try { git('merge-base', '--is-ancestor', baseline, sha); range = `${baseline}..${sha}`; } catch { baseline = ''; }
      }
      if (baseline === sha) notes = 'Manually rebuilt master with no source changes since the previous beta.';
      else {
        const commits = git('log', '--format=%h %s', '--max-count=201', range, '--').split('\n').filter(Boolean);
        const lines = [];
        let size = 0;
        for (const line of commits.slice(0, 200)) {
          const bullet = `- ${line.replace(/[\x00-\x1f\x7f]/g, ' ').slice(0, 500)}`;
          size += Buffer.byteLength(bullet) + 1;
          if (size > 96 * 1024) break;
          lines.push(bullet);
        }
        notes = `${baseline ? 'Changes since the previous published build:' : 'Initial GitHub build:'}\n\n${lines.join('\n') || '- No source changes.'}`;
        if (commits.length > lines.length) notes += `\n\nShowing the latest ${lines.length} commits within the release-note limit.`;
        if (baseline) notes += `\n\n[Compare changes](https://github.com/rkw6086/galaxy/compare/${baseline}...${sha})`;
      }
    }
    const temporary = process.env.RUNNER_TEMP;
    const job = process.env.GALAXY_JOB_ID;
    if (!temporary || !path.isAbsolute(temporary) || !/^[0-9]+-[0-9]+$/.test(job ?? '')) throw new Error('Missing job-owned notes directory identity.');
    const directory = path.join(temporary, `galaxy-notes-${job}`);
    fs.mkdirSync(directory, { mode: 0o700 });
    const file = path.join(directory, 'release-notes.md');
    if (!notes.trim() || Buffer.byteLength(notes) > 128 * 1024) throw new Error('Release notes exceed their safe bound.');
    fs.writeFileSync(file, `${notes.trim()}\n`, { mode: 0o600, flag: 'wx' });
    fs.appendFileSync(process.env.GITHUB_ENV, `GALAXY_RELEASE_NOTES_FILE=${file}\n`);
    fs.appendFileSync(process.env.GITHUB_OUTPUT, `notes_file=${file}\n`);
  }
  fs.appendFileSync(process.env.GITHUB_OUTPUT,
    `build=true\nchannel=${channel}\nversion=${version}\nsha=${sha}\nprevious_beta_sha=${previous}\nprevious_beta_release_id=${previousReleaseId}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV,
    `GALAXY_RELEASE_VERSION=${version}\nGALAXY_RELEASE_SHA=${sha}\nGALAXY_PREVIOUS_BETA_SHA=${previous}\nGALAXY_PREVIOUS_BETA_RELEASE_ID=${previousReleaseId}\n`);
  console.log(`Validated Galaxy ${channel} ${version} at ${sha}.`);
}

main().catch((error) => { console.error(error.message); process.exitCode = 1; });
