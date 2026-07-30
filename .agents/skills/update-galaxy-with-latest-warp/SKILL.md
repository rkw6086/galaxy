---
name: update-galaxy-with-latest-warp
description: Selectively synchronize Galaxy with upstream Warp by updating Rust dependencies and porting eligible terminal, UI, editor, and local core improvements. Do not perform a blanket merge or import Warp API, proprietary AI, cloud auth, telemetry, or billing code.
---

# Selectively Update Galaxy from Warp

Galaxy is a fork of Warp, but it intentionally does not use Warp's proprietary service architecture. The default update strategy is therefore **not** `git merge warp/master`. It is a controlled, selective synchronization focused on code that can run locally in Galaxy.

## Non-negotiable exclusions

Never import or reintroduce upstream code that depends on:

- Warp's proprietary AI API, agent backend, or hosted orchestration
- Warp authentication, subscriptions, billing, teams, or cloud-only flows
- Warp telemetry or analytics that phone home to Warp
- Warp server GraphQL/API endpoints, unless the change is explicitly adapted to an existing Galaxy service
- Warp-specific deployment, channel, branding, or package identity
- Any change that would replace or bypass Galaxy's Bedrock/OpenAI provider dispatch

Galaxy's AI provider implementation remains authoritative in:

- `app/src/ai/bedrock/`
- `app/src/ai/openai/`
- `app/src/ai/provider/`
- `app/src/ai/blocklist/controller/response_stream.rs`

## Phase 0: Inspect the working tree

Before changing anything:

```bash
git --no-optional-locks status --short --branch
```

Do not overwrite or reset user changes. If the worktree contains unrelated modifications, keep them intact and make the update in a separate branch or ask the user before proceeding.

## Phase 1: Update Rust dependencies separately

A dependency update is independent from synchronizing upstream source code. Run it from the Galaxy repository root:

```bash
cargo update
```

Then inspect the dependency diff:

```bash
git diff -- Cargo.lock
cargo check --workspace
```

If `cargo update` causes unrelated or excessive churn, do not blindly keep it. Prefer targeted updates for a specific dependency:

```bash
cargo update -p <package>
```

If the lockfile update is useful but a package causes breakage, revert only that package's update or restore the lockfile and update dependencies incrementally. Never use a lockfile regeneration as a substitute for source synchronization.

`cargo update` updates crates.io/git dependency resolution; it does **not** bring terminal or UI source changes from Warp. Those require the selective workflow below.

## Phase 2: Fetch and inventory upstream changes

The `warp` remote may be configured, but fetching is read-only with respect to Galaxy's branches:

```bash
git fetch warp master
git log --oneline -30 warp/master
```

Compare upstream with the Galaxy base without merging:

```bash
git diff --stat HEAD...warp/master
git diff --name-status HEAD...warp/master
```

Classify candidate changes by path and commit. Good candidates generally include:

- `app/src/terminal/` and terminal model/emulation code, excluding agent/provider integrations
- `crates/galaxy_terminal/` or the corresponding upstream terminal crate
- `crates/galaxyui/`, `crates/galaxyui_core/`, and UI components
- `crates/editor/`, `crates/sum_tree/`, completers, parsers, and local utilities
- Local-only bug fixes and platform behavior fixes

Reject candidates that touch or depend on:

- Warp AI/server/auth/telemetry/billing modules
- Warp-specific GraphQL/API schema or cloud synchronization
- Galaxy identity, channels, settings, deployment, or Bedrock/OpenAI files
- Broad refactors whose dependency surface cannot be isolated safely

For a candidate commit, inspect before applying it:

```bash
git show --stat --summary <commit>
git show --format=fuller --find-renames <commit> -- <path>
```

## Phase 3: Apply only selected changes

Create a working branch before porting source changes:

```bash
git switch -c update-from-warp-$(date +%Y%m%d)
```

Prefer, in order:

1. A focused upstream commit with a small, eligible file set:
   ```bash
   git cherry-pick -n <commit>
   ```
2. A file- or hunk-level patch:
   ```bash
   git diff <base> <commit> -- <eligible-paths> | git apply --3way
   ```
3. A manual port when Galaxy renamed paths or diverged substantially.

Do **not** cherry-pick a commit merely because it includes one useful terminal fix. If a commit mixes terminal code with Warp API/AI/cloud changes, extract only the eligible hunks or manually port the local change.

After each selected change:

```bash
git diff --check
cargo check -p galaxy_terminal
cargo check -p galaxyui
cargo check -p galaxy_editor
```

Use the actual package name from the relevant `Cargo.toml` if it differs. For changes affecting the application, also run:

```bash
cargo check -p galaxy
```

## Path and naming adaptation

Upstream may still use Warp names while Galaxy has renamed crates and paths. Adapt imports to Galaxy's existing names rather than introducing new aliases or reverting Galaxy's naming:

- `warpui` → `galaxyui`
- `warpui_core` → `galaxyui_core`
- `warp_core` → `galaxy_core`
- `warp_terminal` → `galaxy_terminal`
- `warp_editor` → `galaxy_editor`

Preserve existing Galaxy aliases only where the codebase already requires them. Do not perform a repository-wide rename as part of an update.

## Dependency policy

When a selected upstream change needs a new dependency:

1. Check whether an equivalent dependency already exists in the Galaxy workspace.
2. Add only the smallest required dependency or feature.
3. Confirm it is local/client-side and not a Warp service crate.
4. Run the narrowest affected `cargo check`.

Do not add dependencies for Warp's proprietary APIs, cloud auth, telemetry, billing, or hosted AI.

## AI and service boundary review

Before accepting a patch that touches shared app or agent code, inspect all new imports and calls. It must route AI requests through Galaxy's provider dispatch. Reject or adapt code containing concepts such as:

- `api.warp.dev`, `warp.dev/v1`, Warp AI clients, hosted agent endpoints
- Warp auth tokens or Warp account/session APIs
- Warp telemetry/analytics senders
- Warp billing/subscription/team APIs
- Hosted orchestration/Oz dependencies

Useful checks:

```bash
grep -RInE 'api\.warp\.dev|warp\.dev/v1|WarpAIService|WarpAiClient|WARP_API_KEY|WARP_AUTH_TOKEN' app/src crates --include='*.rs'
```

Existing intentional compatibility names or comments should be reviewed rather than mechanically deleted. New hits introduced by the update must be removed or adapted.

## Phase 4: Validation

Run focused checks first, then broader validation as appropriate:

```bash
cargo fmt --all -- --check
git diff --check
cargo check -p galaxy
cargo check -p galaxy_terminal
cargo check -p galaxyui
cargo check -p galaxy_editor
```

For a release-ready update, run the repository-required checks:

```bash
./script/format
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
cargo build
```

If the build fails, fix only errors caused by the selected update. Do not solve incompatibilities by importing excluded Warp service code or weakening Galaxy's provider boundaries.

## Reporting

Report separately:

- Dependencies updated by `cargo update`, including notable lockfile changes
- Upstream commits or patches selectively ported
- Files and functionality intentionally skipped because they were Warp-specific
- Validation commands run and their results
- Any candidate changes that need a future manual port

Do not commit, push, or merge into `master` unless the user explicitly requests it.

## Recommended fallback

If upstream has accumulated a large architectural delta, stop doing a broad synchronization. Use the `bring-warp-feature-over` skill to identify and migrate individual eligible features. That workflow is safer for Galaxy than attempting to reconcile all of Warp's unrelated service and AI changes at once.
