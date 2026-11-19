#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { checkVersions } from '../release.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

function main() {
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const sha = process.env.GITHUB_SHA;
  if (process.env.GITHUB_EVENT_NAME !== 'push' || process.env.GITHUB_REF !== 'refs/heads/master'
      || event.ref !== 'refs/heads/master' || event.deleted === true
      || !/^[0-9a-f]{40}$/.test(sha ?? '') || /^0+$/.test(sha) || event.after !== sha) {
    throw new Error('Beta signing requires the exact commit from a non-deleted push to master.');
  }
  if (!process.env.GITHUB_REPOSITORY || event.repository?.full_name !== process.env.GITHUB_REPOSITORY) {
    throw new Error('The beta push does not belong to this workflow repository.');
  }
  const git = (...args) => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim();
  if (git('rev-parse', 'HEAD') !== sha || git('status', '--porcelain')) {
    throw new Error('Beta builds require a clean checkout of the triggering master commit.');
  }
  // Newer master pushes may be queued while this commit is building. Validate
  // ancestry rather than silently substituting the newest commit for this run.
  git('merge-base', '--is-ancestor', sha, 'refs/remotes/origin/master');
  const version = checkVersions();
  fs.appendFileSync(process.env.GITHUB_OUTPUT, `version=${version}\nsha=${sha}\n`);
  fs.appendFileSync(process.env.GITHUB_ENV,
    `GALAXY_BUILD_CHANNEL=beta\nGALAXY_RELEASE_VERSION=${version}\nGALAXY_RELEASE_SHA=${sha}\nGALAXY_BETA_ID=${sha}\n`);
  console.log(`Validated Galaxy beta at ${sha} (package version ${version}).`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(); } catch (error) { console.error(error.message); process.exitCode = 1; }
}
