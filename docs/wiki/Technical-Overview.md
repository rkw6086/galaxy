# Technical overview

Galaxy is a native Rust desktop application: terminal, editor, local workspace, and an agent layer. The public desktop binary is `galaxy-oss`; `galaxy` is the application Cargo package. Most application code uses AGPL-3.0-only, while the GalaxyUI crates use MIT; third-party components retain their own notices.

The normal desktop entry point disables the inherited Warp server connection. Local persistence, direct model providers, and session-owned external agents are the important architectural boundaries. Retained cloud-service types and UI are not a promise that Galaxy operates the corresponding hosted service.

## Source map

| Area | Main source locations |
| --- | --- |
| Desktop entry and feature/channel configuration | `app/src/bin/oss.rs`, `crates/galaxy_core/src/channel/` |
| Terminal, shell launch, sessions, panes | `app/src/terminal/`, `app/src/pane_group/`, `app/src/workspace/` |
| Settings and provider setup | `app/src/settings/`, `app/src/settings_view/` |
| Durable direct-provider state machine | `crates/galaxy_agent_core/src/provider_run.rs` |
| Runtime coordination and request projection | `app/src/ai/runtime/` |
| Native Bedrock transport and discovery | `app/src/ai/provider/` |
| Other native/direct provider runtimes | `crates/galaxy_agent_rig/`, selected through `app/src/ai/runtime/rig.rs` |
| ACP transport and setup | `app/src/ai/acp/` and the response-stream projection |
| Agent action execution and permissions | `app/src/ai/blocklist/` |
| Independent shell jobs | `app/src/ai/background_jobs/` |
| MCP connections | `app/src/ai/mcp/` |
| Local objects, conversations, and configuration | `app/src/local_object*`, local persistence, `crates/galaxy_core/src/paths.rs` |
| Activity and diagnostic capture | `app/src/ai/activity/`, `app/src/ai/remote_logging*`, `crates/galaxy_logging/` |
| Native release pipeline | `.github/workflows/build.yml`, `script/ci/` |

## Direct providers

The controller prepares a run; the durable `ProviderRun` state machine owns continuation, bounded model retries, tool batches, cancellation, and terminal outcome. A provider runtime performs one model turn at a time:

```text
conversation controller
  → prepare provider run
  → durable ProviderRun + coordinator
  → one AgentRuntime model call
  → correlated tool batch and committed results
  → next ProviderRun boundary, or terminal outcome
```

Provider selection can choose transport and request details, but must not choose a different lifecycle owner. A runtime cannot independently start an untracked follow-up loop after returning one turn.

The coordinator validates model events, projects output into the terminal/chat UI, and commits action lifecycle events. Actions and callbacks are correlated with their owning conversation, run, epoch, and call. Stale or duplicate callbacks must not advance a replacement run. Tool-result lookup is conversation-scoped rather than a global search by action ID.

OpenAI/LiteLLM, ChatGPT subscription, native Anthropic, Gemini, and Vertex AI use one-turn runtimes selected through the Rig integration. Bedrock uses the native AWS `ConverseStream` runtime. Compatibility/background transport helpers remain in the source and should not be mistaken for owners of the main conversation lifecycle.

## ACP agents

ACP is a separate, session-owned runtime. The external agent controls its own model, authentication, internal tool loop, and compaction. Galaxy owns its connection and UI projection, and applies the relevant protocol permission boundaries; it does not reconstruct the external agent's hidden model traffic.

The activity inspector can therefore show what Galaxy exchanges with ACP, but not every model request made inside an external agent. A direct-provider trace and an ACP trace do not contain the same information.

## Durability and cancellation

Active direct-provider work checkpoints state before external actions. Checkpoints must not contain credentials. Recovery validates saved invariants and reconciles command/job state before continuing.

A saved state indicating that a model call was awaiting a remote outcome is uncertain after a process restart; it must not blindly replay the call. Known recoverable failures observed in the same process can enter the bounded retry lifecycle. User follow-ups for the same conversation queue behind cancellation/cleanup of the old generation and rebuild their history after its final output is committed.

Background commands and child conversations have their own ownership. Cancelling a parent conversation turn must not be treated as universal authorization to kill unrelated jobs or independently running children. Completion evidence must return to the owning conversation before the parent can reliably reason about its result.

## Tools, permissions, and background jobs

Tool definitions are filtered by session capabilities and the active profile. Execution checks those permissions again at the action boundary. A model asking for a tool is not permission to execute it. Unknown/invalid calls receive a correlated error result rather than silently disappearing.

Agent shell commands use noninteractive background jobs with closed stdin and no PTY. The job manager owns the process group/job object, logs, exit status, termination controls, and eventual completion delivery. User-typed commands continue through the normal interactive terminal path.

See [Profiles, permissions, and tools](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools) for the user-facing behavior.

## Context accounting and compaction

Request construction separates prompt/tool overhead, usable history, and reply reservations. Lower positive provider default/input limits and profile caps are preserved instead of automatically expanding every request to an advertised maximum.

Direct-provider progressive compaction runs as a separate request alongside foreground work. It snapshots a prefix and applies a completed summary only at a safe model boundary after validating that exact prefix. Newer history is preserved, and live accounting is recomputed. A pending summary should block work only after the usable context is exhausted; cancellation and terminal cleanup remain responsive.

Tool-use/result pairs are archived before progressive summarization removes their full text from the active transcript. Explicit history recall can return archived evidence, with tool calls/results kept correlated. The activity inspector is a diagnostic view of these events, not additional hidden model context.

## Local persistence and network boundaries

Normal macOS/Linux data lives under `~/.galaxy`; Windows uses platform application-data paths. Conversations, local Drive objects, profiles, settings, and workspace state are local. Legacy names are retained where needed for serialization, protocol, or migration compatibility.

Configured providers and external tools still receive their requests. Remote diagnostics require a separate opt-in, and raw model payload capture requires another explicit setting. See [Data and privacy](https://github.com/rkw6086/galaxy/wiki/Data-and-Privacy).

## Known incomplete areas

The local Docker sandbox still depends on an external `sbx` helper, inherited hosted-environment setup, and incomplete container-aware command/file handling and cleanup. The inherited hosted environment wizard is not a supported Galaxy provisioning service. These limitations should be fixed as product work rather than hidden by changing a documentation URL.

## Implementation references

- [Architecture and invariants](https://github.com/rkw6086/galaxy/blob/master/AGENTS.md#ai-provider-architecture)
- [Desktop entry point](https://github.com/rkw6086/galaxy/blob/master/app/src/bin/oss.rs)
- [ProviderRun](https://github.com/rkw6086/galaxy/blob/master/crates/galaxy_agent_core/src/provider_run.rs)
- [Provider run coordinator](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/runtime/provider_run_coordinator.rs)
- [Runtime selection](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/runtime/rig.rs)
- [Local-first migration plan](https://github.com/rkw6086/galaxy/blob/master/plans/galaxy-local-first-rig.md)
