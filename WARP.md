# WARP.md

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
- `cargo nextest run -p warp_completer --features v2` - Run completer tests with v2 features
- `cargo test --doc` - Run doc tests
- `cargo test` - Run standard tests for individual packages

### Linting and Formatting
- `./script/presubmit` - Run all presubmit checks (fmt, clippy, tests)
- `cargo fmt` - Format code
- `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings` - Run clippy
- `./script/run-clang-format.py -r --extensions 'c,h,cpp,m' ./crates/warpui/src/ ./app/src/` - Format C/C++/Obj-C code
- `find . -name "*.wgsl" -exec wgslfmt --check {} +` - Check WGSL shader formatting

### Bedrock Diagnostics
- Set `GALAXY_BEDROCK_DIAGNOSTICS=1` to enable Bedrock diagnostic output, including:
  - `Error_<timestamp>.txt` snapshot files written to the repository root on request/stream failures (includes the serialized Bedrock context window, tool definitions, protobuf request debug payload, captured Bedrock diagnostic lines, and log tails)
  - Per-event Bedrock diagnostic logs written to `bedrock-diagnostics.log` in the active Warp log directory

### Bedrock Translator Architecture
The Bedrock integration uses a **translator service pattern** where Warp proto types flow in, get converted to Bedrock SDK types, and responses are translated back:

```
Warp UI (proto) → translator.rs → request_translator.rs → Bedrock API
Warp UI (proto) ← response_translator.rs ← Bedrock stream
```

Key files in `app/src/ai/bedrock/`:
- `translator.rs` — Orchestrator: takes `api::Request` + config, returns `ResponseStream`
- `request_translator.rs` — Converts Warp proto → Bedrock SDK types (messages, system prompt, tools, sanitization)
- `response_translator.rs` — Converts Bedrock stream events → Warp proto `ResponseEvent`s
- `convert.rs` — Shared types (`ConversationMessage`, `ToolDefinition`) and Bedrock SDK type builders
- `client.rs` — AWS SDK client construction and `converse_stream` call
- `models.rs` — Model registry and cross-region inference prefix logic
- `discovery.rs` — AWS profile listing and model discovery (STS identity check + ListFoundationModels)
- `diagnostic.rs` — Debug logging (enabled via `GALAXY_BEDROCK_DIAGNOSTICS=1`)
- `external_config.rs` — Fallback config from Claude Code/OpenCode settings

Key invariants:
- Known tools are in `KNOWN_TOOLS` constant in `response_translator.rs`
- Tool definitions are built via `tool_definition_for_name()` in `convert_request.rs`; includes `recall_tool_history` for retrieving past tool results
- Unknown/hallucinated tool calls are caught in the stream, paired with synthetic error results, and now emit a visible `AgentOutput` text message to the UI
- `recall_tool_history` is handled inline in the response translator (synthetic result from `messages_sent`)
- Tool result archive: before progressive summarization drains messages, `ConversationMessage::archive_tool_results()` extracts all tool_use/tool_result pairs into a separate `tool_result_archive` vec. `recall_tool_history` searches both live history + archived results, and supports a `tool_use_id` parameter for exact ID lookup
- Prompt caching uses three cache points: system prompt, conversation history (second-to-last message), tool config
- `ensure_tool_results_paired()` enforces Bedrock's invariant that every `tool_use` has a matching `tool_result`
- `inject_input_messages_into_task()` and `extract_user_query_text()` ensure user queries persist for session restore
- The stream emits a `UserQuery` proto message at the start of each response for conversation title
- Progressive summary (if present) is prepended to the messages array as a user/assistant pair in `translator.rs`
- Loop prevention guardrail in `controller.rs` detects repeated tool failures (3+ identical) and injects corrective instructions

### Platform Setup
- `./script/bootstrap` - Platform-specific setup (calls platform-specific bootstrap scripts)
- `./script/install_cargo_build_deps` - Install Cargo build dependencies
- `./script/install_cargo_test_deps` - Install Cargo test dependencies

## Architecture Overview

This is a Rust-based terminal emulator with a custom UI framework called **WarpUI**.

### Key Components

**WarpUI Framework** (`ui/`):
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
- `warp_core/` - Core utilities and platform abstractions
- `editor/` - Text editing functionality
- `ui/` - Custom UI framework
- `ipc/` - Inter-process communication
- `graphql/` - GraphQL client and schema

### Key Architectural Patterns

1. **Entity-Handle System**: Views reference other views via handles, not direct ownership
2. **Modular Structure**: Workspace contains multiple workspace configurations, each with terminals, notebooks, etc.
3. **Cross-Platform**: Native implementations for macOS, Windows, Linux, plus WASM target
4. **AI Integration**: Built-in AI assistant with context awareness and codebase indexing
5. **Cloud Sync**: Objects can be synchronized across devices via Warp Drive

### Development Guidelines

**Workspace Structure**:
- This is a Cargo workspace with 34+ member crates
- Main binary is in `app/`, UI framework in `ui/`
- Platform-specific code is conditionally compiled
- Integration tests are in `integration/`

**Coding Style Preferences**:
- Avoid unnecessary type annotations, especially in closure params.
- Avoid using too many Rust path qualifiers and use imports for concision. Place import statements at the top of the file as per convention.
  An exception to this is inside cfg-guarded code branches. In those cases, you can either embed the import into the relevant scope or just use an absolute path for one-offs.
- If a function takes a context parameter (`AppContext`, `ViewContext`, or `ModelContext`), it should be named `ctx` and go last. The one exception is for
  functions that take a closure parameter, in which case the closure should be last.
- Always remove unused parameters completely rather than prefixing them with `_`. Update the function signature and all call sites accordingly.
- Prefer inline format arguments in macros like `println!`, `eprintln!`, and `format!` (for example, `eprintln!("{message}")` instead of `eprintln!("{}", message)`) to satisfy Clippy's `uninlined_format_args` lint.
- Do not remove existing comments when making unrelated changes. Only remove or modify a comment if the logic it describes has changed.

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
- **ALWAYS** run cargo fmt and cargo clippy (the versions specified in ./script/presubmit) before opening a PR or pushing updates to an existing PR branch
- Those commands must pass completely before creating or updating a pull request
- Specifically, ensure `cargo fmt` and `cargo clippy` checks pass
- If they fail, fix all issues before proceeding with the PR
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
- Schema and client code generation from `graphql/api/schema.graphql`
- TypeScript types generated for frontend integration

### Feature Flags

Warp uses compile-time feature flags with a small runtime plumbing layer.

How to add a feature flag:
- Add a new variant to `warp_core/src/features.rs` in the `FeatureFlag` enum
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

- Samsung-inspired built-in themes are available as `SamsungDark` and `SamsungLight`.
- UI font selection is persisted in `appearance.text.ui_font_name` and uses an empty string as the system-default sentinel.
- The one-click Samsung brand preset is implemented in `app/src/settings_view/appearance_page.rs` and applies:
  - Samsung dark/light theme mapping
  - terminal + AI font defaults
  - a best-available Samsung-style UI font fallback
