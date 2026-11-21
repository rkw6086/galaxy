# Gitea repository synchronization

Gitea at <https://git.ryserve.net/public/galaxy> is the primary development repo.
Two jobs run on Gitea: **Sync GitHub** for
<https://github.com/rkw6086/galaxy>, and **Sync Samsung** for the Samsung GitLab
project `samnasbo/shared/galaxy`. GitHub runs all native build and release jobs.

Developers push only to the local `origin` remote, which points at Gitea. Release
tags are pushed only to `origin` too. The server jobs handle the other services.
They run after primary `master` pushes, every five minutes, and on manual dispatch.
Changes imported from one peer reach the other through Gitea on its next pass.

## Branch behavior

- A branch present on only one side is created on the other.
- Identical tips require no push.
- When one side is ahead, the other is fast-forwarded.
- When both sides have related new commits, a clean Git merge preserves both
  histories, with Gitea as the first parent. It is pushed to Gitea, then the peer.
- Conflicting content or unrelated histories remain intact for maintainer review.
  Other branches can proceed. Connection or initial-fetch races defer the pass.

The jobs never force-push branches, resolve conflicting content automatically,
or execute incoming source. They use temporary bare repositories and ordinary
Git push guards. Equal commits produce no writes, so reflected pushes stop.
The single-capacity Gitea Linux runner serializes the jobs.

Deletion is manual across all three services: a surviving branch would otherwise
be restored by the next sync. Pause both jobs before coordinated branch retirement.
The initial Samsung-only old branches were retained as private Samsung archive
tags before retirement. The performance work is also retained under
`archive/improve-agent-performance-20261119`.

Repository collaborators on either peer can change the primary source through
sync. Fork pull requests remain proposals until merged. Gitea job-token imports
do not start other push workflows; scheduled sync and GitHub Nightly builds still
use current `master`.

## Tags, large files and automation

Immutable version tags are created on Gitea and copied outward. Existing mismatched
tags are reported and never replaced. Tags found only on a peer are not imported,
so Samsung's legacy and archive tags remain there. Rolling `nightly`, legacy
`beta`, and temporary staging tags are excluded; each release service owns them.

Before publishing a changed ref, the job fetches all reachable Git LFS objects
from its source and uploads them to the destination. A clean merge fetches from
both parents. Failed LFS transfers prevent ref publication. Unchanged passes
perform no repository-object or LFS downloads. Git and Git LFS must be installed
on the Gitea runner.

Git synchronization does not copy issues, pull requests, reviews, release records,
downloads, workflow history or secrets. Current installers are at
<https://github.com/rkw6086/galaxy/releases>. GitHub builds Windows, Linux and Apple
Silicon macOS; Samsung-bound mirror pushes include `ci.skip`.

Galaxy's build workflow is `.github/workflows/build.yml`. Repository synchronization
is under `.gitea/workflows/`. Inherited Warp GitHub automation and retired Gitea
macOS build definitions are retained in their respective archive directories.

## Credentials and controls

Each peer uses its own project-scoped write deploy key, held only in Gitea:

| Job | Repository Actions secret | Additional control |
| --- | --- | --- |
| Sync GitHub | `GALAXY_GITHUB_SYNC_KEY` | GitHub key allows writes, including workflow files |
| Sync Samsung | `GALAXY_SAMSUNG_SYNC_KEY` | `GALAXY_SAMSUNG_SYNC_ENABLED=true` repository variable |

The short-lived Gitea job token grants `code: write` for the primary repo. Private
keys exist in job-owned temporary files only for synchronization and are removed
on exit. Personal account keys and API tokens are not stored in these jobs.
GitHub CLI authorizations used to create deploy keys need the `workflow` scope
when workflow files are synchronized.

Host keys are pinned in `script/ci/github-known-hosts` and
`script/ci/gitlab-known-hosts`. Verify official announced rotations before updating
them. A host-key mismatch fails the job.

For read-only inspection with the corresponding credentials configured:

```bash
python3 script/ci/sync-repositories.py --peer github --dry-run
python3 script/ci/sync-repositories.py --peer samsung --dry-run
```

These fetch history when necessary and report proposed operations without pushing.
