# ACP Implementation Task List

Status key: `[ ]` not started · `[~]` in progress · `[x]` complete · `[!]` blocked

Execution mode: autonomous batch implementation. Continue through all phases without pausing for confirmation; stop only for a genuine external blocker or after final validation.

## Phase 1 — Shared types

- [~] Define shared normalized ACP agent metadata.
- [~] Define shared ACP capability representation.
- [x] Define shared ACP config option/value representation.
- [~] Define structured `AcpModelSelection`.
- [x] Add protocol-to-Galaxy conversion helpers.
- [ ] Add selection identity/equality helpers.
- [ ] Replace avoidable ad hoc JSON representations.
- [ ] Add conversion and ordering tests.
- [x] Persist ACP selections in `AcpConversationData`.
- [x] Preserve selections across forks while clearing session IDs.

## Phase 2 — ACP discovery

- [ ] Advertise supported boolean config options during initialization.
- [x] Capture `initialize.agentInfo`.
- [x] Capture initialization capabilities.
- [x] Add manager-level metadata accessors.
- [x] Add manager-level discovery API.
- [x] Create temporary discovery sessions.
- [x] Read `configOptions` from `session/new`.
- [x] Preserve option/value ordering.
- [x] Remove placeholder synchronous config accessor once async discovery is available.
- [ ] Support custom executable discovery.
- [ ] Close/delete temporary sessions when supported.
- [ ] Preserve last-known-good cache on failure.
- [ ] Add timeout/cancellation handling.
- [ ] Add fake-agent discovery tests.

## Phase 3 — Runtime config handling

- [x] Carry selected config values in `AcpTurnRequest`.
- [x] Apply selected values after `session/new`.
- [x] Export `SessionConfigOptionValue` from `galaxy_acp`.
- [~] Capture `session/set_config_option` responses.
- [ ] Replace complete current config state after setting an option.
- [x] Add async runtime result channels for discovery/config state.
- [x] Emit `ConfigOptions` for `config_option_update` notifications.
- [ ] Reconcile dependent options.
- [ ] Reject/drop stale selections safely.
- [ ] Reconcile restored sessions with current agent config.
- [x] Track latest config options in session metadata.
- [ ] Enforce all capability-gated behavior.
- [ ] Add runtime config tests.

## Phase 4 — Settings.toml

- [x] Add `ai.acp.agents` setting.
- [x] Store agent metadata/config options in settings.
- [~] Store capabilities in normalized form.
- [ ] Store discovery timestamps/source/version.
- [~] Wire discovery results into `AISettings.acp_agents`.
- [ ] Atomically persist discovery results.
- [ ] Invalidate cache on launch/version changes.
- [ ] Preserve custom launch settings during refresh.
- [ ] Refresh model preferences when settings change.
- [ ] Handle malformed/stale cache values.
- [ ] Add settings schema/round-trip tests.
- [ ] Add documented settings example.

## Phase 5 — ACP Registry

- [ ] Implement registry client.
- [ ] Parse registry version and agents.
- [ ] Parse npm/npx distributions.
- [ ] Parse uvx distributions.
- [ ] Parse platform binary distributions.
- [ ] Select OS/architecture distribution.
- [ ] Preserve command/args/env/archive/checksum/icon/version metadata.
- [ ] Validate registry commands, URLs, environment names, and checksums.
- [ ] Cache last-known-good registry data.
- [ ] Add explicit user-confirmed installation flow.
- [ ] Preserve custom agents.
- [ ] Add registry tests.

## Phase 6 — Model selector

- [~] Inject ACP choices through `LLMPreferences`.
- [ ] Use structured selection data instead of parsing IDs.
- [ ] Group entries by ACP agent.
- [x] Display model/mode labels such as `GPT 5.4 Sol (Ultra)`.
- [ ] Use `model` as primary selector dimension.
- [ ] Use `mode`/`thought_level` as secondary display/configuration.
- [ ] Keep `model_config`/unknown options in advanced configuration UI.
- [ ] Avoid invalid dependent combinations.
- [ ] Refresh choices after discovery/config updates.
- [ ] Propagate selected values into conversation persistence.
- [ ] Use selected identity in transcripts/usage/restoration.
- [ ] Add selector propagation tests.

## Phase 7 — Agent Settings UI

- [ ] Add registry-backed agent list.
- [ ] Show icon/name/description/version/install status.
- [ ] Add registry refresh action.
- [ ] Add ACP capability discovery action.
- [ ] Show discovery status/timestamp/errors.
- [ ] Show discovered models/modes/options.
- [ ] Show advanced options without duplicating selector logic.
- [ ] Add explicit install/configure confirmation.
- [ ] Preserve custom command/args.
- [ ] Use shared UI themes/components.
- [ ] Add settings UI tests.

## Phase 8 — Credentials

- [x] Keep global Keychain integration intact.
- [ ] Audit ACP/OpenAI/LiteLLM plaintext credential paths.
- [ ] Store Galaxy-managed ACP/provider credentials in secure storage.
- [ ] Keep only non-secret references in settings.toml.
- [ ] Migrate existing plaintext credentials.
- [ ] Define unavailable-secure-storage behavior.
- [ ] Keep ACP-owned credentials under ACP agent control.
- [ ] Add credential migration/security tests.

## Phase 9 — Backend/translators

- [~] Verify selection flow from UI to request creation.
- [x] Verify persistence/restoration/fork flow.
- [~] Use complete structured ACP identity in translators.
- [ ] Preserve OpenAI/LiteLLM routing.
- [ ] Preserve Bedrock routing.
- [ ] Keep ACP assumptions isolated from provider translators.
- [ ] Add OpenCode + LiteLLM integration coverage.
- [ ] Verify cancellation/steering/tools/usage/session restore.

## Phase 10 — Cleanup and validation

- [ ] Remove duplicate ACP representations.
- [ ] Remove dead/temporary ACP helpers.
- [ ] Review exhaustive matches.
- [ ] Run `./script/format`.
- [ ] Run targeted ACP/persistence/settings/model tests.
- [ ] Run `cargo check --workspace`.
- [ ] Run nextest.
- [ ] Run clippy with warnings denied.
- [ ] Run doc tests.
- [ ] Review Cargo.lock changes.
- [ ] Review registry execution security.
- [ ] Review credential migration security.
- [ ] Confirm Settings, Agent screen, selector, runtime, persistence, registry, and secure storage are synchronized.

## Completion gates

- [ ] Live ACP discovery works for OpenCode and Codex.
- [ ] Discovered options persist in `ai.acp.agents`.
- [ ] Selector shows valid ACP model/config choices.
- [ ] Selected structured values reach `session/set_config_option` before prompting.
- [ ] Dynamic agent config updates refresh Galaxy state.
- [ ] Restored/forked conversations retain correct ACP selections.
- [ ] Registry metadata supports known and custom agents.
- [ ] Credentials are not written to settings.toml.
- [ ] Existing Bedrock/OpenAI/LiteLLM/MCP/auth behavior is unchanged.
- [ ] All required validation commands pass.
