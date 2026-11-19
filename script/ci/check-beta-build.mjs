#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { checkVersions } from '../release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const COMMIT = /^[0-9a-f]{40}$/;
const MARKER = /<!-- galaxy-beta:v1 commit=([0-9a-f]{40})(?: [^>\r\n]*)? -->/;

async function main() {
  const eventName = process.env.GITHUB_EVENT_NAME;
  if (!['schedule', 'workflow_dispatch'].includes(eventName)
      || process.env.GITHUB_REF !== 'refs/heads/master') {
    throw new Error('Beta signing requires a scheduled or manual run on master.');
  }
  const repository = process.env.GITEA_REPOSITORY;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? '')
      || repository.split('/').some((part) => part === '.' || part === '..')
      || process.env.GITHUB_REPOSITORY !== repository) {
    throw new Error('Invalid beta workflow repository.');
  }
  const server = new URL(process.env.GITEA_SERVER_URL);
  if (server.protocol !== 'https:' || server.username || server.password
      || server.search || server.hash || server.pathname !== '/') {
    throw new Error('Gitea must use an HTTPS server URL without credentials or a path.');
  }
  const token = process.env.GITEA_TOKEN;
  if (!token || /\s/.test(token)) throw new Error('Missing workflow token.');
  const api = async (suffix, allowMissing = false) => {
    const response = await fetch(new URL(`/api/v1/repos/${repository}${suffix}`, server), {
      headers: { Authorization: `token ${token}`, Accept: 'application/json' },
      redirect: 'error', signal: AbortSignal.timeout(30_000),
    });
    if (response.status === 404 && allowMissing) return null;
    if (!response.ok) throw new Error(`Beta preflight API returned HTTP ${response.status}.`);
    return response.json();
  };
  const git = (...args) => execFileSync('git', args, {
    cwd: root, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024,
  }).trim();
  const sha = git('rev-parse', 'HEAD');
  if (!COMMIT.test(sha) || git('status', '--porcelain', '--untracked-files=all')
      || git('rev-parse', 'refs/remotes/origin/master') !== sha) {
    throw new Error('Beta builds require a clean checkout of master.');
  }
  // Gitea schedules can retain the workflow's older context SHA. The workflow
  // explicitly checks out master, and its live API head is the authority here.
  const master = await api('/branches/master');
  if (master.commit?.id !== sha) throw new Error('Master advanced during checkout; rerun the beta workflow.');

  const current = await api('/releases/tags/beta', true);
  let previous = '';
  if (current) {
    previous = current.body?.match(MARKER)?.[1] ?? '';
    if (!previous || current.tag_name !== 'beta' || current.draft || !current.prerelease) {
      throw new Error('The existing beta does not match Galaxy’s rolling release; left unchanged.');
    }
  }
  const version = checkVersions();
  const expectedNames = new Set([`Galaxy_beta_${sha}_aarch64.dmg`, 'SHA256SUMS']);
  const complete = current?.assets?.length === 2 && current.assets.every((asset) =>
    expectedNames.delete(asset.name) && Number.isSafeInteger(asset.size) && asset.size > 0
    && asset.size <= (asset.name === 'SHA256SUMS' ? 4096 : 512 * 1024 * 1024))
    && expectedNames.size === 0;
  if (eventName === 'schedule' && previous === sha && complete) {
    const tag = await api('/tags/beta', true);
    if (tag?.commit?.sha === sha && current.target_commitish === sha) {
      fs.appendFileSync(process.env.GITHUB_OUTPUT, 'build=false\n');
      console.log(`Master remains at ${sha}; the published beta is current. Nothing to build.`);
      return;
    }
  }

  // Migrate the previous per-push beta without filling the first rolling
  // release's notes with unrelated repository history. Its retained artifact
  // carries the exact source SHA and links to a completed native beta run.
  let notesBaseline = previous;
  if (!notesBaseline) {
    const artifacts = [];
    for (let page = 1; page <= 100; page += 1) {
      const data = await api(`/actions/artifacts?limit=50&page=${page}`);
      if (!Array.isArray(data.artifacts)) throw new Error('Cannot inspect previous beta artifacts.');
      artifacts.push(...data.artifacts);
      if (data.artifacts.length < 50) break;
      if (page === 100) throw new Error('Previous beta artifact history exceeds its safe bound.');
    }
    artifacts.sort((left, right) => right.id - left.id);
    for (const artifact of artifacts) {
      const match = artifact.name?.match(/^Galaxy-beta-([0-9a-f]{40})-[0-9]+-macos-arm64$/);
      if (!match || artifact.expired || !Number.isSafeInteger(artifact.workflow_run?.id)) continue;
      const run = await api(`/actions/runs/${artifact.workflow_run.id}`);
      if (run.status === 'completed' && run.conclusion === 'success' && run.head_sha === match[1]
          && /(?:^|\/)macos-beta.yml@refs\/heads\/master$/.test(run.path ?? '')) {
        notesBaseline = match[1];
        break;
      }
    }
  }

  const temporary = process.env.RUNNER_TEMP;
  const job = process.env.GALAXY_JOB_ID;
  if (!temporary || !path.isAbsolute(temporary) || !/^[0-9]+-[0-9]+$/.test(job ?? '')) {
    throw new Error('Missing job-owned temporary directory or job identity.');
  }
  const notesDirectory = path.join(temporary, `galaxy-notes-${job}`);
  fs.mkdirSync(notesDirectory, { mode: 0o700 });
  let notes;
  if (notesBaseline === sha) {
    notes = eventName === 'workflow_dispatch'
      ? 'Manually rebuilt master with no source changes since the previous beta.'
      : 'No source changes since the previous beta build; publishing the initial rolling beta.';
  } else {
    let range = sha;
    let introduction = 'Initial rolling beta from master.';
    if (notesBaseline) {
      // The baseline is the published beta, never an attempted or failed build.
      let ancestor = false;
      try { git('merge-base', '--is-ancestor', notesBaseline, sha); ancestor = true; } catch { /* Explicit history fallback below. */ }
      if (ancestor) {
        range = `${notesBaseline}..${sha}`;
        introduction = `Changes since the previous beta (\`${notesBaseline.slice(0, 12)}\`):`;
      } else {
        introduction = 'Master history changed since the previous beta. Recent commits:';
      }
    }
    const commits = git('log', '--format=%h %s', '--max-count=201', range, '--').split('\n').filter(Boolean);
    const lines = [];
    let bytes = 0;
    for (const line of commits.slice(0, 200)) {
      const bullet = `- ${line.replace(/[\x00-\x1f\x7f]/g, ' ').slice(0, 500)}`;
      bytes += Buffer.byteLength(bullet, 'utf8') + 1;
      if (bytes > 96 * 1024) break;
      lines.push(bullet);
    }
    notes = `${introduction}\n\n${lines.join('\n') || '- No source changes.'}`;
    if (commits.length > lines.length) notes += `\n\nShowing the latest ${lines.length} commits within the release-note limit.`;
    if (notesBaseline) notes += `\n\n[Compare changes](${server.origin}/${repository}/compare/${notesBaseline}...${sha})`;
  }
  const notesFile = path.join(notesDirectory, 'release-notes.md');
  fs.writeFileSync(notesFile, `${notes}\n`, { mode: 0o600, flag: 'wx' });
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `build=true\nversion=${version}\nsha=${sha}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV,
    `GALAXY_BUILD_CHANNEL=beta\nGALAXY_RELEASE_VERSION=${version}\nGALAXY_RELEASE_SHA=${sha}\nGALAXY_BETA_ID=${sha}\n`
    + `GALAXY_PREVIOUS_BETA_SHA=${previous}\nGALAXY_RELEASE_NOTES_FILE=${notesFile}\n`);
  console.log(`Validated Galaxy beta at ${sha}; previous published beta: ${previous || 'none'}.`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => { console.error(error.message); process.exitCode = 1; });
}
