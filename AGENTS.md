# AGENTS.md

This file provides guidance when working with code in this repository.

## Development Commands

### Build and Run
- `cargo run` - Build and run Warp locally
- `cargo bundle --bin warp` - Bundle the main app

### Running with local warp-server
To connect Warp client to a local warp-server instance:

```bash
# Connect to server on default port 8080
cargo run --features with_local_server

# Connect to server on custom port (e.g., 8082)
SERVER_ROOT_URL=http://localhost:8082 WS_SERVER_URL=ws://localhost:8082/graphql/v2 cargo run --features with_local_server
```

Environment variables:
- `SERVER_ROOT_URL` - HTTP endpoint (default: `http://localhost:8080`)
- `WS_SERVER_URL` - WebSocket endpoint (default: `ws://localhost:8080/graphql/v2`)

### Testing
- `cargo nextest run --no-fail-fast --workspace --exclude command-signatures-v2` - Run tests with nextest
- `cargo nextest run -p galaxy_completer --features v2` - Run completer tests with v2 features
- `cargo test --doc` - Run doc tests
- `cargo test` - Run standard tests for individual packages

### Linting and Formatting
- `./script/presubmit` - Run all presubmit checks (fmt, clippy, tests)
- `./script/format` - Format code
- `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings` - Run clippy
- `./script/run-clang-format.py -r --extensions 'c,h,cpp,m' ./crates/galaxyui/src/ ./app/src/` - Format C/C++/Obj-C code
- `find . -name "*.wgsl" -exec wgslfmt --check {} +` - Check WGSL shader formatting

### Bedrock Diagnostics
- Set `GALAXY_BEDROCK_DIAGNOSTICS=1` to enable Bedrock diagnostic output, including:
  - `Error_<timestamp>.txt` snapshot files written to the repository root on request/stream failures (includes the serialized Bedrock context window, tool definitions, protobuf request debug payload, captured Bedrock diagnostic lines, and log tails)
  - Per-event Bedrock diagnostic logs written to `bedrock-diagnostics.log` in the active Warp log directory

### AI Provider Architecture

Galaxy supports session-owned ACP backends and direct model providers. Direct-provider selection is
controlled by settings (`ai.openai.enabled` takes priority over `ai.bedrock.enabled`), but provider
configuration never selects lifecycle ownership.

```
Direct provider: controller.rs → prepare_provider_run() → ProviderRunCoordinator
                                                        ↓ one model call per turn
                                                 AgentRuntime implementation
                                                        ↓ tool batch
                                                 correlated action execution
                                                        ↓ committed results
                                                   next ProviderRun turn

ACP: controller.rs → ResponseStream → acp_output_stream (session-owned lifecycle)
```

**Durable direct-provider run**:
- `crates/galaxy_agent_core/src/provider_run.rs` — Serializable `ProviderRun` state machine, run/epoch identity, bounded model retries, ordered tool batches, cancellation, and terminal outcomes
- `app/src/ai/runtime/provider_run_coordinator.rs` — Drives one-turn `AgentRuntime` calls, validates model events, projects output, and commits exact tool lifecycle events
- `app/src/ai/runtime/rig.rs` — Builds base/CLI request profiles and resolves the configured one-turn runtime; it does not own follow-through
- `app/src/ai/runtime/rig_request.rs` — Converts controller request state into provider-neutral `TurnRequest` history, tools, prompts, and MCP aliases
- `app/src/ai/runtime/event_translator.rs` — Projects provider-neutral runtime events into Warp response events for UI/history compatibility
- `app/src/ai/blocklist/controller.rs` — Retains active runs, correlates actions by `(conversation_id, run_id, epoch, call_id)`, monitors commands, persists checkpoints, and restores interrupted runs
- `app/src/ai/blocklist/controller/response_stream.rs` — Owns ACP transport and shared UI/history projection only; it must not drive direct-provider retries or follow-up turns

**Shared provider types** in `app/src/ai/provider/`:
- `types.rs` — `ConversationMessage`, `MessageRole`, `MessageContent`, `ContentPart`, `ToolDefinition`
- `mod.rs` — `ProviderConfig` enum (Bedrock | OpenAI | None)

**Bedrock provider** in `app/src/ai/bedrock/`:
- `runtime.rs` — Native one-call `AgentRuntime` over `ConverseStream`, including cancellation, reasoning signatures, token usage, and stop/error classification
- `request_translator.rs` — Shared Bedrock message sanitization and tool definitions
- `response_translator.rs` — Compatibility conversion helpers used by tests and background flows
- `convert.rs` — Bedrock request construction and prompt-caching behavior
- `client.rs` — AWS SDK client construction, runtime creation, and independent background streaming calls
- `models.rs` — Model registry and cross-region inference prefix logic
- `discovery.rs` — AWS profile listing and model discovery (STS identity check + ListFoundationModels)
- `diagnostic.rs` — Debug logging (enabled via `GALAXY_BEDROCK_DIAGNOSTICS=1`)
- `external_config.rs` — Fallback config from Claude Code/OpenCode settings

**OpenAI-compatible providers**:
- Direct turns use one-call runtimes from `galaxy_agent_rig` selected in `app/src/ai/runtime/rig.rs` for OpenAI/LiteLLM, ChatGPT subscription, Anthropic, Gemini, and Vertex AI
- `app/src/ai/openai/request_translator.rs` sanitizes provider-neutral history for OpenAI-compatible APIs
- `app/src/ai/openai/client.rs`, `convert.rs`, and `response_translator.rs` remain compatibility/background transport helpers, not lifecycle owners

**Provider settings** (in settings TOML):
- `ai.bedrock.enabled` — Use AWS Bedrock directly (default: true)
- `ai.openai.enabled` — Use OpenAI-compatible endpoint(s) (default: false, takes priority over Bedrock)
- `ai.openai.base_url` — Legacy single-provider endpoint URL (default: `http://localhost:4000/v1`)
- `ai.openai.api_key` — Legacy single-provider API key (stored in keychain)
- `ai.openai.model` — Model name override sent to the endpoint
- `ai.openai.models` — Legacy single-provider model list (`Vec<OpenAIModelConfig>`)
- `ai.providers` — **Multi-provider config** (`Vec<OpenAIProviderConfig>`): each entry has `name`, `base_url`, `api_key`, `models[]`

**Multi-provider example** (settings.toml):
```toml
[ai.openai]
enabled = true

[[ai.providers]]
name = "LiteLLM"
base_url = "http://localhost:4000/v1"
api_key = "sk-..."

[[ai.providers.models]]
model_id = "claude-sonnet-4-20250514[1m]"
display_name = "Claude Sonnet 4 (1M)"
context_size = 1000000

[[ai.providers]]
name = "Ollama (Local)"
base_url = "http://localhost:11434/v1"

[[ai.providers.models]]
model_id = "llama3.2"
display_name = "Llama 3.2"
context_size = 128000
```

**OpenAI/LiteLLM model discovery**:
- Models can be auto-fetched from the `/models` endpoint via the Settings > OpenAI / LiteLLM page
- For each model, the system probes `{model_id}[1m]` with a minimal chat completion request
- If the `[1m]` variant is accepted (HTTP 200 or 429), it's used with 1M context window
- Otherwise, the base model ID is used with its reported context size
- Models injected via `ai.providers[]` are routed to their specific endpoint (per-model routing map)
- Provider name shown as the description label in the model picker; icon shows OpenAI logo for all OpenAI-compatible providers

Key invariants:
- Every direct-provider `AgentRuntime::start_turn` performs exactly one model call; only `ProviderRun` may schedule another turn or retry
- Direct-provider model calls allow 120 seconds for stream startup and 300 seconds between stream events; either timeout is a recoverable transport failure that enters the existing bounded retry lifecycle with the same work identity
- Direct-provider remote telemetry records requested, started, retry-scheduled, and finished model-turn phases with explicit `llm_finished` state; root `provider_run_finished` records distinguish clean completion from failure or cancellation and mark the response stream terminal
- `use_rig` and provider selection may choose request/transport details but must never choose lifecycle ownership
- Direct-provider output may be projected through `ResponseStream`, but provider progress must not depend on response-stream result draining or `AfterStreamFinished`
- A clean direct-provider `ProviderRunOutcome::Completed` explicitly finalizes the conversation as `Success` after terminal output projection, even if earlier turns added tool actions; child-completion waits rely on that status
- Provider actions and results must correlate by `(conversation_id, run_id, epoch, call_id)`; stale or duplicate callbacks must not advance a run
- Active provider runs must checkpoint before external work, persist without credentials, normalize unsafe restored states, and reconcile command state before continuing
- Known tools are in `KNOWN_TOOLS` in `response_translator.rs`; definitions are built by `tool_definition_for_name()` in `convert_request.rs`
- Direct-provider normal and plan turns must advertise `read_plan`, `create_plan`, and `edit_plan` when the matching document capabilities are enabled; plan-creation requests should call `create_plan` after research rather than only returning prose
- Unknown or invalid tool calls receive one correlated synthetic error result and a visible `AgentOutput` message; the durable run owns any continuation
- `recall_tool_history` is an inline completed tool batch. `ProviderRun` commits its synthetic result and starts a bounded next turn without routing it through client action execution
- `recall_tool_history` excludes earlier calls to itself; archived tool results remain searchable by query or exact `tool_use_id`
- Before progressive summarization drains messages, `ConversationMessage::archive_tool_results()` moves tool-use/result pairs into `tool_result_archive`
- Bedrock prompt caching uses three cache points: system prompt, second-to-last history message, and tool configuration
- `ensure_tool_results_paired()` enforces Bedrock's invariant that every `tool_use` has a matching `tool_result`
- Progressive summaries are prepended to provider requests as a user/assistant pair; background summarization remains independent of the active provider run
- Loop prevention in `controller.rs` detects repeated tool failures (3+ identical) and injects a corrective instruction
- Direct-provider long-running shell follow-ups retain CLI tasks as UI projections while command monitoring and completion stay owned by the same provider run
- Direct-provider completed-command assessments are hidden, tool-free root-task turns whose output and hidden input survive CLI-task deactivation and restoration
- ACP remains a separate session-owned runtime; direct-provider cleanup must not move ACP lifecycle into `ProviderRun`
- Orchestrated child conversations are leaf workers: nested `RunAgents` and legacy `StartAgent` calls are rejected before autonomy or permission bypasses, and child requests do not advertise delegation tools
- Direct-provider `RunAgents` remains pending until every local child reaches `Success`, `Error`, or `Cancelled`, or is removed/deleted; recoverable `Blocked`, `TransientError`, and `WaitingForEvents` states remain pending, and the hosted 30-second startup timeout must not apply to these completion waits

### Platform Setup
- `./script/bootstrap` - Platform-specific setup plus common agent skill installation from `skills-lock.json`; prompts for project/global when an install or update is needed unless a target flag or environment override is provided.
- `./script/bootstrap --skip-common-skills` - Platform setup without installing or updating common agent skills.
- `./script/bootstrap --install-common-skills` - Explicitly install common agent skills from `skills-lock.json`; this is the default behavior.
- `./script/bootstrap --install-common-skills-in-repo` - Platform setup plus common agent skill installation in this checkout's `.agents/skills`.
- `./script/bootstrap --install-common-skills-globally` - Platform setup plus common agent skill installation in `~/.agents/skills`.
- `../common-skills/scripts/install_common_skills --repo-root "$PWD" --project --if-needed` - Install or refresh shared agent skills in this checkout's `.agents/skills`.
- `../common-skills/scripts/install_common_skills --repo-root "$PWD" --global --if-needed` - Install or refresh shared agent skills in `~/.agents/skills`.
- `../common-skills/scripts/remove_common_skills --repo-root "$PWD"` - Remove shared agent skills listed in `skills-lock.json` from this checkout's `.agents/skills`.
- `../common-skills/scripts/remove_common_skills --repo-root "$PWD" --global` - Remove shared agent skills listed in `skills-lock.json` from `~/.agents/skills`.
- `../common-skills/scripts/remove_common_skills --repo-root "$PWD" --clear-lock` - Remove shared agent skills from this checkout and delete `skills-lock.json`.
- `./script/install_cargo_build_deps` - Install Cargo build dependencies
- `./script/install_cargo_test_deps` - Install Cargo test dependencies

`skills-lock.json` is the standard project lock file managed by `npx skills`. `warpdotdev/common-skills/scripts/install_common_skills` requires an explicit install target before restoring: pass `--project`, pass `--global`, set `WARP_COMMON_SKILLS_INSTALL_TARGET`, or answer the interactive prompt from bootstrap. Non-interactive flows fail if no target is explicit. The installer creates `skills-lock.json` from `warpdotdev/common-skills` if it is missing, uses global as the recommended interactive default, errors if common skills are present in both project and global locations, prevents a global install pinned to one lock from being silently overwritten by another checkout pinned to a different lock, and verifies installed skills against the lock after successful install or skip paths. `script/run` and `script/bootstrap` execute this installer with `script/resolve_common_skills`, which uses `WARP_COMMON_SKILLS_SCRIPTS_DIR` only when explicitly set and otherwise runs the raw script from `warpdotdev/common-skills`. To test a remote common-skills branch, set `WARP_COMMON_SKILLS_REF=<branch>`. Cloud setup should use `common-skills/scripts/install_common_skills --repo-root <warp-checkout> --project --if-needed --non-interactive` or set `WARP_COMMON_SKILLS_INSTALL_TARGET=project` to avoid the prompt. To update the locked common skills, run `npx --yes skills@1.5.6 update -p -y` and commit the resulting `skills-lock.json` changes.

## Architecture Overview

This is a Rust-based terminal emulator with a custom UI framework called **GalaxyUI**.

### Key Components

**GalaxyUI Framework** (`ui/`):
- Custom UI framework with Entity-Component-Handle pattern
- Global `App` object owns all views/models (entities)
- Views hold `ViewHandle<T>` references to other views
- `AppContext` provides temporary access to handles during render/events
- Elements describe visual layout (Flutter-inspired)
- Actions system for event handling
- MouseStateHandle must be created once during construction, and then referenced/cloned anywhere we're using mouse input to track mouse changes. Inline `MouseStateHandle::default()` while rendering will cause no mouse interactions to work.

**Main App** (`app/`):
- Terminal emulation and shell management (`terminal/`)
- AI integration including Agent Mode (`ai/`)
- Cloud synchronization and Drive features (`drive/`)
- Authentication and user management (`auth/`)
- Settings and preferences (`settings/`)
- Workspace and session management (`workspace/`)

**Core Libraries**:
- `crates/galaxy_core/` - Core utilities and platform abstractions
- `crates/editor/` - Text editing functionality
- `crates/galaxyui/` and `crates/galaxyui_core/` - Custom UI framework
- `crates/ipc/` - Inter-process communication
- `crates/graphql/` - GraphQL client and schema

### Key Architectural Patterns

1. **Entity-Handle System**: Views reference other views via handles, not direct ownership
2. **Modular Structure**: Workspace contains multiple workspace configurations, each with terminals, notebooks, etc.
3. **Cross-Platform**: Native implementations for macOS, Windows, Linux, plus WASM target
4. **AI Integration**: Built-in AI assistant with context awareness and codebase indexing
5. **Cloud Sync**: Objects can be synchronized across devices via Galaxy Drive

### Development Guidelines

**Workspace Structure**:
- This is a Cargo workspace with 60+ member crates
- Main binary is in `app/`, UI framework in `crates/galaxyui/`
- Platform-specific code is conditionally compiled
- Integration tests are in `crates/integration/`

**Coding Style Preferences**:
- Avoid unnecessary type annotations, especially in closure params.
- Avoid using too many Rust path qualifiers and use imports for concision. Place import statements at the top of the file as per convention.
  An exception to this is inside cfg-guarded code branches. In those cases, you can either embed the import into the relevant scope or just use an absolute path for one-offs.
- If a function takes a context parameter (`AppContext`, `ViewContext`, or `ModelContext`), it should be named `ctx` and go last. The one exception is for
  functions that take a closure parameter, in which case the closure should be last.
- Always remove unused parameters completely rather than prefixing them with `_`. Update the function signature and all call sites accordingly.
- Prefer inline format arguments in macros like `println!`, `eprintln!`, and `format!` (for example, `eprintln!("{message}")` instead of `eprintln!("{}", message)`) to satisfy Clippy's `uninlined_format_args` lint.
- Do not pass `Itertools::format` results directly to logging macros (`log::*`, `safe_*`, etc.). `Itertools::format` produces a single-use formatter, while logging implementations may format a message more than once. Use a reusable `String` such as `iter.join(", ")` for logging arguments instead. Direct use in `format!` or `write!` is fine.
- Do not remove existing comments when making unrelated changes. Only remove or modify a comment if the logic it describes has changed.
- When adding a toggleable setting, also add the matching Command Palette enable/disable entry and any required context flags so the setting is discoverable outside Settings.

**Terminal Model Locking**:
- Be extremely careful when calling `model.lock()` on the terminal model (`TerminalModel`). Acquiring multiple locks on the same model from different call sites can cause a deadlock, resulting in a UI freeze (beach ball on macOS).
- Before adding a new `model.lock()` call, verify that no caller in the current call stack already holds the lock.
- Prefer passing already-locked model references down the call stack rather than acquiring new locks.
- If you must lock the model, keep the lock scope as short as possible and avoid calling other functions that might also attempt to lock.

**Testing**:
- Use `cargo nextest` for parallel test execution
- Integration tests use custom framework in `integration/`
- Tests should be run via presubmit script before submitting
- Unit tests should be placed in separate files using the naming convention `${filename}_tests.rs` or `mod_test.rs`
- Test files should be included at the end of their corresponding module with:
  ```rust
  #[cfg(test)]
  #[path = "filename_tests.rs"]  // or "mod_test.rs"
  mod tests;
  ```

**Pull Request Workflow**:
- **ALWAYS** run `./script/format` and `cargo clippy` (the versions specified in ./script/presubmit) before opening a PR or pushing updates to an existing PR branch
- Those commands must pass completely before creating or updating a pull request
- Specifically, ensure `./script/format` and `cargo clippy` checks pass
- If they fail, fix all issues before proceeding with the PR
- Do not create public pull requests or public issues that disclose a non-public security vulnerability. Refer users to `SECURITY.md` for the proper disclosure methods instead.
- This applies to:
  - Opening new pull requests
  - Pushing new commits to existing PR branches
  - Any branch updates that will be reviewed
 - When opening PRs, use the PR template at `.github/pull_request_template.md`
 - Add changelog entries when appropriate using the format at the bottom of the PR template. Use the following prefixes (without the `{{}}` brackets):
   - `CHANGELOG-NEW-FEATURE:` for new, relatively sizable features (use sparingly - these may get marketing/docs)
   - `CHANGELOG-IMPROVEMENT:` for new functionality of existing features
   - `CHANGELOG-BUG-FIX:` for fixes related to known bugs or regressions
   - `CHANGELOG-IMAGE:` for GCP-hosted image URLs
   - Leave changelog lines blank or remove them if no changelog entry is needed

**Database**:
- Uses Diesel ORM with SQLite
- Migrations in `migrations/` directory
- Schema defined in `app/src/persistence/schema.rs`
- Database file is `galaxy.sqlite` (renamed from Warp's `warp.sqlite`); legacy filename migration is handled in `init_db()`

**Session Restoration**:
- Controlled by `general.restore_session` setting
- App state (windows, tabs, pane tree, CWD, agent conversations) is snapshotted to SQLite on window events (close, move, resize, focus change)
- `TerminalView::active_session_path_if_local()` provides the CWD for each pane; falls back to `session_startup_path` for agent-mode or fresh tabs
- Agent conversations are persisted via `BlocklistAIHistoryEvent` → `ModelEvent::UpsertAIQuery` and restored via `RestoredAgentConversations` singleton
- The `active_conversation_id` field in `TerminalPaneSnapshot` controls whether agent view restores in fullscreen mode

**GraphQL**:
- Schema and client code generation from `crates/galaxy_graphql_schema/api/schema.graphql`
- TypeScript types generated for frontend integration

### Feature Flags

Warp uses compile-time feature flags with a small runtime plumbing layer.

How to add a feature flag:
- Add a new variant to `galaxy_core/src/features.rs` in the `FeatureFlag` enum
- (Optional) Enable it by default for dogfood builds by listing it in `DOGFOOD_FLAGS`
- Gate code paths with `FeatureFlag::YourFlag.is_enabled()`
- For preview or release rollout, add to `PREVIEW_FLAGS` or `RELEASE_FLAGS` respectively (as appropriate)

Best practices:
- **Prefer runtime checks over cfg directives**: Prefer `FeatureFlag::YourFlag.is_enabled()` over `#[cfg(...)]` compile-time directives so flags can be toggled without recompilation and are easier to clean up later. Use `#[cfg(...)]` only when the code cannot compile without them (for example, platform-specific code or dependencies that do not exist when the feature is disabled).
- Keep flags high-level and product-focused rather than per-call-site
- Remove the flag and dead branches after launch has stabilized
- For UI sections that expose a new feature, hide the UI behind the same flag

Example:
```rust
#[derive(Sequence)]
pub enum FeatureFlag {
    YourNewFeature,
}

// Default-on for dogfood builds
pub const DOGFOOD_FLAGS: &[FeatureFlag] = &[
    FeatureFlag::YourNewFeature,
];

// Use in code
if FeatureFlag::YourNewFeature.is_enabled() {
    // gated behavior
}
```

### Code Editor IntelliSense (LSP Completion)

The code editor has full LSP-powered autocompletion with documentation resolution:

**Key files:**
- `app/src/code/completion.rs` — Completion state, rendering (menu + docs panel), resolve logic
- `app/src/code/local_code_editor.rs` — Keybindings and action handling

**Behavior:**
- Auto-completes as you type (triggered by alphanumeric/underscore with 50ms debounce)
- Trigger characters: `.` and `::` fire immediately
- Manual trigger: `Ctrl+Alt+Space`
- Keyboard navigation: Up/Down to select, Tab/Enter to confirm
- Mouse: hover an item to select it and show docs, click to confirm
- Documentation panel appears beside the menu when the LSP returns docs for the selected item (via `completionItem/resolve`)

**Architecture:**
- `CompletionState::Showing` holds items, filtered indices, per-item `MouseStateHandle`s, and resolved docs
- `resolve_selected_completion_docs()` sends `completionItem/resolve` to the LSP server
- The docs panel renders markdown via `FormattedTextElement` in a scrollable container beside the menu

### Exhaustive Matching

When adding/editing match statements, avoid using the wildcard _ when at all possible. Exhaustive matching is helpful for ensuring that all variants are handled, especially when adding new variants to enums in the future.

### Rules System

Global rules (behavioral instructions for the AI agent) are stored as `AIFact::Memory` cloud objects and managed via the Rules settings pane.

Key files:
- `app/src/ai/facts/mod.rs` — `AIFact` / `AIMemory` data model
- `app/src/ai/facts/predefined_rules.rs` — Default system-defined rules (seeded on first launch)
- `app/src/ai/facts/view/rule.rs` — `RuleView` UI with Global/Project tabs and "Add Predefined Rules" button
- `app/src/ai/facts/view/mod.rs` — `AIFactView` parent container (Rules + RuleEditor pages)
- `app/src/ai/facts/manager.rs` — `AIFactManager` singleton for pane tracking
- `app/src/settings/ai.rs` — `has_seeded_predefined_rules` setting (one-time flag)

Behavior:
- On first launch (no existing global rules and `has_seeded_predefined_rules` is false), predefined rules are automatically created
- The "Add Predefined Rules" button in the Global rules tab will add/update system-defined rules (identified by the "System Defined Rule" name prefix)
- Rules are persisted via the cloud object sync system (`UpdateManager::create_ai_fact` / `update_ai_fact`)
- The `memory_enabled` setting (`agents.knowledge.rules_enabled`) controls whether rules are sent to the AI

### Appearance Settings Notes

- Galaxy's built-in brand themes are available as `GalaxyDark` and `GalaxyDay`.
- UI font selection is persisted in `appearance.text.ui_font_name` and uses an empty string as the system-default sentinel.
- The one-click Galaxy brand preset is implemented in `app/src/settings_view/appearance_page.rs` and applies:
  - Galaxy Dark/Day system theme mapping
  - terminal + AI font defaults
  - the bundled, SIL Open Font License-licensed Roboto UI font
