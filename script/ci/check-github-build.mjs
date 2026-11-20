#!/usr/bin/env node
// Resolve trusted build inputs before any platform receives signing keys.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { checkVersions, compareVersions, changelog } from '../release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const SHA = /^[0-9a-f]{40}$/;
const BUILD = /^([0-9a-f]{40})_([1-9][0-9]{0,18})-([1-9][0-9]{0,8})$/;
const NIGHTLY = /<!-- galaxy-nightly:v1 commit=([0-9a-f]{40}) build=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}) -->/g;
const TRANSITION = /<!-- galaxy-nightly-transition:v1 release=(new|[1-9][0-9]{0,18}) previous=([0-9a-f]{40}|none) build=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}|none) ref=([0-9a-f]{40}|none) candidate=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}) -->/g;
const git = (...args) => execFileSync('git', args, {
  cwd: root, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024, timeout: 60_000,
  stdio: ['ignore', 'pipe', 'pipe'],
}).trim();

function ownedNightlyPlaceholder(release) {
  // GitHub can temporarily replace an initial draft's absent tag with an
  // internal name. Recover only our bot-authored, explicitly bound initial draft.
  if (!/^untagged-[0-9a-f]{20}$/.test(release.tag_name ?? '') || release.draft !== true
      || release.name !== 'Nightly' || release.prerelease !== true
      || release.author?.login !== 'github-actions[bot]' || release.author?.type !== 'Bot'
      || !Number.isSafeInteger(release.id) || release.id <= 0 || typeof release.body !== 'string') return false;
  const markers = [...release.body.matchAll(NIGHTLY)];
  const transitions = [...release.body.matchAll(TRANSITION)];
  if (markers.length !== 1 || transitions.length !== 1
      || release.body.split('<!-- galaxy-nightly:').length !== 2
      || release.body.split('<!-- galaxy-nightly-transition:').length !== 2
      || release.body.includes('<!-- galaxy-release:')) return false;
  const [, commit, build] = markers[0];
  const [, id, prior, priorBuild, oldRef, candidate] = transitions[0];
  return release.target_commitish === commit && BUILD.exec(build)?.[1] === commit && candidate === build
    && (id === 'new' || id === String(release.id)) && prior === 'none' && priorBuild === 'none' && oldRef === 'none';
}

function ownedStablePlaceholder(release, version) {
  if (!VERSION.test(version) || !/^untagged-[0-9a-f]{20}$/.test(release.tag_name ?? '')
      || release.draft !== true || release.prerelease !== false || release.name !== `Galaxy ${version}`
      || release.author?.login !== 'github-actions[bot]' || release.author?.type !== 'Bot'
      || !Number.isSafeInteger(release.id) || release.id <= 0 || typeof release.body !== 'string'
      || release.body.includes('<!-- galaxy-nightly:') || release.body.includes('<!-- galaxy-nightly-transition:')) return false;
  const markers = [...release.body.matchAll(/<!-- galaxy-release:v1 tag=([0-9.]+) commit=([0-9a-f]{40}) -->/g)];
  return markers.length === 1 && release.body.split('<!-- galaxy-release:').length === 2
    && markers[0][1] === version && release.target_commitish === markers[0][2];
}

function nightlyState(release, ref) {
  if ((release.tag_name !== 'nightly' && !ownedNightlyPlaceholder(release)) || release.name !== 'Nightly' || !release.prerelease
      || !Number.isSafeInteger(release.id) || release.id <= 0) throw new Error('Existing Nightly is not an owned rolling record.');
  const markers = [...(release.body ?? '').matchAll(NIGHTLY)];
  const transitions = [...(release.body ?? '').matchAll(TRANSITION)];
  if (markers.length !== 1 || transitions.length > 1 || BUILD.exec(markers[0][2])?.[1] !== markers[0][1]) throw new Error('Nightly build identity is invalid.');
  const [, commit, build] = markers[0];
  let previous = commit;
  if (transitions.length) {
    const [, id, prior, priorBuild, oldRef, candidate] = transitions[0];
    if (candidate !== build || (prior === 'none') !== (priorBuild === 'none')
        || (priorBuild !== 'none' && BUILD.exec(priorBuild)?.[1] !== prior)
        || (id !== 'new' && id !== String(release.id))) throw new Error('Nightly transition identity is invalid.');
    if (id === 'new' && (!release.draft || prior !== 'none' || oldRef !== 'none')) throw new Error('Only an initial Nightly draft may have a new-record marker.');
    if (![oldRef === 'none' ? null : oldRef, commit].includes(ref)) throw new Error('Nightly ref changed outside its recorded transition.');
    if (id !== 'new') {
      const names = new Set([`Galaxy_nightly_${build}_aarch64.dmg`, `Galaxy_nightly_${build}_x86_64-setup.exe`,
        `Galaxy_nightly_${build}_amd64.deb`, `Galaxy_nightly_${build}_linux_x86_64.tar.gz`, `SHA256SUMS_nightly_${build}`]);
      for (const asset of release.assets ?? []) {
        if (names.has(asset.name) && Number.isSafeInteger(asset.size) && asset.size > 0
            && asset.size <= (asset.name.startsWith('SHA256SUMS_') ? 8192 : 2 * 1024 ** 3)) names.delete(asset.name);
      }
      if (names.size) throw new Error('Pending Nightly transition is missing its candidate downloads; existing data was left intact.');
    }
    if (release.draft) previous = prior === 'none' ? '' : prior;
  } else if (release.draft || ref !== commit) throw new Error('Nightly record and source ref do not agree.');
  return { commit, build, previous, pending: Boolean(release.draft || transitions.length) };
}

async function main() {
  if (process.env.GITHUB_SERVER_URL !== 'https://github.com' || process.env.GITHUB_REPOSITORY !== 'rkw6086/galaxy'
      || (process.env.GITHUB_API_URL && process.env.GITHUB_API_URL !== 'https://api.github.com')) throw new Error('Use the Galaxy GitHub repository.');
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const eventName = process.env.GITHUB_EVENT_NAME;
  const ref = process.env.GITHUB_REF;
  let channel;
  if (eventName === 'pull_request' || (eventName === 'push' && ref === 'refs/heads/master')) channel = 'ci';
  else if (['schedule', 'workflow_dispatch'].includes(eventName) && ref === 'refs/heads/master') channel = 'nightly';
  else if (eventName === 'push' && ref?.startsWith('refs/tags/') && !event.deleted && !/^0+$/.test(event.after ?? '')) channel = 'release';
  else throw new Error('Unsupported build event or ref.');
  let version = checkVersions();
  const sha = git('rev-parse', 'HEAD');
  if (!SHA.test(sha) || git('status', '--porcelain', '--untracked-files=all')) throw new Error('Build preparation requires a clean source checkout.');
  if (channel === 'ci' && process.env.GITHUB_SHA !== sha) throw new Error('CI checkout differs from the triggering commit.');
  const token = process.env.GITHUB_TOKEN;
  const api = async (suffix, missing = false) => {
    const headers = { Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2026-03-10' };
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(`https://api.github.com/repos/rkw6086/galaxy${suffix}`, {
      headers, redirect: 'error', signal: AbortSignal.timeout(30_000),
    });
    if (response.status === 404 && missing) return null;
    if (!response.ok) throw new Error(`Build preflight API returned HTTP ${response.status}.`);
    return response.json();
  };
  let releaseHistory;
  const allReleases = async () => {
    if (!token || /\s/.test(token)) throw new Error('Release lookup requires authenticated GitHub access.');
    if (releaseHistory) return releaseHistory;
    const releases = [];
    for (let page = 1; page <= 100; page += 1) {
      const values = await api(`/releases?per_page=100&page=${page}`);
      if (!Array.isArray(values) || values.some((value) => !value || typeof value !== 'object' || Array.isArray(value))) throw new Error('Cannot inspect release history.');
      releases.push(...values);
      if (values.length < 100) {
        releaseHistory = releases;
        return releaseHistory;
      }
    }
    throw new Error('Release history exceeds its safe bound.');
  };
  const releaseForTag = async (tag) => {
    // The by-tag endpoint excludes unpublished drafts. Authenticated listings
    // include them for the trusted publication job's push-capable token.
    const belongs = (release) => release.tag_name === tag
      || (tag === 'nightly' ? ownedNightlyPlaceholder(release) : ownedStablePlaceholder(release, tag));
    const matches = (await allReleases()).filter(belongs);
    if (matches.length > 1) throw new Error('Multiple releases claim the requested tag; inspect them before continuing.');
    if (!matches.length) return null;
    const id = matches[0].id;
    if (!Number.isSafeInteger(id) || id <= 0) throw new Error('Release lookup returned an invalid record identity.');
    const current = await api(`/releases/${id}`);
    if (current?.id !== id || current.tag_name !== matches[0].tag_name || !belongs(current)) throw new Error('Release identity changed during lookup.');
    return current;
  };
  const tagObject = async (tag, missing = false) => {
    const value = await api(`/git/ref/tags/${encodeURIComponent(tag)}`, missing);
    if (!value) return null;
    if (value.ref !== `refs/tags/${tag}`) throw new Error('Unexpected remote ref identity.');
    return value.object;
  };
  const tagCommit = async (tag) => {
    let object = await tagObject(tag);
    for (let depth = 0; depth < 8 && object?.type === 'tag'; depth += 1) object = (await api(`/git/tags/${object.sha}`)).object;
    if (object?.type !== 'commit' || !SHA.test(object.sha)) throw new Error('Tag does not identify a bounded commit object.');
    return object.sha;
  };
  let nightlyId = '';
  let previous = '';
  let previousBuild = '';
  let previousId = '';
  let previousRef = '';
  let pending = false;
  let baseline = '';
  let notes = '';
  if (channel !== 'ci') {
    if (!token || /\s/.test(token)) throw new Error('Missing GitHub workflow token.');
    const primaryRef = channel === 'nightly' ? 'refs/heads/master' : ref;
    const rows = git('-c', 'credential.helper=', 'ls-remote', 'https://git.ryserve.net/public/galaxy.git', primaryRef, `${primaryRef}^{}`)
      .split('\n').filter(Boolean).map((line) => line.split('\t'));
    if ((rows.find((row) => row[1] === `${primaryRef}^{}`)?.[0] ?? rows.find((row) => row[1] === primaryRef)?.[0]) !== sha) {
      throw new Error('Gitea primary differs from this source; wait for repository sync before signing.');
    }
    if (channel === 'nightly') {
      if ((await api('/branches/master')).commit?.sha !== sha || git('rev-parse', 'refs/remotes/origin/master') !== sha) throw new Error('Nightly checkout must match current master.');
      const run = process.env.GITHUB_RUN_ID;
      const attempt = process.env.GITHUB_RUN_ATTEMPT;
      if (!/^[1-9][0-9]{0,18}$/.test(run ?? '') || !/^[1-9][0-9]{0,8}$/.test(attempt ?? '')) throw new Error('Missing Nightly run identity.');
      nightlyId = `${sha}_${run}-${attempt}`;
      const current = await releaseForTag('nightly');
      const object = await tagObject('nightly', true);
      if (object && (object.type !== 'commit' || !SHA.test(object.sha))) throw new Error('Nightly must use its owned lightweight source ref.');
      previousRef = object?.sha ?? '';
      if (current) {
        const state = nightlyState(current, object?.sha ?? null);
        previous = state.commit;
        previousBuild = state.build;
        previousId = String(current.id);
        pending = state.pending;
        baseline = state.previous;
      } else if (object) throw new Error('An unowned Nightly ref exists without its rolling release; left unchanged.');
      // Every scheduled/manual run produces a distinct candidate, even when
      // master has not changed since the last completed Nightly.
    } else {
      version = ref.slice('refs/tags/'.length);
      if (!VERSION.test(version) || compareVersions(version, checkVersions()) < 0) throw new Error('Use a bare, nondecreasing stable version tag.');
      if (git('rev-parse', `${ref}^{commit}`) !== sha || await tagCommit(version) !== sha
          || git('rev-parse', `${process.env.GITHUB_SHA}^{commit}`) !== sha) throw new Error('Release tag or source changed.');
      git('merge-base', '--is-ancestor', sha, 'refs/remotes/origin/master');
      const existing = await releaseForTag(version);
      if (existing && !existing.draft) {
        fs.appendFileSync(process.env.GITHUB_OUTPUT, 'build=false\nchannel=release\n');
        console.log(`Galaxy ${version} is already published; its files remain unchanged.`);
        return;
      }
      try { notes = changelog(version); } catch { /* Bounded commit notes below. */ }
    }
    const releases = await allReleases();
    const stable = releases.filter((release) => !release.draft && !release.prerelease && VERSION.test(release.tag_name ?? ''))
      .sort((a, b) => compareVersions(b.tag_name, a.tag_name));
    const previousStable = channel === 'release'
      ? stable.find((release) => compareVersions(release.tag_name, version) < 0)
      : stable[0];
    if (!baseline) baseline = previousStable?.body?.match(/<!-- galaxy-release:v1 tag=[0-9.]+ commit=([0-9a-f]{40}) -->/)?.[1] ?? '';
    if (!notes) {
      let range = sha;
      if (baseline) {
        try { git('merge-base', '--is-ancestor', baseline, sha); range = `${baseline}..${sha}`; } catch { baseline = ''; }
      }
      if (baseline === sha) notes = 'Rebuilt the same source as the previous Nightly; no source changes.';
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
    fs.appendFileSync(process.env.GITHUB_OUTPUT, `notes_file=${file}\n`);
    fs.appendFileSync(process.env.GITHUB_ENV, `GALAXY_RELEASE_NOTES_FILE=${file}\n`);
  }
  fs.appendFileSync(process.env.GITHUB_OUTPUT,
    `build=true\nchannel=${channel}\nversion=${version}\nsha=${sha}\nnightly_id=${nightlyId}\nprevious_nightly_sha=${previous}\nprevious_nightly_build=${previousBuild}\nprevious_nightly_release_id=${previousId}\nprevious_nightly_ref=${previousRef}\nprevious_nightly_pending=${pending}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV,
    `GALAXY_RELEASE_VERSION=${version}\nGALAXY_RELEASE_SHA=${sha}\nGALAXY_NIGHTLY_ID=${nightlyId}\n`);
  console.log(`Validated Galaxy ${channel} ${version} at ${sha}.`);
}

main().catch((error) => { console.error(error.message); process.exitCode = 1; });
