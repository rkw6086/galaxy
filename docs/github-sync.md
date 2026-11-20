# Gitea and GitHub synchronization

Gitea at <https://git.ryserve.net/public/galaxy> is Galaxy's primary development
service. <https://github.com/rkw6086/galaxy> is the public GitHub copy and runs
Galaxy's native build and release jobs.
The **Sync GitHub** job runs entirely on Gitea, after pushes to `master`, every
five minutes, and on manual dispatch. The periodic run discovers changes on
every branch on either service. No GitHub runner or GitHub Actions secret is
needed.

## Branch behavior

- A branch present on only one service is created on the other.
- Identical tips require no push.
- When one branch is ahead, the other is fast-forwarded.
- When both sides have new commits, a clean Git merge is committed by
  `Galaxy Repository Sync` and pushed to Gitea first, then GitHub.
- Merge conflicts, unrelated histories, protected-branch rejections, and
  concurrent incompatible changes are reported as job failures. Other branches
  can still synchronize. Conflicting branches remain available on both services
  for a maintainer to resolve.

A connection failure or a ref changing during the initial fetch stops that pass;
the next scheduled job reads fresh snapshots and retries.

The job never force-pushes, automatically resolves conflicting content, or
executes code from an incoming branch. It uses a temporary bare repository and
compares both histories before pushing. A repeated run sees matching commits
and does nothing, so pushes reflected back from GitHub do not form a loop. The
single-capacity Gitea Linux runner serializes these jobs; ordinary Git push
checks also protect against simultaneous human pushes.

Branch deletion is deliberately manual on both services. Deleting a branch on
only one side causes the next sync to restore it from the other. Rename a branch
by creating the new name and removing the old name on both services while the
sync workflow is paused. Changes from GitHub become Gitea changes: repository
collaborators and branch protection should reflect who may edit the project.
Fork pull requests remain proposals until a maintainer merges them.

Gitea does not trigger other push workflows for a push made with an Actions job
token. Imports therefore do not start another sync or immediate Release checks;
run Release checks manually when needed. Scheduled builds continue to use the
current `master`.

## Tags, releases and automation

Version tags are created on Gitea. The job copies immutable tags to GitHub
when missing and reports mismatched tags without replacing them. Tags created
only on GitHub are not imported. Rolling `beta` and temporary `beta-stage-*`
tags are not synchronized because each release service owns their lifecycle.
GitHub owns new multi-platform release downloads; legacy Gitea releases remain
available separately.

Git synchronization copies commits, branches, tags, and files, including the
Gitea workflow definitions. Before publishing a changed ref, the job fetches
every Git LFS object reachable from its history and uploads it to the destination.
Clean merges fetch the LFS objects from both parents before either ref is pushed.
A failed LFS transfer prevents that ref from being published. Unchanged runs
perform no LFS transfers. The Linux runner must provide Git and Git LFS.

Synchronization does not copy issues, pull requests, reviews,
release records, downloadable assets, workflow history, or secrets. Current
downloads are at <https://github.com/rkw6086/galaxy/releases>. Contributors
can open pull requests on GitHub or Gitea; merging a GitHub pull
request updates its target branch and the next Gitea sync imports the result.

Inherited Warp GitHub workflows have been archived under
`.github/upstream-workflows/`. Galaxy's build workflow is
`.github/workflows/build.yml`; repository synchronization stays under
`.gitea/workflows/`. Existing historical tags may still contain the old
workflows; their history is preserved. The retired performance branch is
recoverable through tag `archive/improve-agent-performance-20261119`.

## Authentication and operation

1. Add a dedicated Ed25519 public key to the GitHub repository's **Settings →
   Deploy keys**, with **Allow write access** enabled.
   When registering it through GitHub CLI, the login also needs `workflow`
   permission to copy branches or historical tags containing GitHub workflow
   files. Use `gh auth refresh --hostname github.com --scopes workflow` if GitHub
   rejects those files. This does not move the sync job to GitHub.
2. Save its private key as the **Gitea repository Actions secret**
   `GALAXY_GITHUB_SYNC_KEY`. Do not put it in Git or use a personal account key.
3. The job's short-lived Gitea token has `code: write` for this repository. The
   GitHub key is scoped to this repository, is installed only for the sync step,
   and is removed from the job's temporary filesystem on exit.
4. Run **Sync GitHub** manually and inspect its per-branch result. A normal
   unchanged run prints its no-change summary without fetching repository
   objects or pushing.

GitHub's published SSH host keys are pinned in `script/ci/github-known-hosts`.
Update them from <https://api.github.com/meta> after verifying an announced
rotation. A host-key mismatch fails the job rather than accepting a new key.

For local read-only inspection, with the same SSH identity and Gitea token
configured, run `python3 script/ci/sync-github.py --dry-run`. It fetches history
when necessary and reports proposed operations without publishing them.
