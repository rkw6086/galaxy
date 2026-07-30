---
name: bring-warp-feature-over
description: Find and selectively port explicitly requested upstream Warp pull requests into Galaxy. Accepts PR/MR numbers or a user-provided feature description, confirms the selected PRs before editing, filters out Warp-only service code, preserves Galaxy's Bedrock/OpenAI/ACP provider architecture and branding, and adapts eligible local changes.
---

# Bring a Specific Warp Feature into Galaxy

Use this skill to identify and port selected upstream Warp functionality into Galaxy without performing a broad upstream synchronization.

## Required scope

The user must provide either:

1. One or more upstream Warp pull request numbers, or
2. A description of the feature they want to find upstream.

Examples:

- `Bring over Warp PR #12345`
- `Port PRs 12345 and 12389`
- `Find the Warp PR that added terminal search highlighting`
- `Bring over the recent worktree picker improvements`

If the user provides a description, search for candidate PRs and ask the user to confirm the matching PR number(s) before making code changes. Do not scan and port arbitrary recent history.

## Non-negotiable Galaxy boundaries

Galaxy-specific architecture always wins. Never replace or bypass:

- Bedrock provider code in `app/src/ai/bedrock/`
- OpenAI/LiteLLM provider code in `app/src/ai/openai/`
- ACP provider/runtime code in `app/src/ai/acp/` and the related ACP crate(s)
- Provider dispatch and response streaming in `app/src/ai/provider/` and `app/src/ai/blocklist/controller/response_stream.rs`
- Galaxy branding, package names, channels, settings, deployment, and installer behavior

Do not import or preserve new upstream code that depends on:

- Warp's proprietary AI API or hosted agent backend
- Warp authentication, billing, subscriptions, teams, or cloud-only flows
- Warp telemetry/analytics that phone home to Warp
- Warp server GraphQL/API endpoints unless explicitly adapted to an existing Galaxy service
- Warp-specific deployment or branding
- Oz/hosted orchestration that Galaxy cannot run locally

## Phase 0: Inspect the working tree

Before making changes:

```bash
git --no-optional-locks status --short --branch
```

Do not overwrite unrelated user work. If the working tree is dirty, keep existing changes intact and use a separate branch where possible.

## Phase 1: Resolve the requested scope

### If PR/MR numbers were provided

Use the supplied numbers directly. Do not search unrelated upstream history.

### If a feature description was provided

Search upstream GitHub for matching Warp PRs. Prefer the GitHub search API, using a concise query derived from the user’s description:

```bash
curl -sS --get 'https://api.github.com/search/issues' \
  --data-urlencode 'q=<keywords> repo:warpdotdev/warp is:pr' \
  --data-urlencode 'per_page=10'
```

If authentication or API rate limits prevent the search, use the public GitHub web search URL or fetch recent commit/PR metadata as a fallback. Keep the search bounded by the user’s description; do not enumerate the entire repository history.

For each candidate, collect:

- PR number and title
- State and merge status
- Updated/merged date
- Body excerpt
- Labels, when available
- Changed-file summary

Inspect candidate files before presenting them:

```bash
curl -sS "https://api.github.com/repos/warpdotdev/warp/pulls/<PR>"
curl -sS "https://api.github.com/repos/warpdotdev/warp/pulls/<PR>/files?per_page=100"
```

Present a short ranked list with the reason each candidate matches and its likely adaptation cost. Then ask for confirmation, for example:

```text
I found these likely matches:

1. #12345 — Improve terminal search highlighting
   Matches the requested behavior; mostly local terminal/UI code. Low adaptation cost.

2. #12389 — Add cloud search synchronization
   Related wording, but depends on Warp cloud APIs. Likely ineligible.

Which PR(s) should I port? Reply with the number(s), or say “none”.
```

Do not create a branch, apply patches, or edit source files until the user confirms the selected PR number(s). Research and recommendation are allowed before confirmation.

If the search returns no confident match, say so and ask for a PR link, more keywords, or clarification. Do not guess.

## Phase 2: Fetch and inspect only the confirmed PRs

After the user confirms PR numbers, fetch upstream refs if needed:

```bash
git fetch warp master
```

For each confirmed PR, inspect:

- PR title, state, merge commit, and description
- Files changed and additions/deletions
- New dependencies, feature flags, migrations, or generated files
- Whether the change is local-only or depends on Warp services
- Whether Galaxy has renamed or diverged from the affected paths

For local context:

```bash
git log --oneline --all -- <path>
git diff HEAD...warp/master -- <path>
```

## Phase 3: Eligibility decision

Classify each confirmed PR as one of:

### Eligible: direct port

Usually local terminal, editor, UI, completion, parser, filesystem, SSH, theme, or platform bug fixes with limited dependencies.

### Eligible: adapted port

Useful functionality that touches Galaxy provider or branding boundaries but can be redirected safely to Bedrock, OpenAI/LiteLLM, or ACP. Document the adaptation before editing.

### Ineligible

Reject the PR, or only extract isolated local hunks, when it fundamentally depends on Warp-only services such as hosted AI, Warp auth, billing, telemetry, server GraphQL, Warp Drive infrastructure, or Oz orchestration.

When a PR mixes eligible and ineligible changes, do not cherry-pick the whole PR. Port only the eligible files or hunks manually.

Summarize the decision before implementation. If the user asked to implement and the PR is clearly eligible, proceed after the PR confirmation. If the PR has meaningful adaptation risk, explain the risk and ask before making substantial changes.

## Phase 4: Create a focused working branch

Before source changes:

```bash
git switch -c port-warp-pr-<PR>
```

For multiple PRs, use a descriptive branch containing all numbers, or apply them sequentially on one focused branch.

Do not commit unless the user explicitly requests it.

## Phase 5: Apply the smallest safe change

Prefer these strategies in order:

1. Cherry-pick without committing only when the PR is narrowly scoped and service-free:
   ```bash
   git cherry-pick -n <merge-commit-or-commit>
   ```
2. Apply only eligible paths:
   ```bash
   git diff <base> <commit> -- <eligible-paths> | git apply --3way
   ```
3. Manually port the relevant hunks when Galaxy renamed paths, has diverged, or the PR mixes service and local changes.

Do not use a blanket `git merge warp/master` for this skill.

Preserve Galaxy naming and adapt upstream imports where necessary:

- `warpui` → `galaxyui`
- `warpui_core` → `galaxyui_core`
- `warp_core` → `galaxy_core`
- `warp_terminal` → `galaxy_terminal`
- `warp_editor` → `galaxy_editor`

Do not perform repository-wide renames as part of a feature port.

## Provider-specific adaptation rules

### Bedrock and OpenAI/LiteLLM

A PR that adds AI behavior must use Galaxy’s provider dispatch. Adapt request/response types to the existing provider interfaces rather than importing Warp’s AI client or endpoint.

### ACP

ACP changes are allowed only when they preserve Galaxy’s ACP runtime, transport, launch, and model-selection architecture. Do not replace ACP with Warp’s hosted agent service or introduce a Warp-specific backend. Inspect relevant files under `app/src/ai/acp/` and the ACP crate before applying upstream changes.

### Shared agent/blocklist code

Treat `app/src/ai/agent/` and `app/src/ai/blocklist/` as manual-merge areas. Keep Galaxy’s provider selection, tool handling, persistence, and ACP integration intact even when adopting local UX or controller improvements.

## API and service leak checks

Review new code before accepting it:

```bash
grep -RInE 'api\.warp\.dev|warp\.dev/v1|WarpAIService|WarpAiClient|WARP_API_KEY|WARP_AUTH_TOKEN' app/src crates --include='*.rs'
```

New hits introduced by the port must be removed or adapted. Do not mechanically remove existing compatibility names without checking their purpose.

## Dependencies and generated files

For each new dependency:

1. Check whether Galaxy already has an equivalent.
2. Add the smallest local/client-side dependency needed.
3. Reject dependencies that exist only for Warp services, hosted AI, auth, telemetry, or billing.
4. Regenerate code only when the PR genuinely changes a Galaxy-supported schema or generated source.

## Validation

Run focused checks based on changed files:

```bash
git diff --check
cargo fmt --all -- --check
```

Typical targeted checks include:

```bash
cargo check -p galaxy_terminal
cargo check -p galaxyui
cargo check -p galaxy_editor
cargo check -p galaxy
```

Also check the ACP crate or affected package when ACP files change. Use the actual package names from the relevant `Cargo.toml` files.

For a release-ready port, run:

```bash
./script/format
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
cargo build
```

Fix errors caused by the port, but do not pull in excluded Warp service code to make the build pass.

## Output report

Report:

- Search description, candidate PRs, and the user’s confirmed selection
- Eligibility decision for each selected PR
- Files and hunks ported
- Bedrock/OpenAI/ACP adaptations made, if any
- Warp-only changes intentionally skipped
- Dependencies or generated files changed
- Validation commands and results
- Any follow-up work or manual testing needed

If a requested PR is too intertwined with Warp's service architecture, recommend a smaller follow-up port or explain which local hunks can be extracted safely.
