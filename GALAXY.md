# GALAXY.md

This file provides guidance when working with code in this repository.

## Development Commands

### Build and Run
- `cargo run` - Build and run Galaxy locally (default binary: `galaxy-ai-oss`)
- `cargo run --bin galaxy-ai` - Run the local/dev binary

### Testing
- `cargo nextest run --no-fail-fast --workspace --exclude command-signatures-v2` - Run tests with nextest
- `cargo nextest run -p galaxy_completer --features v2` - Run completer tests with v2 features
- `cargo test --doc` - Run doc tests

### Linting and Formatting
- `./script/presubmit` - Run all presubmit checks (fmt, clippy, tests)
- `cargo fmt` - Format code
- `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings` - Run clippy

### Platform Setup
- `./script/bootstrap` - Platform-specific setup
- `./script/install_cargo_build_deps` - Install Cargo build dependencies
- `./script/install_cargo_test_deps` - Install Cargo test dependencies

## Architecture Overview

This is a Rust-based terminal emulator with a custom UI framework called **GalaxyUI**.

### Key Components

**GalaxyUI Framework** (`crates/galaxyui/`, `crates/galaxyui_core/`, `crates/galaxyui_extras/`):
- Custom UI framework with Entity-Component-Handle pattern
- Global `App` object owns all views/models (entities)
- Views hold `ViewHandle<T>` references to other views
- `AppContext` provides temporary access to handles during render/events
- Elements describe visual layout (Flutter-inspired)
- Actions system for event handling
- MouseStateHandle must be created once during construction, and then referenced/cloned anywhere we're using mouse input to track mouse changes. Inline `MouseStateHandle::default()` while rendering will cause no mouse interactions to work.

**Main App** (`app/`):
- Terminal emulation and shell management (`terminal/`)
- AI integration via Amazon Bedrock (`ai/`)
- Cloud synchronization and Drive features (`drive/`)
- Authentication and user management (`auth/`)
- Settings and preferences (`settings/`)
- Workspace and session management (`workspace/`)

**Core Libraries**:
- `crates/galaxy_core/` - Core utilities and platform abstractions
- `crates/editor/` - Text editing functionality
- `crates/graphql/` - GraphQL client and schema
- `crates/galaxy_terminal/` - Terminal emulation
- `crates/galaxy_util/` - Shared utilities

### AI Architecture

Galaxy uses **Amazon Bedrock** as the sole AI provider. The client calls Bedrock directly:
- `app/src/ai/bedrock/client.rs` - AWS Bedrock client (`BedrockClient::converse_stream`)
- `app/src/ai/bedrock/convert_request.rs` - System prompt construction, message/tool extraction
- `app/src/ai/bedrock/convert.rs` - Conversion to Bedrock wire format
- `app/src/ai/bedrock/tool_docs.rs` - Tool documentation (served via `get_tool_documentation` meta-tool)
- `app/src/ai/bedrock/stream.rs` - Response stream processing

System prompt is dynamically built from request context (OS, shell, pwd, git, project rules, global rules, skills, MCP servers).

Global rules are loaded from `~/.galaxy-ai/rules/*.md` (filename = rule name, content = rule text).

Project rules are loaded from `GALAXY.md` or `AGENTS.md` files found in the project directory tree.

### Key Architectural Patterns

1. **Entity-Handle System**: Views reference other views via handles, not direct ownership
2. **Modular Structure**: Workspace contains multiple workspace configurations, each with terminals, notebooks, etc.
3. **Cross-Platform**: Native implementations for macOS, Windows, Linux, plus WASM target
4. **AI Integration**: Built-in AI assistant with context awareness and codebase indexing
5. **Cloud Sync**: Objects can be synchronized across devices via Galaxy Drive

### Development Guidelines

**Workspace Structure**:
- This is a Cargo workspace with 56 member crates
- Main binary is in `app/`, UI framework in `crates/galaxyui*/`
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
- Do not remove existing comments when making unrelated changes. Only remove or modify a comment if the logic it describes has changed.

**Terminal Model Locking**:
- Be extremely careful when calling `model.lock()` on the terminal model (`TerminalModel`). Acquiring multiple locks on the same model from different call sites can cause a deadlock, resulting in a UI freeze (beach ball on macOS).
- Before adding a new `model.lock()` call, verify that no caller in the current call stack already holds the lock.
- Prefer passing already-locked model references down the call stack rather than acquiring new locks.
- If you must lock the model, keep the lock scope as short as possible and avoid calling other functions that might also attempt to lock.

**Testing**:
- Use `cargo nextest` for parallel test execution
- Integration tests use custom framework in `crates/integration/`
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
- If they fail, fix all issues before proceeding with the PR
- When opening PRs, use the PR template at `.github/pull_request_template.md`

**Database**:
- Uses Diesel ORM with SQLite
- Migrations in `migrations/` directory
- Schema defined in `app/src/persistence/schema.rs`

### Feature Flags

Galaxy uses compile-time feature flags with a small runtime plumbing layer.

How to add a feature flag:
- Add a new variant to `galaxy_features/src/lib.rs` in the `FeatureFlag` enum
- (Optional) Enable it by default for dogfood builds by listing it in `DOGFOOD_FLAGS`
- Gate code paths with `FeatureFlag::YourFlag.is_enabled()`
- For preview or release rollout, add to `PREVIEW_FLAGS` or `RELEASE_FLAGS` respectively

Best practices:
- **Prefer runtime checks over cfg directives**: Prefer `FeatureFlag::YourFlag.is_enabled()` over `#[cfg(...)]` compile-time directives so flags can be toggled without recompilation and are easier to clean up later. Use `#[cfg(...)]` only when the code cannot compile without them.
- Keep flags high-level and product-focused rather than per-call-site
- Remove the flag and dead branches after launch has stabilized
- For UI sections that expose a new feature, hide the UI behind the same flag

### Exhaustive Matching

When adding/editing match statements, avoid using the wildcard _ when at all possible. Exhaustive matching is helpful for ensuring that all variants are handled, especially when adding new variants to enums in the future.

### Appearance Settings Notes

- Samsung-inspired built-in themes are available as `SamsungDark` and `SamsungLight`.
- UI font selection is persisted in `appearance.text.ui_font_name` and uses an empty string as the system-default sentinel.
- The one-click Samsung brand preset is implemented in `app/src/settings_view/appearance_page.rs` and applies:
  - Samsung dark/light theme mapping
  - terminal + AI font defaults
  - a best-available Samsung-style UI font fallback

### Configuration

- User config directory: `~/.galaxy-ai/` (channel-suffixed for non-stable: `-dev`, `-oss`, etc.)
- Global AI rules: `~/.galaxy-ai/rules/*.md`
- Skills: `~/.galaxy-ai/skills/`
- MCP config: `~/.galaxy-ai/.mcp.json`
- Environment variables use `GALAXY_` prefix (e.g., `GALAXY_API_KEY`, `GALAXY_INTEGRATION`)

## Future Work

### IDE-Level LSP Integration
Add Zed-quality IDE capabilities on top of Galaxy's existing LSP client (`crates/lsp/`) and editor (`crates/editor/`). Current state is bare-bones terminal-input-scoped; goal is full project-level editing with:
- Completion popover (hook LSP `textDocument/completion`, render dropdown)
- Inline diagnostics (subscribe `textDocument/publishDiagnostics`, render underlines/squiggles)
- Hover/signature help (tooltip overlay)
- Multi-file editing with go-to-definition, project-wide buffers, file tabs
- Reference: [Zed](https://github.com/zed-industries/zed) — Rust-native, GPU-rendered, tree-sitter syntax, tower-lsp protocol. Architecturally compatible with Galaxy's primitives.
