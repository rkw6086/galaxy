#!/usr/bin/env node
import fs from 'node:fs';
import { checkHistory } from './check-release-tag.mjs';
import { RECOVERY_VERSION, RECOVERY_SOURCE_SHA, recipeRoot, git, validateRecipe, validateRecovery, validRunUrl } from './recovery-source.mjs';

async function main() {
  const context = validateRecipe();
  const token = process.env.GITEA_TOKEN;
  if (!token) throw new Error('Missing recovery preflight token.');
  const api = async (suffix, missing = false) => {
    const response = await fetch(`https://git.ryserve.net/api/v1/repos/public/galaxy${suffix}`, {
      headers: { Authorization: `token ${token}`, Accept: 'application/json' },
      redirect: 'error', signal: AbortSignal.timeout(30_000),
    });
    if (missing && response.status === 404) return null;
    if (!response.ok) throw new Error(`Recovery preflight API returned HTTP ${response.status}.`);
    return response.json();
  };
  const tag = await api(`/tags/${RECOVERY_VERSION}`);
  if (tag.name !== RECOVERY_VERSION || tag.commit?.sha !== RECOVERY_SOURCE_SHA) {
    throw new Error('The remote release tag no longer identifies the original 3.1.0 source.');
  }
  for (let page = 1; page <= 100; page += 1) {
    const data = await api(`/actions/workflows/macos-release.yml/runs?limit=100&page=${page}`);
    const runs = Array.isArray(data) ? data : (data.workflow_runs ?? data.runs);
    if (!Array.isArray(runs)) throw new Error('Cannot verify original release tag history.');
    checkHistory(runs, RECOVERY_VERSION, RECOVERY_SOURCE_SHA);
    if (runs.length < 100) break;
    if (page === 100) throw new Error('Release history exceeds the safe recovery limit.');
  }
  const current = await api(`/actions/runs/${process.env.GITHUB_RUN_ID}`);
  if (current.head_sha !== context.recipeSha || current.event !== 'workflow_dispatch' || !validRunUrl(current.html_url)
      || !String(current.path).endsWith('macos-release-recovery.yml@refs/heads/master')) {
    throw new Error('Cannot bind recovery to this trusted master workflow run.');
  }
  const release = await api(`/releases/tags/${RECOVERY_VERSION}`, true);
  if (release) {
    const marker = `<!-- galaxy-release:v1 tag=${RECOVERY_VERSION} commit=${RECOVERY_SOURCE_SHA} -->`;
    const recipeMarker = `<!-- galaxy-recipe:v1 commit=${context.recipeSha} -->`;
    if (!release.draft || release.target_commitish !== RECOVERY_SOURCE_SHA || !release.body?.includes(marker)
        || !release.body?.includes(recipeMarker) || !release.body?.includes(`](${current.html_url})`)) {
      throw new Error('An existing release cannot be replaced by recovery; retry its original publication job.');
    }
  }
  if (fs.existsSync(context.manifestPath) || fs.existsSync(context.sourceRoot)) throw new Error('Recovery job paths already exist.');
  git(recipeRoot, 'worktree', 'add', '--detach', context.sourceRoot, RECOVERY_SOURCE_SHA);
  const manifest = { schema: 1, version: RECOVERY_VERSION, sourceSha: RECOVERY_SOURCE_SHA,
    recipeSha: context.recipeSha, recipeRoot, sourceRoot: context.sourceRoot,
    jobId: process.env.GALAXY_JOB_ID, runId: process.env.GITHUB_RUN_ID,
    runAttempt: process.env.GITHUB_RUN_ATTEMPT ?? '', runUrl: current.html_url, changes: context.changes };
  fs.writeFileSync(context.manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  const values = { GALAXY_RELEASE_RECOVERY_MANIFEST: context.manifestPath,
    GALAXY_RELEASE_VERSION: RECOVERY_VERSION, GALAXY_RELEASE_SHA: RECOVERY_SOURCE_SHA,
    RELEASE_TAG: RECOVERY_VERSION, GALAXY_RELEASE_RECIPE_SHA: context.recipeSha,
    GALAXY_RELEASE_RUN_URL: current.html_url };
  Object.assign(process.env, values);
  validateRecovery();
  for (const [name, value] of Object.entries(values)) {
    if (/[\r\n]/.test(value)) throw new Error('Recovery outputs cannot contain newlines.');
    fs.appendFileSync(process.env.GITHUB_ENV, `${name}=${value}\n`);
  }
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `version=${RECOVERY_VERSION}\nsha=${RECOVERY_SOURCE_SHA}\nrecipe_sha=${context.recipeSha}\nrun_url=${current.html_url}\n`);
  console.log(`Validated Galaxy ${RECOVERY_VERSION} source ${RECOVERY_SOURCE_SHA}; packaging recipe ${context.recipeSha}.`);
}

main().catch((error) => { console.error(`Galaxy recovery preflight failed: ${error.message}`); process.exitCode = 1; });
