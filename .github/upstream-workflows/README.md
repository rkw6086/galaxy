# Archived upstream automation

These are inherited Warp workflows, retained for reference. GitHub does not run
workflows in this directory. They refer to Warp infrastructure, repositories,
runners and secrets and must be adapted before use with Galaxy.

Galaxy builds, signs and publishes on Gitea using `.gitea/workflows/`. The
GitHub repository receives Git branches and stable release tags through the
Gitea sync job described in `docs/github-sync.md`.
