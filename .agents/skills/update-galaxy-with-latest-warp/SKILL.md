---
name: update-galaxy-with-latest-warp
description: Merge the latest changes from upstream Warp into Galaxy, preserving Galaxy's identity, branding, and AI provider architecture (Bedrock / OpenAI-LiteLLM only). This skill fetches the latest Warp master, merges it in, resolves conflicts in favor of Galaxy's customizations, strips any Warp API/AI/cloud code, and then iteratively repairs the build until `cargo build` succeeds cleanly.
---

# Update Galaxy with Latest Warp

This skill merges the latest upstream Warp changes into Galaxy while preserving everything that makes Galaxy what it is. Galaxy is a fork of Warp that:

- Uses **Amazon Bedrock** and/or **OpenAI/LiteLLM** as its AI providers — NEVER Warp's proprietary AI API
- Has its own branding (Galaxy, not Warp) in user-facing surfaces
- Does NOT use Warp's cloud authentication, telemetry, or billing
- Maintains its own deployment pipeline (Hermes)
- Keeps all Galaxy-specific features, settings, and customizations intact

---

## Phase 1: Fetch Latest Warp

The `warp` remote is already configured in this repository pointing to `git@github.com:warpdotdev/warp.git`.

```bash
git fetch warp master
```

Verify the fetch succeeded and note the latest commit:

```bash
git log --oneline -1 warp/master
```

---

## Phase 2: Create a Working Branch

Create a dedicated branch for the merge work:

```bash
git checkout -b update-from-warp-$(date +%Y%m%d) master
```

This ensures master stays clean until we have a working build.

---

## Phase 3: Merge Warp into Galaxy

Perform the merge, expecting conflicts:

```bash
git merge warp/master --no-commit --no-ff
```

Using `--no-commit` so we can inspect and fix everything before committing.

---

## Phase 4: Resolve Conflicts — Galaxy Always Wins on Identity

When resolving merge conflicts, follow these **non-negotiable rules**:

### 4a. Files Where Galaxy ALWAYS Wins (keep ours)

For these files/patterns, always take Galaxy's version (`--ours`):

- `app/Cargo.toml` — Galaxy's version, package name, binary targets
- `Cargo.lock` — Will be regenerated anyway
- `AGENTS.md` / `CLAUDE.md` — Galaxy's agent instructions
- `.agents/` — Galaxy's skill definitions
- `script/build-and-deploy-hermes*` — Galaxy's deploy pipeline
- `script/install-galaxy.sh` — Galaxy's installer
- `app/channels/` — Galaxy's channel configurations and icons
- Any file under `app/src/ai/bedrock/` — Galaxy's Bedrock provider (keep ours)
- Any file under `app/src/ai/openai/` — Galaxy's OpenAI/LiteLLM provider (keep ours)
- Any file under `app/src/ai/provider/` — Galaxy's provider dispatch (keep ours)
- `app/src/ai/llms.rs` — Galaxy's model registry (keep ours)
- `app/src/settings/ai.rs` — Galaxy's AI settings (keep ours)
- `app/src/ai/blocklist/controller/response_stream.rs` — Galaxy's provider resolution (keep ours)
- Files with Samsung/Galaxy branding customizations

To resolve these in bulk:
```bash
git checkout --ours <file_path>
git add <file_path>
```

### 4b. Files Where Warp Wins (take theirs)

For pure infrastructure/terminal/UI improvements that don't touch AI or branding:

- `crates/galaxyui/` (formerly `warpui`) — Take Warp's UI improvements, then rename
- `crates/galaxyui_core/` — Same
- `app/src/terminal/` — Terminal emulation improvements (EXCEPT `app/src/terminal/input/agent.rs`)
- `crates/editor/` — Editor improvements
- `crates/sum_tree/` — Data structure improvements
- Pure algorithm / utility crates

For these:
```bash
git checkout --theirs <file_path>
git add <file_path>
```

### 4c. Files That Need Manual Merge

These require reading both versions and combining:

- `app/src/ai/agent/` — Take Warp's agent logic improvements BUT ensure they route through Galaxy's provider dispatch, not Warp's API
- `app/src/ai/blocklist/` — Similar: take improvements but keep Galaxy's provider architecture
- `app/src/workspace/` — Take improvements but keep Galaxy branding
- `app/src/settings_view/` — Take UI improvements but keep Galaxy's AI settings pages
- Root `Cargo.toml` — Merge new dependencies from Warp but keep Galaxy's workspace metadata

### 4d. Files/Directories to DELETE if Warp Adds Them

If the merge introduces any of these, remove them:

- Any Warp-proprietary AI client (e.g. `app/src/ai/warp_api/`, `app/src/ai/warp_server/`)
- Warp authentication modules that phone home to `api.warp.dev`
- Warp telemetry/analytics senders
- Warp billing/subscription code
- Any new GraphQL queries targeting Warp's server for AI (model listing from Warp's API, etc.)

```bash
git rm -r <unwanted_path>
```

### 4e. Naming Fixups After Merge

After resolving conflicts, some Warp naming may have leaked in from theirs-wins files. Do a sweep:

```bash
# Check for Warp API endpoints that should not exist
grep -rn "api\.warp\.dev" app/ crates/ --include="*.rs"
grep -rn "warp\.dev/api" app/ crates/ --include="*.rs"

# Check for Warp AI service calls
grep -rn "WarpAIService\|warp_ai_service\|WarpAiClient" app/ crates/ --include="*.rs"
```

Fix any hits — either remove the code or replace with Galaxy equivalents.

---

## Phase 5: Regenerate Cargo.lock

After all conflict resolution:

```bash
cargo generate-lockfile
```

Or if that fails due to errors, just delete and let the build recreate it:

```bash
rm Cargo.lock
cargo metadata --format-version 1 > /dev/null 2>&1 || true
```

---

## Phase 6: Build Repair Loop

This is the critical phase. **Keep iterating until `cargo build` succeeds.**

### Strategy

Run the build and fix errors one category at a time:

```bash
cargo build 2>&1 | head -100
```

### Common Error Categories and Fixes

**1. Missing modules / unresolved imports:**
- Warp may have added new modules. Check if they're AI/cloud related → delete them.
- If they're legitimate (terminal, UI, utilities) → keep them but ensure they compile.
- If they reference renamed crates (`warpui` vs `galaxyui`) → fix the import paths.

**2. Type mismatches in AI code:**
- Warp may have changed AI types/traits. Galaxy's AI architecture takes priority.
- If Warp added new fields to shared types used by both AI and non-AI code, add the fields but make them optional or provide Galaxy-appropriate defaults.

**3. Missing crate features:**
- New Warp code may need features not enabled in Galaxy's `Cargo.toml`.
- Add the features if they're for legitimate crates. Do NOT add features that enable Warp-proprietary functionality.

**4. Renamed/moved items:**
- Warp may have refactored. Follow their refactoring for non-AI code.
- For AI code, keep Galaxy's structure.

**5. New dependencies:**
- If Warp added a new crate to `[workspace.dependencies]`, add it to Galaxy's too (unless it's a Warp-internal crate).

**6. Compilation errors in files we took from Warp:**
- These files may reference things that exist in Warp but not Galaxy.
- Stub out or adapt as needed.

### The Loop

Repeat this cycle until clean:

```
1. cargo build 2>&1 | head -80
2. Identify the FIRST error
3. Fix it
4. Go to 1
```

When individual crate errors are isolated, use targeted checks to speed up:

```bash
cargo check -p <crate_name> 2>&1 | head -50
```

**IMPORTANT**: If you encounter more than 50 errors in a single file that all stem from Warp's AI API being absent, the correct fix is usually to **revert that file to Galaxy's version**:

```bash
git checkout HEAD~1 -- <file_path>
```

Or if the file is new from Warp and entirely AI-API-dependent, just delete it.

---

## Phase 7: Post-Build Verification

Once `cargo build` succeeds:

### 7a. Run clippy

```bash
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings 2>&1 | head -100
```

Fix any warnings. Repeat until clean.

### 7b. Run formatter

```bash
./script/format
```

### 7c. Verify no Warp API leaks

```bash
grep -rn "api\.warp\.dev" app/src/ crates/ --include="*.rs" | grep -v "// ported from"
grep -rn "warp\.dev/v1" app/src/ crates/ --include="*.rs"
grep -rn "WARP_API_KEY\|WARP_AUTH_TOKEN" app/src/ crates/ --include="*.rs"
```

Any hits must be removed.

### 7d. Verify Galaxy's AI providers still work

Ensure these files are intact and functional:
- `app/src/ai/bedrock/translator.rs` — Bedrock orchestrator
- `app/src/ai/bedrock/client.rs` — AWS SDK client
- `app/src/ai/bedrock/request_translator.rs` — Request builder
- `app/src/ai/bedrock/response_translator.rs` — Response parser
- `app/src/ai/openai/translator.rs` — OpenAI/LiteLLM orchestrator
- `app/src/ai/openai/client.rs` — HTTP client
- `app/src/ai/openai/convert.rs` — Message conversion
- `app/src/ai/openai/response_translator.rs` — SSE parser
- `app/src/ai/provider/mod.rs` — Provider dispatch
- `app/src/ai/provider/types.rs` — Shared types
- `app/src/ai/blocklist/controller/response_stream.rs` — `resolve_provider_config()`

### 7e. Quick smoke test

```bash
cargo build --release 2>&1 | tail -5
```

If release build also passes, we're good.

---

## Phase 8: Commit and Report

Once everything is clean:

```bash
git add -A
git commit -m "Merge latest Warp upstream into Galaxy

Merged warp/master ($(git log --oneline -1 warp/master | cut -d' ' -f1)) into Galaxy.

Kept Galaxy's:
- AI provider architecture (Bedrock + OpenAI/LiteLLM)
- Branding and deployment pipeline
- Settings and model configuration

Took from Warp:
- Terminal emulation improvements
- UI framework updates
- Editor and utility improvements
- Bug fixes

Stripped:
- Any Warp API/cloud/auth/telemetry additions"
```

Then inform the user of:
- What was merged
- What conflicts were resolved and how
- What Warp additions were rejected/stripped
- Whether any manual follow-up is needed

Ask the user if they want to merge this branch into master:

```bash
git checkout master
git merge update-from-warp-$(date +%Y%m%d)
git push
```

---

## Critical Invariants — NEVER Violate These

1. **Galaxy's AI MUST only use Bedrock or OpenAI/LiteLLM** — defined in `app/src/ai/bedrock/` and `app/src/ai/openai/`. Warp's AI API/server calls are NEVER acceptable.

2. **Galaxy's version and package name stay as-is** — `app/Cargo.toml` keeps `name = "galaxy"` and Galaxy's version number.

3. **Galaxy's binary targets stay as-is** — `galaxy-oss`, `galaxy-dev`, `galaxy-preview`, `galaxy-stable`.

4. **No Warp telemetry** — Any analytics/tracking code from Warp gets deleted, not commented out.

5. **No Warp authentication flows** — Galaxy does not phone home to Warp's servers.

6. **Galaxy's deploy pipeline is untouched** — `script/build-and-deploy-hermes*` and `script/install-galaxy.sh` are always kept.

7. **The build MUST succeed before this skill is considered complete** — If the build is broken, keep fixing. Do not stop.

---

## Failure Recovery

If the merge becomes unrecoverable (e.g., Warp has done a massive architectural change that breaks everything):

1. Abort the merge:
   ```bash
   git merge --abort
   ```

2. Or reset the branch:
   ```bash
   git checkout master
   git branch -D update-from-warp-$(date +%Y%m%d)
   ```

3. Inform the user that a manual, selective port is needed instead of a full merge.

4. Suggest using the `bring-warp-feature-over` skill to cherry-pick specific improvements instead.

---

## Reference: Galaxy ↔ Warp Name Mapping

- `warp` (package) → `galaxy`
- `warp_core` → `galaxy_core`
- `warpui` → `galaxyui`
- `warpui_core` → `galaxyui_core`
- `warp_features` → `galaxy_features`
- `warp_completer` → `galaxy_completer`
- `warp_graphql_schema` → `galaxy_graphql_schema`
- `WARP_` env var prefix → `GALAXY_`
- `~/.warp/` → `~/.galaxy-ai/`
- `warp.sqlite` → `galaxy.sqlite`
- Binary names: `warp` → `galaxy-oss` (main), `galaxy-dev`, `galaxy-preview`, `galaxy-stable`

## Reference: Galaxy's AI Architecture

```
Provider dispatch: response_stream.rs → resolve_provider_config() → ProviderConfig enum
                                       ↓ Bedrock          ↓ OpenAI
                              bedrock/translator.rs    openai/translator.rs
```

- Settings: `ai.bedrock.enabled` (default true), `ai.openai.enabled` (takes priority if true)
- Multi-provider: `ai.providers[]` array with per-provider `base_url`, `api_key`, `models[]`
- Model discovery: OpenAI providers probe `/models` endpoint + `[1m]` variant detection
- Bedrock: Direct AWS SDK calls via `aws-sdk-bedrockruntime`, uses cross-region inference

This architecture is SACRED. Warp's AI changes must never replace or bypass it.
