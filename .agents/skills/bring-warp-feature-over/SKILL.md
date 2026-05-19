---
name: bring-warp-feature-over
description: Identify recent features merged into upstream Warp, assess eligibility for the Galaxy fork, and migrate selected features with Bedrock API adaptations. Use when syncing new Warp functionality into Galaxy.
---

# bring-warp-feature-over

Fetches recent upstream Warp changes, filters for eligible features, lets the user pick which to migrate, plans the work, and spawns agents to implement each migration.

## Overview

Galaxy is a fork of Warp that uses AWS Bedrock instead of Warp's proprietary AI APIs. When upstream Warp ships new features, this skill identifies which ones can be brought over, adapts API calls to Bedrock, and replaces Warp-specific branding where needed.

## Workflow

### 1. Fetch recent upstream changes

Use the GitHub API to pull merged commits from `warpdotdev/warp` on the default branch. Group by PR (commits reference `#<number>`).

```bash
# Fetch commits from the last N days (default 14)
curl -s "https://api.github.com/repos/warpdotdev/warp/commits?per_page=100&since=$(date -u -v-14d +%Y-%m-%dT00:00:00Z)" \
  | python3 -c "
import json, sys
data = json.load(sys.stdin)
for c in data:
    sha = c['sha'][:8]
    msg = c['commit']['message'].split('\n')[0]
    date = c['commit']['author']['date'][:10]
    print(f'{date} {sha} {msg}')
"
```

If the user specifies a date range or number of days, adjust the `since` parameter. For longer lookups, paginate with `&page=2`, etc.

To get more detail on a specific PR:
```bash
curl -s "https://api.github.com/repos/warpdotdev/warp/pulls/<PR_NUMBER>" | python3 -c "
import json, sys
pr = json.load(sys.stdin)
print(pr.get('title'))
print(pr.get('body','')[:2000])
"
```

To see the files changed in a PR:
```bash
curl -s "https://api.github.com/repos/warpdotdev/warp/pulls/<PR_NUMBER>/files" | python3 -c "
import json, sys
files = json.load(sys.stdin)
for f in files:
    print(f['status'], f['filename'])
"
```

### 2. Assess eligibility

For each feature/PR, determine eligibility. A feature is **ineligible** if:

- It depends on Warp's server APIs that we don't have access to (e.g. `warp-server` endpoints, GraphQL mutations for Warp Cloud services)
- It's specific to "Warp Drive" syncing infrastructure (unless it can be adapted to "Galaxy Drive")
- It references "Oz" orchestration that relies on Warp's hosted agent backend
- It requires Warp's proprietary AI proxy and cannot be redirected to Bedrock
- It's purely a Warp billing, subscription, or team management feature
- It touches WASM-only paths that Galaxy doesn't ship

A feature **is eligible** if:

- It's a client-side UX improvement (terminal, editor, completions, themes, settings)
- It's an AI feature that calls a model API we can route through Bedrock (Claude, etc.)
- It's a local-only feature (git integration, file system, SSH, etc.)
- It's a bug fix applicable to shared code paths
- It touches "Warp Drive" but can be rebranded to "Galaxy Drive"

When assessing, also note the **adaptation cost**:
- **Low**: Drop-in (UI fix, terminal behavior, keybindings)
- **Medium**: Needs Bedrock API mapping or minor branding changes
- **High**: Significant refactoring of server-dependent code to work with Bedrock

### 3. Present feature checklist to user

After assessment, present the eligible features to the user using `AskUserQuestion` with `multiSelect: true`. Group by adaptation cost. Include the PR title and a one-line summary of what it does.

Example:
```
Which features would you like to bring over?

Low effort:
- [ ] #10958 - Make worktree menu paths readable for long entries
- [ ] #11099 - Clip terminal view column to prevent split-pane footer overflow

Medium effort:
- [ ] #11049 - Add sleep auto handoff to cloud (needs Bedrock adaptation)

High effort:
- [ ] #10857 - Add orchestration create environment modal (heavy server dependency)
```

### 4. Research selected features

For each selected feature, perform detailed research:

1. **Read the PR diff** — use the GitHub API to understand what changed:
   ```bash
   curl -s "https://api.github.com/repos/warpdotdev/warp/pulls/<PR>/files?per_page=100"
   ```

2. **Map to local files** — identify corresponding files in our Galaxy fork. Check if the files exist and what state they're in.

3. **Identify Bedrock adaptations** — if the feature makes AI/model calls, document:
   - What model is being called and how
   - What the equivalent Bedrock invocation looks like
   - What request/response transformations are needed

4. **Identify branding changes** — flag any references to:
   - "Warp Drive" → "Galaxy Drive"
   - "Oz" → remove or replace
   - Warp-specific UI copy that needs updating

5. **Document dependencies** — note any new crates, feature flags, or config changes needed.

### 5. Write migration plans

For each selected feature, create a plan file at:
```
plans/warp-migrations/<PR_NUMBER>-<short-slug>.md
```

Each plan should contain:

```markdown
# Migration: <PR Title>

**Source PR**: warpdotdev/warp#<number>
**Adaptation Cost**: Low | Medium | High
**Date Assessed**: <today>

## Summary
<What the feature does, 2-3 sentences>

## Files Changed (upstream)
<List of files from the PR>

## Local File Mapping
<Corresponding Galaxy files, noting any that don't exist yet>

## Required Adaptations
- <Bedrock changes if any>
- <Branding changes if any>
- <New dependencies if any>

## Implementation Steps
1. <Step 1>
2. <Step 2>
...

## Testing Notes
<How to verify this works in Galaxy>
```

### 6. Spawn implementation agents

For each migration plan, spawn an agent using the `Agent` tool to make the code changes. Key rules for spawning:

- **Agents only write code** — they do NOT run `cargo check`, `cargo build`, or `cargo clippy`
- **Spawn agents in parallel** for independent features (no shared file conflicts)
- **Spawn sequentially** if two features touch the same files
- Each agent's prompt must include:
  - The full migration plan content
  - The specific files to modify and what changes to make
  - Instructions to NOT run cargo commands
  - Instructions to report back what files were changed

Example agent prompt structure:
```
You are implementing a Warp feature migration into the Galaxy fork.

Migration plan:
<paste plan content>

Instructions:
- Make ONLY the code changes described in the plan
- Do NOT run cargo check, cargo build, cargo clippy, or any compilation commands
- Adapt any Warp API calls to use Bedrock (see plan for specifics)
- Replace "Warp Drive" with "Galaxy Drive" where applicable
- Remove or skip any "Oz" references
- Report back: list all files you modified and a brief summary of changes
```

### 7. Verify builds (parent agent only)

After all implementation agents complete, the parent agent (you) runs:

```bash
cargo fmt
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
```

If there are errors, fix them directly or re-delegate targeted fixes to agents.

### 8. Instruct user to test

After a clean build, present the user with testing instructions:

- List each migrated feature and how to exercise it
- Note any features that need specific configuration or feature flags enabled
- Ask the user to report back any issues they find
- Remind them to test with `cargo run` and verify the features work end-to-end

## Branding Reference

| Upstream (Warp)       | Galaxy equivalent     |
|-----------------------|-----------------------|
| Warp Drive            | Galaxy Drive          |
| Oz / Orchestrator     | Remove or skip        |
| Warp AI / Warp Agent  | Galaxy AI / Agent     |
| warp-server endpoints | Skip (ineligible)     |

## Bedrock Adaptation Patterns

When adapting AI features from Warp's proxy to Bedrock:

- Warp's AI calls typically go through their proxy server — Galaxy calls Bedrock directly
- Look for the existing Bedrock integration patterns in `app/src/ai/` for how Galaxy makes model calls
- Ensure streaming responses are handled correctly (Bedrock uses different event formats)
- Check `WARP.md` "Bedrock Diagnostics" section for debugging tools

## Related Skills

- `fix-errors` — for resolving build failures after migration
- `add-feature-flag` — if the migrated feature needs gating
- `implement-specs` — for larger features that need full specs
