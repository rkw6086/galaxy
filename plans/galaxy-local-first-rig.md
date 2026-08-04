# Galaxy Local-First Recovery and Rig Migration

> **Started:** 2026-08-04
> **Status:** Active architecture recovery
> **UI ledger:** [`ui-flow-inventory.md`](ui-flow-inventory.md)
> **Supersedes:** [`galaxy-refactor.md`](galaxy-refactor.md)

## Product contract

Galaxy is a local-first developer terminal with Warp-quality interaction design. It may communicate
with a model provider, remote machine, or tool only when the user or an administrator has explicitly
configured that boundary. It must not depend on Warp authentication, cloud storage, billing,
telemetry, remote logging, remote feature control, session sharing, or Oz.

The non-negotiable properties are:

1. A fresh install works without an account.
2. Terminal, editor, conversation, rules, profiles, notebooks, workflows, and history data are local.
3. No inherited Warp endpoint can address an external host in the OSS build.
4. Model traffic goes only to the provider selected for the active model.
5. ACP agents are explicit, trusted local subprocesses with a visible permission boundary.
6. Network-capable tools are off by default and visible when enabled or invoked.
7. The UI consumes Galaxy-owned domain types, not a provider SDK or Warp wire protocol.
8. Provider and agent implementations are replaceable without changing conversation UI code.

## Current baseline

The code is not merely untidy; it has conflicting architectural centers.

- `app/src` contains roughly 1.08 million lines of Rust across product and test code.
- `app/src/ai` alone contains roughly 270,000 lines in 555 Rust files.
- Seventy-six app files reference `warp_multi_agent_api`.
- The provider and ACP implementation inspected for this plan spans more than 22,000 lines.
- Large presentation/coordinator files include `workspace/view.rs` (about 29,000 lines),
  `terminal/view.rs` (about 29,000), `terminal/input.rs` (about 16,000), and
  `settings_view/ai_page.rs` (about 8,500).
- The OSS binary configured Warp production HTTP, RTC, session-sharing, and Firebase values even
  though telemetry sending had already been stubbed out. The first safety patch replaces those
  values with loopback-only disabled configuration and rejects Warp/Firebase auth exchange.

The provider-backed prompt path currently resembles:

```text
Galaxy UI/controller
  -> RequestParams (already contains provider-specific Bedrock history fields)
  -> warp_multi_agent_api::Request protobuf
  -> Bedrock or OpenAI request translator
  -> provider SDK / JSON / SSE
  -> provider response translator
  -> warp_multi_agent_api::ResponseEvent protobuf
  -> Galaxy controller/history/UI
```

ACP takes another branch inside the same `ResponseStream` model and translates ACP events into the
same legacy Warp response events. Provider choice, provider credentials, ACP session state, retry
policy, network recovery, cancellation, telemetry remnants, and UI event emission therefore meet in
one coordinator.

The problem is not that translations exist. Every integration needs one boundary translation. The
problem is that Warp's former server protocol is acting as Galaxy's domain model, so every new
provider needs translations on both sides of a protocol Galaxy does not own.

## Target architecture

```text
GalaxyUI views and models
        |
        v
Galaxy application services
  conversation / permissions / local persistence / provider registry
        |
        v
galaxy_agent_core
  TurnRequest, Message, Content, ToolSpec, AgentEvent, Usage, StopReason, AgentError
  AgentRuntime trait -> AgentEventStream
        |
        +-------------------------+
        |                         |
        v                         v
galaxy_agent_rig             galaxy_agent_acp
  OpenAI-compatible            ACP subprocess/session
  LiteLLM/Ollama/LM Studio     ACP event adapter
  AWS Bedrock                  Galaxy tool bridge
  Rig/MCP tool bridge
        |                         |
        +------------+------------+
                     v
             explicit egress policy
```

### `galaxy_agent_core`

This crate is the dependency rule that makes the refactor possible. It owns only stable Galaxy
concepts:

- ordered conversation messages with text, images, reasoning, tool calls, and tool results;
- model/provider identifiers that do not encode a particular SDK type;
- dynamic tool descriptions and JSON schemas;
- turn events such as text delta, reasoning delta, tool proposed, permission requested, tool
  started, tool completed, usage updated, turn stopped, and failure;
- cancellation and live steering control;
- structured stop and error classification;
- the `AgentRuntime` interface.

It must not depend on GalaxyUI, `warp_multi_agent_api`, Rig, an AWS SDK, ACP, GraphQL, or app
persistence.

### `galaxy_agent_runtime`

This application-service layer owns:

- resolving a conversation's backend once per conversation;
- resolving a model to a configured provider endpoint;
- building system and project context;
- conversation history and summarization policy;
- tool registration and permission policy;
- retry, cancellation, steering, and recovery semantics;
- mapping runtime events to local persistence and UI-facing models.

The current UI can initially be kept alive with a temporary adapter from `AgentEvent` to legacy
`warp_multi_agent_api::ResponseEvent`. That adapter is a migration device, not the final boundary.

### `galaxy_agent_rig`

Rig becomes the implementation for provider-backed conversations. The version evaluated for this
plan is Rig 0.40.0. When introduced, it must be pinned exactly until its documented breaking-change
cadence settles for Galaxy.

Rig is a good fit for the provider side because it already defines a canonical completion request,
provider implementations, streaming content/tool events, model history, typed tools, hooks, MCP via
`rmcp`, and a multi-turn agent runner. The integration should use those abstractions rather than
copying Rig's internal provider request structs into Galaxy types.

Provider coverage for the first migration:

| Galaxy provider | Rig implementation | Notes |
|---|---|---|
| LiteLLM / generic OpenAI-compatible | Rig OpenAI-compatible client | Custom base URL and key; preserve per-model endpoint routing. |
| Ollama / LM Studio | OpenAI-compatible or Rig provider adapter | Treat as explicit local/LAN endpoints. |
| AWS Bedrock | `rig-bedrock` through the Rig facade | Preserve profile, static credential, SSO, region, and inference-profile behavior through a focused compatibility audit. |
| MCP tools | Rig `rmcp` tool server/client support | Reuse existing Galaxy MCP lifecycle where it is stronger; bridge tools at one boundary. |

Rig's documented integrations cover model providers and MCP, not Agent Client Protocol. ACP should
not be forced through Rig. It is a peer implementation of `AgentRuntime`.

### Tool execution and permissions

Galaxy must continue to own the user-facing tool lifecycle. A model framework may drive the loop,
but it must not silently bypass Galaxy's permission cards or execute a shell/file operation before
the UI can authorize it.

The Rig adapter will therefore:

1. register thin Rig tools that delegate into Galaxy's tool executor;
2. attach a Rig agent hook to observe and fail closed on tool calls;
3. emit a Galaxy `ToolProposed` or `PermissionRequested` event before execution;
4. await a permission decision when required;
5. execute through the existing Galaxy tool implementation;
6. return the result to Rig and emit correlated start/result events using a stable Galaxy call ID.

Rig 0.40's streamed model-tool-call, tool-execution-start, tool-result, hooks, request patching, and
fail-closed flow semantics are useful here, but contract tests must prove the exact ordering Galaxy's
UI expects.

### `galaxy_agent_acp`

The existing `crates/acp` runtime has useful protocol/session work and should be retained initially.
Its application adapter should move out of `ResponseStream` and emit `AgentEvent` directly.

ACP-specific capabilities remain visible in backend metadata:

- session load/new-session support;
- agent authentication methods;
- configuration discovery;
- filesystem and terminal capability negotiation;
- permission requests;
- prompt steering and cancellation.

Provider model controls should not appear for ACP-owned conversations because the external agent
owns its model and authentication.

## Local data architecture

"Galaxy Drive" becomes a local content library, not a renamed cloud sync client. Existing UI for
rules, profiles, notebooks, workflows, environment-variable collections, and MCP configurations can
be preserved while its storage service is replaced.

The target repository interface is local and revisioned:

```text
LocalObjectRepository
  list(kind, scope)
  get(id)
  create(object)
  update(id, expected_revision, object)
  delete(id)
  watch(kind/scope)
```

SQLite remains the default store. Filesystem import/export can be layered on later. The UI should
not know whether an object used to be a `CloudObject`; it should receive local object IDs and local
repository events.

Migration must preserve existing local rows before removing cloud-shaped schemas. A temporary
compatibility repository can read the current tables without starting `SyncQueue`, `UpdateManager`,
GraphQL, or RTC listeners.

## Network and trust model

Every runtime network path belongs to one of these classes:

| Class | Default | Examples |
|---|---|---|
| Inherited product service | Forbidden | Warp auth, GraphQL, RTC, session sharing, Oz, telemetry, remote logs, remote flags. |
| Configured model provider | Allowed only when selected | Bedrock, LiteLLM, OpenAI-compatible endpoint, Ollama on another host. |
| User-initiated remote development | Allowed with visible intent | SSH, Git fetch/push, remote MCP, provider/model discovery. |
| Agent network tool | Disabled until enabled by policy | Web fetch/search, HTTP MCP tools, computer-use browser actions. |
| Product maintenance | Separate explicit policy | Update checks, release download, optional LSP/runtime downloads. |

The OSS binary must never contain a usable inherited Warp endpoint. UI hiding and feature flags do
not satisfy this requirement by themselves.

## Migration sequence

### Phase 0 — Freeze, inventory, and close implicit egress

- Maintain the UI ledger and classify every registered action/menu/settings route.
- Disable inherited Warp service endpoints in OSS and fail closed on auth exchange.
- Rotate and revoke the signing credential currently tracked in migration documentation, remove it
  from the working tree, and purge it from repository history in a coordinated security change.
- Add an automated forbidden-domain test for shipped configuration and runtime network fixtures.
- Mark old Bedrock-only architecture documents as historical.
- Stop porting upstream cloud, billing, telemetry, or Oz features during this migration.

Exit condition: a clean OSS launch and normal local terminal use cannot address a Warp-operated
runtime endpoint, even if a stale UI action is triggered.

### Phase 1 — Introduce the Galaxy agent domain seam

- [x] Add `galaxy_agent_core` with requests, messages, events, errors, turn control, and the
  `AgentRuntime` trait.
- [x] Add contract tests using a deterministic fake runtime.
- [x] Move the provider-neutral conversation message and tool-definition types out of the app
  crate, retaining only a temporary compatibility re-export.
- [x] Route provider request startup through a named `ProviderRuntime` boundary so the controller
  no longer calls the provider generator directly.
- [ ] Map legacy response events to `AgentEvent` and make the compatibility runtime implement
  `AgentRuntime`; keep the inverse UI adapter until consumers migrate.
- [ ] Remove provider-specific fields such as `bedrock_message_history` from UI-level
  `RequestParams`.

The compatibility path still produces a Warp protobuf stream for the UI, but Rig-backed providers
implement `AgentRuntime` and cross that protocol boundary only in the app-owned UI adapter. Legacy
providers remain behind `ProviderRuntime` while their migrations continue.

Exit condition: the conversation controller selects an `AgentRuntime` and does not match directly on
Bedrock/OpenAI/ACP configuration.

### Phase 2 — First Rig vertical slice: OpenAI-compatible streaming

- [x] Pin `rig-core` 0.40.0 and implement one explicit OpenAI-compatible provider.
- [x] Support text, reasoning where available, cancellation, stop reason, usage, and persisted
  history.
- [x] Route any model entry with `use_rig = true` through Rig while leaving unmarked models on the
  compatibility path.
- [x] Test Rig's real Chat Completions SSE parser against normalized events, plus the UI stop/usage
  compatibility mappings.

Initial opt-in example:

```toml
[ai.openai]
enabled = true

[[ai.providers]]
name = "LiteLLM (ai.ryserve.net)"
base_url = "https://ai.ryserve.net/v1"
api_key = "REPLACE_WITH_LOCAL_KEY"

[[ai.providers.models]]
model_id = "codex-gpt-5.6-sol-xhigh"
display_name = "Codex GPT-5.6 SOL (xhigh)"
context_size = 200000
provider = "openai"
use_rig = true
supports_system_messages = false
```

Phase 2 intentionally does not expose Galaxy's legacy tool list to Rig. That ownership moves as a
unit in Phase 3; until then, the opt-in slice validates text conversation streaming without two
competing tool executors.

Exit condition: a LiteLLM or local OpenAI-compatible conversation streams through Rig without
`warp_multi_agent_api::Request` on the provider side.

### Phase 3 — Tools, permissions, MCP, and multi-turn behavior

- Bridge the core Galaxy tools into Rig.
- Preserve permission cards, denial, cancellation, parallel-call ordering, and error visibility.
- Bridge current MCP tools through Rig's `rmcp` support or a single Galaxy tool-server adapter.
- Port loop prevention and unknown-tool handling to domain-level policies.

Exit condition: representative read, edit, shell, MCP, denial, and failure flows pass integration
tests without provider-specific UI code.

### Phase 4 — Bedrock through Rig

- Implement Bedrock client construction and model resolution through `rig-bedrock`.
- Compare request behavior for system prompts, images, tool schemas, cache controls, reasoning,
  inference profiles, token usage, and context limits.
- Keep a short-lived compatibility fallback for unsupported Bedrock behavior, measured by tests.
- Delete custom Bedrock translation code only after parity is proven.

Exit condition: supported Bedrock models use the same `AgentRuntime` event contract as
OpenAI-compatible models.

### Phase 5 — ACP convergence

- Move ACP launch/session/transport control behind `galaxy_agent_acp`.
- Translate ACP events directly to `AgentEvent`.
- Remove ACP branching from the UI response stream model.
- Keep ACP-specific settings and capability disclosure, but share transcript and permission UI.

Exit condition: the controller cannot distinguish ACP from Rig except through backend capability
metadata.

### Phase 6 — Local Galaxy Drive and identity removal

- Introduce `LocalObjectRepository` over existing SQLite data.
- Move rules, profiles, notebooks, workflows, env collections, and MCP configs to the local service.
- Replace account/workspace ownership with local scopes.
- Remove auth, teams, billing, referral, cloud sync, GraphQL, RTC, sharing, and remote-control UI.

Exit condition: none of the kept content flows require `AuthState`, `CloudModel`, `UpdateManager`,
`SyncQueue`, or a server ID.

### Phase 7 — UI untangling

- Split coordinator files along the flow boundaries in the UI ledger.
- Views render state and emit intent; application services perform persistence and runtime work.
- Reuse existing shared button themes and theme tokens.
- Remove unreachable modals/actions instead of continuing to hide them behind flags.

Exit condition: every kept flow has an owner, a state model, a service boundary, and automated
coverage for success, failure, cancellation, and restore where applicable.

### Phase 8 — Delete the legacy protocol center

- Remove `warp_multi_agent_api` from UI/controller and persistence code.
- Delete the custom OpenAI/Bedrock request and response translators replaced by Rig.
- Delete no-op telemetry schemas/macros after call sites no longer depend on them.
- Remove Warp server, GraphQL, Firebase, cloud-object, Oz, billing, and referral crates from default
  and then workspace builds when no retained feature needs them.

Exit condition: `rg` finds no runtime dependency from the shipped app to Warp service code or Warp's
multi-agent wire protocol.

## Verification gates

Every phase must keep these checks green:

- formatting and Clippy for changed crates;
- unit tests for the new domain/runtime layer;
- deterministic transcript contract tests;
- integration coverage for terminal and agent flows touched by the phase;
- a local-only egress test using request interception or a denied-network test environment;
- restart/restore tests for conversations and local content;
- no secret or prompt contents in logs unless a user explicitly enables a diagnostic mode.

Provider parity tests should compare semantic events, not provider JSON snapshots alone. The stable
contract is what the UI and persistence observe.

## Decisions

| Decision | Choice |
|---|---|
| Provider abstraction | Rig behind a Galaxy-owned runtime interface. |
| ACP relationship | Peer runtime, not a Rig provider. |
| UI compatibility during migration | Temporary `AgentEvent` to legacy response-event adapter. |
| Long-term UI model | Galaxy domain events only. |
| Galaxy Drive | Local SQLite-backed content library. |
| Login/account | Remove from OSS product flows. |
| Telemetry/remote logs/remote flags | Remove, not merely default-off. |
| SSH and remote Git | Keep as explicit user-initiated remote development boundaries. |
| Web/network agent tools | Disabled by default and permission-visible. |
| Rig dependency | Exact version pin with upgrade contract tests. |

## Immediate next vertical slice

After the Phase 0 egress guard and UI ledger are verified, the next implementation change is a small
`galaxy_agent_core` crate plus a legacy adapter. It should move only provider-neutral message/event
types and runtime selection. Adding Rig before this seam would couple the UI to a new framework and
repeat the current mistake with a different name.
