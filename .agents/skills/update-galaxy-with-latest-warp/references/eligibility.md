# Galaxy Upstream Eligibility

Use this reference when classifying recent Warp commits for a broad upstream synchronization.

## Safe lanes

These are usually eligible when they remain local and do not import excluded services:

- `app/src/terminal/`
- `crates/galaxy_terminal/` or the corresponding upstream terminal crate
- `crates/galaxyui/`, `crates/galaxyui_core/`, and `crates/galaxyui_extras/`
- `crates/editor/`, `crates/sum_tree/`, and local UI components
- local completion, parser, search, filesystem, Git, SSH, shell, theme, rendering, accessibility, and platform fixes
- tests for eligible local behavior

Path eligibility is only a first-pass signal. Inspect imports, feature flags, constructors, and callers before applying a commit.

## Manual-review lanes

Treat these as adaptation areas, never blind cherry-pick targets:

- `app/src/ai/`
- `app/src/settings/`, onboarding, account surfaces, and shared workspace state
- persistence models or migrations
- feature flags and generated schemas
- cross-cutting application lifecycle or window/session restoration
- code that mixes UI with Warp server responses, account state, experiments, or telemetry

For AI changes, current `AGENTS.md` invariants are authoritative. At minimum preserve:

- `app/src/ai/runtime/` and the durable direct-provider coordinator lifecycle
- `app/src/ai/provider/`
- `app/src/ai/bedrock/` and `app/src/ai/openai/`
- `app/src/ai/blocklist/controller.rs` and `controller/response_stream.rs`
- local tool execution and child-agent orchestration under `app/src/ai/blocklist/action_model/execute/`
- `crates/galaxy_agent_core/` and `crates/galaxy_agent_rig/`
- ACP runtime/transport ownership

An upstream UI improvement inside a manual-review lane may still be extracted when it does not alter those lifecycles.

## Never import

Reject or remove dependencies on:

- Warp proprietary AI clients, hosted agent backends, or hosted orchestration
- Warp authentication, accounts, subscriptions, billing, teams, or entitlements
- Warp telemetry or analytics that transmit to Warp
- Warp server GraphQL/API endpoints
- Warp cloud sync or Warp Drive assumptions that conflict with Galaxy's local-first behavior
- Warp deployment, updater channels, package identity, documentation links, or branding
- service-generated code or schemas unused by a Galaxy-supported local provider

Do not replace these with dead stubs merely to make an upstream patch compile. Extract the local behavior or skip the candidate.

## Leak and dependency review

Review newly introduced lines with `rg`, scoped to the diff and affected files. Useful indicators include:

```text
api.warp.dev
warp.dev/v1
WarpAIService
WarpAiClient
WARP_API_KEY
WARP_AUTH_TOKEN
graphql
telemetry
billing
subscription
entitlement
```

These are indicators, not a deletion list. Existing compatibility names may be intentional; only new or newly reachable Warp coupling must be removed or adapted.

For each new dependency, determine whether it is:

1. already available in the workspace;
2. local/client-side and necessary;
3. pinned to an exact Git revision;
4. transitively pulling Warp service code.

## Decision rule

- Port when the user-visible benefit and implementation are local, bounded, and compatible with Galaxy invariants.
- Adapt automatically when the differences are mechanical or the local replacement is unambiguous.
- Extract only safe hunks from mixed commits.
- Skip service-bound work.
- Hold only when reasonable implementations would produce materially different Galaxy behavior or risk.
