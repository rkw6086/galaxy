---
name: update-galaxy-with-latest-warp
description: Audit and almost-autonomously synchronize recent upstream Warp client changes into Galaxy. Use for broad/latest upstream refreshes of terminal, UI, editor, platform, and local core code; use bring-warp-feature-over for a specific PR or named feature. Never perform a blanket merge or import Warp services, proprietary AI, auth, telemetry, billing, or branding.
---

# Update Galaxy from Recent Warp Client Work

Bring eligible upstream client improvements into Galaxy with minimal user intervention. Cargo dependency updates and upstream source synchronization are separate operations; neither substitutes for the other.

## Autonomy contract

When the user asks to update Galaxy with recent/latest Warp work, perform the complete workflow by default: discover, classify, select, port, adapt, and validate. Do not stop for routine confirmations about clearly eligible local client changes.

Proceed autonomously with:

- read-only upstream discovery and fetching;
- creating a focused local branch or isolated worktree;
- selecting clearly local terminal, UI, editor, parser, completion, filesystem, SSH, theme, and platform fixes;
- mechanically adapting Galaxy crate names and branding;
- extracting eligible hunks from mixed commits;
- adding or updating focused tests;
- targeted dependency updates required by an accepted change;
- formatting and validation allowed by the repository's current `AGENTS.md`.

Pause only when a candidate requires a real product or architecture decision, such as:

- replacing or materially changing Galaxy's provider/runtime lifecycle;
- introducing a new external service, credential flow, persistent data migration, or security-sensitive capability;
- choosing between multiple materially different user experiences;
- resolving an overlap with unrelated user changes that cannot be isolated safely;
- accepting a broad upstream refactor whose local-client behavior cannot be separated confidently.

Skip clearly ineligible changes without asking and report why. Never merge upstream wholesale. Never commit, push, open a PR, or update `master` unless the user explicitly requests that action.

## Load current repository guidance

Before discovery, read the repository's current `AGENTS.md`. Its architecture, testing, and push rules supersede command examples in this skill. Before classifying candidates, read [references/eligibility.md](references/eligibility.md).

If selected work changes UI code, also load and follow `warp-ui-guidelines` before editing. Use other repository skills when their trigger conditions apply.

## 1. Inspect and isolate

Start with:

```bash
git --no-optional-locks status --short --branch
git remote -v
```

Preserve all user changes. A dirty worktree is not automatically a blocker:

- Continue in place when upstream work cannot overlap the user's files.
- Otherwise create a focused branch plus an isolated worktree from the current `HEAD`.
- Ask only if the requested result must include overlapping uncommitted work and safe isolation is impossible.

Unless the user selected a branch, create a unique branch such as `update-from-warp-YYYYMMDD-<upstream-short-sha>` before source edits. Do not switch branches when doing so would disturb user changes; use a worktree instead.

## 2. Resolve and fetch upstream without configuration ceremony

Prefer an existing `warp` remote. If it is absent, do not ask the user to configure one. Resolve the default branch from `https://github.com/warpdotdev/Warp.git` (or `GALAXY_WARP_UPSTREAM_URL` when set) and fetch it directly into a namespaced remote-tracking ref such as `refs/remotes/galaxy-warp-upstream/<branch>`. Do not add or rewrite Git remotes merely to perform the fetch.

Determine the upstream default branch dynamically; do not assume `main` or `master`. Fetching must not modify Galaxy branches.

Use `.galaxy/warp-upstream-last-reviewed` as the local, gitignored review cursor. A valid cursor must be an ancestor of the fetched upstream head. If it is missing or invalid:

1. Try the merge base between Galaxy `HEAD` and the fetched upstream head.
2. Bound first-run discovery to recent history (normally the last 90 days and at most 200 commits).
3. Treat older history as the explicit bootstrap baseline and disclose that choice in the report.

For later runs, inspect the cursor-to-head range. If more than 200 commits accumulated, process a bounded recent batch, state what remains, and do not chase an unbounded backlog in one run.

Do not advance the cursor past an unresolved candidate. Advance it only through commits that were ported, intentionally skipped, already present, or conclusively classified as ineligible.

## 3. Inventory and classify recent commits

Inventory commits and changed paths before applying anything:

```bash
git log --reverse --format='%H%x09%cs%x09%s' <base>..<upstream-ref>
git show --stat --summary <commit>
git show --format=fuller --find-renames <commit> -- <relevant-paths>
```

Use `git cherry` or patch comparison to identify changes already present in equivalent form, but do not rely on patch IDs alone because Galaxy often adapts upstream code.

Classify every candidate as:

- **Auto-port:** clearly eligible and locally scoped.
- **Auto-adapt:** useful, with small mechanical Galaxy naming or local-architecture changes.
- **Extract:** mixed commit; apply only eligible paths or hunks.
- **Already present/superseded:** Galaxy already contains the behavior or a stronger fix.
- **Skip:** Warp service, cloud, proprietary AI, telemetry, auth, billing, branding, or otherwise irrelevant.
- **Hold:** requires one of the genuine decisions listed in the autonomy contract.

Proceed with Auto-port, Auto-adapt, and Extract candidates without an approval round. Collect Hold candidates into one concise checkpoint rather than interrupting repeatedly.

## 4. Apply the smallest safe changes

Prefer, in order:

1. `git cherry-pick -n <commit>` only for narrow, service-free commits.
2. Apply eligible paths or hunks with a three-way patch.
3. Manually port when Galaxy renamed paths, diverged, or has stronger local invariants.

Never solve a conflict by taking an entire upstream file over Galaxy's version. Preserve Galaxy comments and unrelated local behavior.

Adapt upstream crate names to current Galaxy names without repository-wide renames:

- `warpui` -> `galaxyui`
- `warpui_core` -> `galaxyui_core`
- `warp_core` -> `galaxy_core`
- `warp_terminal` -> `galaxy_terminal`
- `warp_editor` -> `galaxy_editor`

Preserve Galaxy branding, local storage semantics, provider selection, and settings defaults. New user-facing settings must follow the repository's current command-palette and discoverability rules.

After each logical batch, inspect the diff and run `git diff --check`. Run focused tests/checks only when permitted by the current `AGENTS.md`; fix regressions before taking another batch so failures remain attributable.

## 5. Handle dependencies deliberately

Do not run blanket `cargo update` as part of a normal upstream source sync.

When an accepted change needs a dependency update:

1. Reuse an equivalent workspace dependency when possible.
2. Prefer `cargo update -p <package>` for lockfile-only movement.
3. Update an exact Git `rev` explicitly when the manifest pins one; `cargo update` will not choose a different revision for it.
4. Add only the smallest local/client-side dependency or feature required.
5. Inspect `Cargo.toml` and `Cargo.lock` diffs separately from the source port.

Run broad dependency upgrades only when the user explicitly asks for dependency maintenance. Never introduce dependencies whose purpose is Warp services, hosted AI, auth, telemetry, billing, or cloud-only behavior.

## 6. Validate in the repository's current order

Use changed paths to choose focused tests and package checks. The current `AGENTS.md` controls whether interactive verification must happen before formatting or static checks.

Before a requested commit or push:

1. Run the formatter required by `AGENTS.md`.
2. Read `script/presubmit` and run its current Clippy commands exactly; do not hard-code obsolete `--all-features` behavior.
3. Run targeted tests for the changed subsystem, plus broader checks proportional to the port.
4. For interactive UI/terminal fixes, launch the exact development binary specified by `AGENTS.md` and leave it running when user verification is required.

Fix only failures caused by the port. Do not import excluded Warp infrastructure or weaken Galaxy invariants to make validation pass.

## 7. Finish the review cursor and report

After the processed range is fully classified, update `.galaxy/warp-upstream-last-reviewed` to the last contiguous reviewed upstream commit. This local cursor is operational state, not a source change.

Report:

- upstream URL, branch, reviewed range, and ending SHA;
- commits auto-ported, adapted, or partially extracted;
- already-present and skipped commits, grouped by reason;
- Hold candidates and the single decision needed for each;
- dependency and generated-file changes;
- validation commands and results;
- remaining unreviewed range, if the bounded batch did not reach upstream head;
- current branch/worktree and whether commit/push authorization is still needed.

For a specifically requested PR or named Warp feature, use `bring-warp-feature-over` instead of scanning recent history.
