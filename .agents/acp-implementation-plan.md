# ACP Model, Mode, Registry, and Credential Implementation Plan

## Goal

Complete Galaxy's ACP integration so that ACP agents can be discovered, configured, selected through the normal model selector, launched with the selected model/configuration, and persisted safely across sessions. Keep all ACP metadata, settings, persistence, selector behavior, runtime protocol handling, and settings UI synchronized without duplicate representations or dead code.

## Constraints and invariants

- ACP session configuration is represented by the protocol's `configOptions`, not a hard-coded model/mode matrix.
- Prefer `configOptions` over the legacy `modes` API.
- Preserve agent-provided option and value ordering.
- Treat option values as opaque IDs; never parse display labels to reconstruct values.
- Omitted ACP capabilities mean unsupported.
- Only send capability-gated request fields when the agent advertises support.
- Registry metadata describes agents and distributions; it does not provide live model/mode availability.
- Live model/config discovery comes from the running ACP agent.
- Keep non-secret ACP metadata in settings/persistence.
- Keep credentials in secure storage/Keychain; do not put secrets in settings.toml or ACP command arguments.
- Preserve support for custom ACP agents not present in the registry.
- Avoid duplicate selector/model representations; use shared normalized ACP types.
- Do not regress existing Bedrock, OpenAI/LiteLLM, MCP, authentication, or provider behavior.

## Shared data model

Create or consolidate one normalized ACP data model shared between the ACP crate, settings, persistence, model selector, and settings UI.

Required concepts:

- `AcpAgentInfo`
  - registry ID
  - display name/title/name
  - description
  - version
  - repository/website
  - authors/license
  - icon URL
  - distribution metadata
- `AcpAgentCapabilities`
  - load session
  - resume
  - close/delete session
  - additional directories
  - prompt image/audio/embedded context
  - MCP HTTP/SSE
  - boolean config options
- `AcpConfigOption`
  - option ID
  - name
  - description
  - category
  - type
  - current value
  - ordered values
- `AcpConfigValue`
  - opaque value ID/value
  - display name
  - description
- `AcpModelSelection`
  - agent ID
  - complete selected option-value map
  - stable selector ID
  - display labels

Use serde-compatible forms for settings and persistence. Keep protocol-native conversion code in the ACP crate or one app adapter module, not duplicated in multiple consumers.

## Task list

### Phase 1: Establish clean shared types

- [ ] Inspect current ACP schema exports and app settings/persistence types.
- [ ] Replace ad hoc `serde_json::Value` use where practical with a shared serializable opaque ACP config value representation.
- [ ] Add explicit conversion functions between generated ACP schema types and Galaxy settings/persistence types.
- [ ] Add stable equality/hash/identity helpers for config selections.
- [ ] Add unit tests for option/value conversion, unknown categories, unknown option types, boolean values, and ordering.
- [ ] Ensure `AcpConversationData::for_fork` preserves selections while clearing session IDs.

### Phase 2: ACP initialization and discovery

- [ ] Extend ACP initialization client capabilities to advertise supported boolean config options.
- [ ] Capture `agentInfo` from `initialize`.
- [ ] Capture all relevant initialization capabilities.
- [ ] Add an explicit discovery API on `AcpSessionManager` or a dedicated discovery result path.
- [ ] Create a temporary discovery session using the configured working directory and appropriate MCP servers.
- [ ] Read the complete `configOptions` response from `session/new`.
- [ ] Preserve agent option/value ordering.
- [ ] Support discovery for custom executables as well as built-in presets.
- [ ] Close/delete the temporary discovery session when the advertised capability allows it.
- [ ] Make discovery failures non-destructive: retain last known cache and expose an actionable error.
- [ ] Add discovery timeout and cancellation handling.
- [ ] Add tests using fake ACP agents for successful discovery, authentication failure, missing options, malformed options, and timeout.

### Phase 3: Runtime config application and updates

- [ ] Apply selected values after `session/new` and before the first prompt.
- [ ] Capture the complete response from `session/set_config_option`.
- [ ] Replace the current complete config state after every successful set operation.
- [ ] Convert `config_option_update` notifications into app-visible events.
- [ ] Reconcile dependent options when changing a model changes modes/thought levels.
- [ ] Reject or safely drop stale values that are no longer advertised.
- [ ] Ensure restored sessions reconcile persisted selections with the agent's current config state.
- [ ] Track current config values in runtime session metadata.
- [ ] Apply capability checks for load/resume/close/delete/additional directories/MCP transports.
- [ ] Add tests for setting options, dependent option changes, rejected values, and runtime notifications.

### Phase 4: Settings.toml persistence and synchronization

- [ ] Finalize the `ai.acp.agents` settings schema.
- [ ] Persist registry metadata and runtime discovery metadata without credentials.
- [ ] Persist capabilities and complete config options atomically.
- [ ] Store discovery timestamp and source/version information.
- [ ] Invalidate or refresh discovery when launch fingerprint, executable, arguments, preset version, or agent version changes.
- [ ] Preserve custom launch configuration while refreshing registry metadata.
- [ ] React to ACP settings changes and refresh the model list.
- [ ] Handle malformed/stale settings gracefully.
- [ ] Add settings schema validation and round-trip tests.
- [ ] Add a documented example to the repository's settings documentation or sample `settings.toml` if one exists.

### Phase 5: ACP Registry integration

- [ ] Add a registry client for `https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json`.
- [ ] Parse the registry version and agent entries.
- [ ] Support npm/npx, uvx, and platform-specific binary distributions.
- [ ] Select the correct operating system and architecture distribution.
- [ ] Preserve command, args, environment, archive, checksum, icon, and version metadata.
- [ ] Validate URLs, commands, arguments, environment names, and checksums before use.
- [ ] Cache the registry with a refresh timestamp and last-known-good fallback.
- [ ] Never auto-install or execute an agent without explicit user action.
- [ ] Preserve custom agents outside the registry.
- [ ] Add registry parsing/platform-selection/security tests.

### Phase 6: Model selector integration

- [ ] Add ACP choices through the existing `LLMPreferences`/model selector path.
- [ ] Use structured selection data instead of parsing encoded display IDs.
- [ ] Show ACP choices under an ACP agent grouping.
- [ ] Render model labels such as `GPT 5.4 Sol (Ultra)` while preserving structured values.
- [ ] Use `model` as the primary selector dimension.
- [ ] Use `mode` and `thought_level` as secondary labels or grouped choices.
- [ ] Keep `model_config` and advanced/unknown options in an ACP configuration panel rather than exploding the model list.
- [ ] Avoid generating invalid Cartesian combinations for dependent options.
- [ ] Refresh selector choices after discovery or config updates.
- [ ] Ensure the selected row propagates into `AcpConversationData.config_values`.
- [ ] Ensure model metadata, transcript metadata, usage metadata, and restored conversations use the selected ACP identity.
- [ ] Add selector and selection propagation tests.

### Phase 7: Agent screen/settings UI

- [ ] Extend the ACP settings widget with registry-backed agent selection.
- [ ] Show agent icon, display name, description, version, and installation status.
- [ ] Add refresh registry action.
- [ ] Add discover/refresh capabilities action.
- [ ] Show discovery status, timestamp, and actionable errors.
- [ ] Show discovered models/modes and option descriptions.
- [ ] Show advanced config options without duplicating selector logic.
- [ ] Add install/configure action only with explicit user confirmation.
- [ ] Support custom command/args fields and preserve them when switching registry entries.
- [ ] Use existing button themes and UI components.
- [ ] Add UI tests or view-model tests for loading, error, refresh, and selection states.

### Phase 8: Credentials and secure storage

- [ ] Leave global Keychain registration intact.
- [ ] Audit ACP/OpenAI/LiteLLM credential paths for secrets in settings.toml or command arguments.
- [ ] Store Galaxy-managed ACP/OpenAI/LiteLLM API keys through the existing secure-storage service.
- [ ] Keep only non-secret references/configuration in settings.toml.
- [ ] Add migration from existing plaintext settings keys to secure storage.
- [ ] Decide and document behavior when secure storage is unavailable.
- [ ] Do not copy or manage credentials owned by ACP agents themselves.
- [ ] Add tests for secure-storage read/write/migration/error behavior.

### Phase 9: Backend/provider/translators

- [ ] Ensure ACP selected configuration flows through request creation, response streaming, persistence, restoration, and fork paths.
- [ ] Ensure ACP translator model IDs use complete structured selections.
- [ ] Ensure OpenAI/LiteLLM provider routing remains unchanged.
- [ ] Ensure Bedrock provider routing remains unchanged.
- [ ] Ensure no ACP-only assumptions leak into provider translators.
- [ ] Add integration coverage for OpenCode over LiteLLM.
- [ ] Verify cancellation, steering, tool calls, usage, and session restore with selected config values.

### Phase 10: Validation and cleanup

- [ ] Search for duplicate ACP model/config representations and consolidate them.
- [ ] Remove temporary or dead ACP helper code.
- [ ] Add exhaustive enum matching where applicable.
- [ ] Run formatting.
- [ ] Run targeted ACP/persistence/settings/model tests.
- [ ] Run workspace checks.
- [ ] Run nextest.
- [ ] Run clippy with warnings denied.
- [ ] Review Cargo.lock changes from `cargo update`.
- [ ] Review security implications of registry execution and credential migration.
- [ ] Confirm settings UI, selector, runtime, persistence, registry, and secure storage are synchronized.

## Required validation commands

```bash
./script/format
cargo check --workspace
cargo nextest run --no-fail-fast --workspace --exclude command-signatures-v2
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
cargo test --doc
```

## Completion criteria

The work is complete only when:

1. A configured ACP agent can be discovered and its live models/config options are cached.
2. The normal model selector displays valid ACP model/config choices.
3. Selecting a choice persists the complete structured configuration.
4. A new ACP session receives the selected values before prompting.
5. Dynamic config updates refresh the selector/settings cache.
6. Restored and forked conversations preserve correct selection behavior.
7. Registry metadata can configure/install known agents without excluding custom agents.
8. Credentials use secure storage and are not written to settings.toml.
9. Existing Bedrock/OpenAI/LiteLLM/MCP/auth behavior remains intact.
10. Targeted tests, workspace checks, formatting, nextest, and clippy pass.
