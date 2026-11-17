#!/usr/bin/env node
// Explicitly scoped recovery of the original, unpublished Galaxy 3.1.0 source.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { checkVersions, changelog } from '../release.mjs';

export const RECOVERY_VERSION = '3.1.0';
export const RECOVERY_SOURCE_SHA = '64466167bde523e40f2b4807bd11947718a21a69';
export const recipeRoot = fs.realpathSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..'));
const allowedChanges = new Set([
  '.gitea/workflows/macos-release-recovery.yml',
  'script/ci/prepare-release-recovery.mjs',
  'script/ci/recovery-source.mjs',
  'script/ci/build-macos.sh',
  'script/ci/package-macos.sh',
  'script/ci/publish-release.py',
  'docs/release-macos.md',
]);

export function git(root, ...args) {
  return execFileSync('git', args, { cwd: root, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }).trim();
}

export function recoveryPaths(env = process.env) {
  if (!/^\d+-\d*$/.test(env.GALAXY_JOB_ID ?? '') || !path.isAbsolute(env.RUNNER_TEMP ?? '')) {
    throw new Error('Recovery requires a valid Actions job identity and absolute temporary directory.');
  }
  const temporary = fs.realpathSync(env.RUNNER_TEMP);
  return {
    sourceRoot: path.join(temporary, `galaxy-source-${env.GALAXY_JOB_ID}`),
    manifestPath: path.join(temporary, `galaxy-recovery-${env.GALAXY_JOB_ID}.json`),
  };
}

export function validateRecipe(env = process.env) {
  if (env.GITHUB_EVENT_NAME !== 'workflow_dispatch' || env.GITHUB_REF !== 'refs/heads/master') {
    throw new Error('Release recovery is allowed only through the manual workflow on master.');
  }
  if (!/^\d+$/.test(env.GITHUB_RUN_ID ?? '') || env.GALAXY_JOB_ID !== `${env.GITHUB_RUN_ID}-${env.GITHUB_RUN_ATTEMPT ?? ''}`) {
    throw new Error('Recovery job identity does not match this Actions run.');
  }
  if (env.GITEA_SERVER_URL !== 'https://git.ryserve.net' || env.GITEA_REPOSITORY !== 'public/galaxy') {
    throw new Error('This recovery is restricted to public/galaxy on its configured Gitea server.');
  }
  const recipeSha = git(recipeRoot, 'rev-parse', 'HEAD');
  if (!/^[0-9a-f]{40}$/.test(recipeSha) || env.GITHUB_SHA !== recipeSha) {
    throw new Error('Recipe checkout must match the actual manual workflow commit.');
  }
  if (fs.realpathSync(git(recipeRoot, 'rev-parse', '--show-toplevel')) !== recipeRoot || git(recipeRoot, 'status', '--porcelain')) {
    throw new Error('Recovery requires a clean, complete recipe checkout.');
  }
  if (git(recipeRoot, 'cat-file', '-t', `refs/tags/${RECOVERY_VERSION}`) !== 'tag'
      || git(recipeRoot, 'rev-parse', `refs/tags/${RECOVERY_VERSION}^{commit}`) !== RECOVERY_SOURCE_SHA) {
    throw new Error('The original annotated 3.1.0 tag is missing or changed.');
  }
  git(recipeRoot, 'merge-base', '--is-ancestor', RECOVERY_SOURCE_SHA, recipeSha);
  git(recipeRoot, 'merge-base', '--is-ancestor', recipeSha, 'refs/remotes/origin/master');
  const changes = git(recipeRoot, 'diff', '--name-only', '--no-renames', '-z', RECOVERY_SOURCE_SHA, recipeSha)
    .split('\0').filter(Boolean);
  for (const changed of changes) {
    if (!allowedChanges.has(changed)) throw new Error(`Recovery cannot change application or build inputs: ${changed}`);
    const entry = git(recipeRoot, 'ls-tree', recipeSha, '--', changed);
    if (!/^(100644|100755) blob [0-9a-f]{40}\t/.test(entry)) {
      throw new Error(`Recovery recipe must remain a regular tracked file: ${changed}`);
    }
  }
  checkVersions({ repo: recipeRoot, expected: RECOVERY_VERSION });
  changelog(RECOVERY_VERSION, { repo: recipeRoot });
  return { recipeSha, changes, ...recoveryPaths(env) };
}

export function validRunUrl(value) {
  try {
    const url = new URL(value);
    return url.origin === 'https://git.ryserve.net' && !url.username && !url.password && !url.search && !url.hash
      && /^\/public\/galaxy\/actions\/runs\/\d+$/.test(url.pathname);
  } catch { return false; }
}

export function validateRecovery(env = process.env) {
  const context = validateRecipe(env);
  if (env.GALAXY_RELEASE_RECOVERY_MANIFEST !== context.manifestPath) {
    throw new Error('Recovery manifest is not the exact file owned by this Actions job.');
  }
  const stat = fs.lstatSync(context.manifestPath);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 16384 || (stat.mode & 0o077) !== 0) {
    throw new Error('Recovery manifest must be a small private regular file.');
  }
  const manifest = JSON.parse(fs.readFileSync(context.manifestPath, 'utf8'));
  if (manifest.schema !== 1 || manifest.version !== RECOVERY_VERSION || manifest.sourceSha !== RECOVERY_SOURCE_SHA
      || manifest.recipeSha !== context.recipeSha || manifest.recipeRoot !== recipeRoot || manifest.sourceRoot !== context.sourceRoot
      || manifest.jobId !== env.GALAXY_JOB_ID || manifest.runId !== env.GITHUB_RUN_ID
      || manifest.runAttempt !== (env.GITHUB_RUN_ATTEMPT ?? '') || !validRunUrl(manifest.runUrl)
      || JSON.stringify(manifest.changes) !== JSON.stringify(context.changes)
      || env.GALAXY_RELEASE_VERSION !== RECOVERY_VERSION || env.GALAXY_RELEASE_SHA !== RECOVERY_SOURCE_SHA
      || env.RELEASE_TAG !== RECOVERY_VERSION || env.GALAXY_RELEASE_RECIPE_SHA !== context.recipeSha
      || env.GALAXY_RELEASE_RUN_URL !== manifest.runUrl) {
    throw new Error('Recovery identity does not match its validated source, recipe, version, or Actions run.');
  }
  if (fs.lstatSync(context.sourceRoot).isSymbolicLink() || fs.realpathSync(context.sourceRoot) !== context.sourceRoot
      || fs.realpathSync(git(context.sourceRoot, 'rev-parse', '--show-toplevel')) !== context.sourceRoot
      || git(context.sourceRoot, 'rev-parse', 'HEAD') !== RECOVERY_SOURCE_SHA
      || git(context.sourceRoot, 'status', '--porcelain')) {
    throw new Error('Recovery source must be the clean, original tagged worktree.');
  }
  const head = fs.readFileSync(path.join(git(context.sourceRoot, 'rev-parse', '--absolute-git-dir'), 'HEAD'), 'utf8').trim();
  if (head !== RECOVERY_SOURCE_SHA) throw new Error('Recovery source must have a detached HEAD.');
  checkVersions({ repo: context.sourceRoot, expected: RECOVERY_VERSION });
  changelog(RECOVERY_VERSION, { repo: context.sourceRoot });
  return manifest;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const manifest = validateRecovery();
    if (process.argv[2] === '--provenance' && process.argv.length === 4) {
      const requested = path.resolve(process.argv[3]);
      const destination = path.join(fs.realpathSync(path.dirname(requested)), path.basename(requested));
      const expected = path.join(fs.realpathSync(process.env.GALAXY_RELEASE_DIR), 'release-provenance.json');
      if (destination !== expected) throw new Error('Provenance must be written into this job\'s verified artifact directory.');
      fs.writeFileSync(destination, `${JSON.stringify({ schema: 1, version: manifest.version,
        sourceSha: manifest.sourceSha, recipeSha: manifest.recipeSha, runUrl: manifest.runUrl }, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
    } else if (process.argv.length === 2) {
      console.log(manifest.sourceRoot);
    } else throw new Error('Use recovery-source.mjs [--provenance PATH].');
  } catch (error) {
    console.error(`Galaxy release recovery validation failed: ${error.message}`);
    process.exitCode = 1;
  }
}
