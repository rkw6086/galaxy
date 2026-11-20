import { fileURLToPath } from 'node:url';
import path from 'node:path';

const SHA = /^[0-9a-f]{40}$/;
const POSITIVE = /^[1-9][0-9]{0,18}$/;
const ATTEMPT = /^[1-9][0-9]{0,8}$/;
const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;

export function releaseTimestamp(value) {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(value ?? '')
      || new Date(value).toISOString() !== value.replace('Z', '.000Z')) throw new Error('Invalid prepared release timestamp.');
  return value;
}

export function nightlyArtifactIdentity(timestamp, runNumber, attempt) {
  releaseTimestamp(timestamp);
  if (!POSITIVE.test(String(runNumber)) || !ATTEMPT.test(String(attempt))) throw new Error('Invalid prepared workflow identity.');
  const parts = Object.fromEntries(new Intl.DateTimeFormat('en-US', {
    timeZone: 'America/Chicago', year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23',
  }).formatToParts(new Date(timestamp)).map(({ type, value }) => [type, value]));
  return `Nightly_${parts.month}${parts.day}${parts.year}_${parts.hour}${parts.minute}${parts.second}_CT_r${runNumber}-${attempt}`;
}

export function prepareReleaseIdentity(run, env, sha, { workflow = '.github/workflows/build.yml', channel = 'nightly', version } = {}) {
  if (!SHA.test(sha) || !POSITIVE.test(env.GITHUB_RUN_ID ?? '') || !ATTEMPT.test(env.GITHUB_RUN_ATTEMPT ?? '')
      || String(run.id) !== env.GITHUB_RUN_ID || run.head_sha !== sha || run.path !== workflow
      || String(run.run_number) !== env.GITHUB_RUN_NUMBER || run.event !== env.GITHUB_EVENT_NAME
      || run.repository?.full_name !== 'rkw6086/galaxy') throw new Error('Workflow identity differs from the prepared source.');
  const timestamp = releaseTimestamp(run.created_at);
  if (channel === 'release' && !VERSION.test(version ?? '')) throw new Error('Invalid stable artifact identity.');
  return { timestamp, identity: channel === 'nightly'
    ? nightlyArtifactIdentity(timestamp, run.run_number, env.GITHUB_RUN_ATTEMPT) : version };
}

export function validateBuildIdentity(env) {
  const channel = env.GALAXY_BUILD_CHANNEL;
  const sha = env.GALAXY_RELEASE_SHA;
  if (!SHA.test(sha ?? '')) throw new Error('Invalid build source identity.');
  if (channel === 'nightly') {
    const match = new RegExp(`^${sha}_([1-9][0-9]{0,18})-([1-9][0-9]{0,8})$`).exec(env.GALAXY_NIGHTLY_ID ?? '');
    if (!match || match[1] !== env.GITHUB_RUN_ID || !ATTEMPT.test(env.GITHUB_RUN_ATTEMPT ?? '')
        || BigInt(match[2]) > BigInt(env.GITHUB_RUN_ATTEMPT)
        || env.GALAXY_ARTIFACT_IDENTITY !== nightlyArtifactIdentity(env.GALAXY_RELEASE_TIMESTAMP, env.GITHUB_RUN_NUMBER, match[2])) {
      throw new Error('Readable Nightly identity differs from its prepared build.');
    }
  } else if (channel === 'release') {
    if (env.GALAXY_UNSIGNED_PREFLIGHT === '1' && env.GALAXY_ARTIFACT_IDENTITY === `ci_${sha}`) return;
    if (env.GALAXY_ARTIFACT_IDENTITY !== env.GALAXY_RELEASE_VERSION) throw new Error('Stable artifact identity differs from its version.');
    if (env.GALAXY_RELEASE_TIMESTAMP) releaseTimestamp(env.GALAXY_RELEASE_TIMESTAMP);
  } else if (channel !== 'ci' || env.GALAXY_ARTIFACT_IDENTITY !== `ci_${sha}`) throw new Error('Invalid artifact identity.');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { validateBuildIdentity(process.env); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
