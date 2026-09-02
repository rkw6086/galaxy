# Galaxy Refactor — Implementation Plan

> **Superseded:** This Bedrock-only plan no longer represents the product direction.
> Use [`galaxy-local-first-rig.md`](galaxy-local-first-rig.md) and
> [`ui-flow-inventory.md`](ui-flow-inventory.md). This file remains as historical
> context so completed work and earlier decisions are not silently lost.

> **Created:** 2026-05-07
> **Status:** In Progress
> **Current Phase:** Phase 1 — Crate Renaming

---

## Table of Contents

1. [Phase 1: Crate Renaming](#phase-1-crate-renaming)
2. [Phase 2: Feature Removal](#phase-2-feature-removal)
3. [Phase 3: Branding Rename](#phase-3-branding-rename)
4. [Phase 4: System Prompt & Rules Redesign](#phase-4-system-prompt--rules-redesign)
5. [Phase 5: Binary & Build Config](#phase-5-binary--build-config)
6. [Phase 6: Bedrock Improvements](#phase-6-bedrock-improvements)
7. [Phase 7: Verification](#phase-7-verification)
8. [Critical Fixes from REVIEW.md](#critical-fixes-from-reviewmd)

---

## Phase 1: Crate Renaming

**Status:** 🔄 In Progress

Rename all 21 `warp*` crates to `galaxy*` equivalents.

### Mapping

| Current | New | Directory Rename |
|---------|-----|-----------------|
| `warp` (app) | `galaxy` | `app/` (keep) |
| `warp_cli` | `galaxy_cli` | `crates/warp_cli/` → `crates/galaxy_cli/` |
| `warp_core` | `galaxy_core` | `crates/warp_core/` → `crates/galaxy_core/` |
| `warp_completer` | `galaxy_completer` | `crates/warp_completer/` → `crates/galaxy_completer/` |
| `warp_editor` | `galaxy_editor` | `crates/editor/` (keep) |
| `warp_features` | `galaxy_features` | `crates/warp_features/` → `crates/galaxy_features/` |
| `warp_files` | `galaxy_files` | `crates/warp_files/` → `crates/galaxy_files/` |
| `warp_graphql` | `galaxy_graphql` | `crates/graphql/` (keep) |
| `warp_graphql_schema` | `galaxy_graphql_schema` | `crates/warp_graphql_schema/` → `crates/galaxy_graphql_schema/` |
| `warp_isolation_platform` | `galaxy_isolation_platform` | `crates/isolation_platform/` (keep) |
| `warp_js` | `galaxy_js` | `crates/warp_js/` → `crates/galaxy_js/` |
| `warp_logging` | `galaxy_logging` | `crates/warp_logging/` → `crates/galaxy_logging/` |
| `warp_managed_secrets` | `galaxy_managed_secrets` | `crates/managed_secrets/` (keep) |
| `warp_ripgrep` | `galaxy_ripgrep` | `crates/warp_ripgrep/` → `crates/galaxy_ripgrep/` |
| `warp_server_client` | `galaxy_server_client` | `crates/warp_server_client/` → `crates/galaxy_server_client/` |
| `warp_terminal` | `galaxy_terminal` | `crates/warp_terminal/` → `crates/galaxy_terminal/` |
| `warp_util` | `galaxy_util` | `crates/warp_util/` → `crates/galaxy_util/` |
| `warp_web_event_bus` | `galaxy_web_event_bus` | `crates/warp_web_event_bus/` → `crates/galaxy_web_event_bus/` |
| `warpui` | `galaxyui` | `crates/warpui/` → `crates/galaxyui/` |
| `warpui_core` | `galaxyui_core` | `crates/warpui_core/` → `crates/galaxyui_core/` |
| `warpui_extras` | `galaxyui_extras` | `crates/warpui_extras/` → `crates/galaxyui_extras/` |

### Actions

- [ ] Update `name =` in each crate's `Cargo.toml`
- [ ] Update `[workspace.dependencies]` in root `Cargo.toml`
- [ ] Update all `[dependencies]` references across all crates
- [ ] Update all `use warp_*::` / `extern crate warp_*` in source files
- [ ] Update `default-members` in workspace root
- [ ] Rename directories containing "warp"
- [ ] Update workspace `authors` to `Ryan Ward <ryan.ward@samsung.com>`

---

## Phase 2: Feature Removal

**Status:** ⬜ Pending

### 2a: Non-Bedrock AI Providers
- [ ] Remove `LLMProvider::OpenAI`, `::Anthropic`, `::Google`, `::Xai` from enum
- [ ] Remove `ApiKeys` fields: `openai`, `anthropic`, `google`, `open_router`
- [ ] Remove BYO key settings UI editors
- [ ] Remove GraphQL `LlmProvider` non-Bedrock variants
- [ ] Remove `ManagedSecretValue::AnthropicApiKey` (keep Bedrock variants)
- [ ] Remove OpenAI/Anthropic/Google/Fireworks secret detection regex
- [ ] Remove `can_use_warp_credits_with_byok` setting
- [ ] Remove `fallback_to_warp` setting + fallback logic

### 2b: CLI Agent Detection & Third-Party Harnesses
- [ ] Delete plugin manager files: `codex.rs`, `claude.rs`, `gemini.rs`, `opencode.rs` + tests
- [ ] Delete harness files: `claude_code.rs`, `claude_transcript.rs`, `gemini.rs` + dirs + tests
- [ ] Delete `codex_modal.rs`
- [ ] Remove `CLIAgent` enum variants (all third-party agents)
- [ ] Remove icons: `OpenAILogo`, `ClaudeLogo`, `GeminiLogo`, `AmpLogo`, `DroidLogo`, `OpenCodeLogo`, `CopilotLogo`, `Oz`
- [ ] Remove `FeatureFlag::CodexNotifications`, `FeatureFlag::GeminiNotifications`
- [ ] Remove color constants: `OPENAI_COLOR`, `GEMINI_BLUE`

### 2c: Oz/Cloud Agent
- [ ] Remove `Harness::Oz`, `AgentHarness::Oz`
- [ ] Remove Oz CLI commands
- [ ] Remove `cloud_setup_guide_view.rs`

### 2d: Referral System
- [ ] Delete `referrals_page.rs`, `referral.rs` (server_api), `referral_theme_status.rs`
- [ ] Delete 8 `referral-*.svg` assets
- [ ] Remove referral themes, GraphQL queries/mutations
- [ ] Remove `WorkspaceAction::ShowReferralSettingsPage`
- [ ] Remove workspace/auth/resource center referral integration

### 2e: Warp Server Fallback
- [ ] Remove server fallback request path entirely
- [ ] Make Bedrock the ONLY AI path
- [ ] Remove `rules_enabled` server flag

---

## Phase 3: Branding Rename

**Status:** ⬜ Pending

### 3a: Critical Code Identifiers
| Find | Replace |
|------|---------|
| `dev.warp.` | `dev.galaxy.` |
| `warp://` | `galaxy://` |
| `WarpTerminal` | `GalaxyTerminal` |
| `warp-app` | `galaxy-app` |
| `warp-cli` | `galaxy-cli` |
| `warp-bedrock-static` | `galaxy-bedrock-static` |
| `warp-bedrock-discovery` | `galaxy-bedrock-discovery` |
| `X-Warp-` | `X-Galaxy-` |
| `warp.sqlite` | `galaxy.sqlite` |
| `warp-terminal` (Linux pkg) | `galaxy-terminal` |
| `/opt/warpdotdev/` | `/opt/galaxydotdev/` |

### 3b: Environment Variables
- [ ] All `WARP_*` → `GALAXY_*` (~15+ env vars)
- [ ] **CRITICAL:** Fix `WARP_INTEGRATION` split in `app/src/lib.rs:658`
- [ ] **CRITICAL:** Windows TTY env vars (entire file skipped previously)
- [ ] Fix `WARP_CLI_MODE`, `WARP_ENABLE_WAYLAND`, `WARP_PLUGIN_HOST_ADDRESS`
- [ ] Fix `WARP_HONOR_PS1` in remote TTY
- [ ] Fix `WARP_CHANNEL_VERSIONS_PATH` in autoupdate files
- [ ] Fix `WARP_USER_SECRET` build-time env

### 3c: User-Facing Strings
- [ ] "Warp" → "Galaxy" in all display text
- [ ] "Warp AI" → "Galaxy AI"
- [ ] "Warp Drive" → "Galaxy Drive"
- [ ] "Open in Warp" → "Open in Galaxy"
- [ ] OAuth client name `"Warp"` → `"Galaxy AI"` in MCP
- [ ] Windows registry login item keys

### 3d: URLs & Org References
- [ ] `warp.dev` → `galaxy.dev`
- [ ] `warpdotdev` → `galaxydotdev`
- [ ] Git dep URLs
- [ ] Email addresses

### 3e: Platform Files
- [ ] 5 Linux desktop entries (rename files + content)
- [ ] RPM/DEB templates
- [ ] Windows installer
- [ ] `deny.toml`

### 3f: Telemetry/Sentry
- [ ] Tags: `warp.*` → `galaxy.*`
- [ ] Events: "Warp AI *" → "Galaxy AI *"

### 3g: Documentation & Assets
- [ ] README.md, CONTRIBUTING.md, FAQ.md, etc.
- [ ] 35+ asset files with "warp" → "galaxy"
- [ ] `WARP.md` → `GALAXY.md`

---

## Phase 4: System Prompt & Rules Redesign

**Status:** ⬜ Pending

### 4a: Rewrite `extract_system_prompt`
- [ ] Build dynamic system prompt from `request.input.context`
- [ ] Include: identity, environment, tool list (brief), tips, autonomy mode
- [ ] Conditionally include: project rules, global rules, skills, MCP servers

### 4b: Add `get_tool_documentation` Meta-Tool
- [ ] New tool definition in `default_tool_definitions()`
- [ ] Client-side handler returns docs for:
  - Core tools (detailed parameters, guidelines)
  - `"capabilities"` keyword (system overview)
  - MCP tools (schema + description)
- [ ] No filesystem/network — pure string lookup

### 4c: Global Rules Loader
- [ ] New function: `galaxy_home_rules_dir()` → `~/.galaxy-ai/rules/`
- [ ] Glob `*.md`, read contents, inject into system prompt
- [ ] Filename stem = rule name, content = rule text

### 4d: Update Project Rules Pattern
- [ ] Change `RULES_FILE_PATTERN` from `["WARP.md", "AGENTS.md"]` to `["GALAXY.md", "AGENTS.md"]`

### 4e: Remove Cloud Rules Infrastructure
- [ ] Remove `CloudAIFactModel`, `AIFact`/`AIMemory`
- [ ] Remove `JsonObjectType::AIFact` / `GraphQLFormat::JsonAIFact`
- [ ] Remove cloud sync for rules

### 4f: Rules Settings UI
- [ ] Rework `ai/facts/view/rule.rs` for filesystem-based rules
- [ ] List/add/edit/delete rules as `.md` files

---

## Phase 5: Binary & Build Config

**Status:** ⬜ Pending

- [ ] Channel binaries: `stable` → `galaxy-stable`, `dev` → `galaxy-dev`, `preview` → `galaxy-preview`
- [ ] Verify `default-run = "galaxy-ai-oss"`
- [ ] Rename `WARP.md` → `GALAXY.md` at project root
- [ ] Update workspace `authors`
- [ ] Review `build.rs`, `DockTilePlugin/Info.plist`, `script/macos/bundle` (flagged as unreviewed)

---

## Phase 6: Bedrock Improvements

**Status:** ⬜ Pending

Items from REVIEW.md that improve the Bedrock path:

### 6a: Config Struct Refactor
- [ ] Create `BedrockConversationConfig` struct to replace 9-param `converse_stream()` signature

### 6b: Stream Text Buffering Fix
- [ ] Fix `TEXT_FLUSH_THRESHOLD = 20` silent text discard
- [ ] Buffer until semantic boundary (tool call start, stop event) instead of char count

### 6c: Synthesized Tool Results
- [ ] `ensure_tool_results_paired()` — inject meaningful error text instead of empty content
- [ ] Use: `"Error: tool result was not provided for this tool call"`

### 6d: Diagnostic Logger
- [ ] Replace `{:?}` debug format in `log_protobuf_input` with JSON serialization
- [ ] Document `GALAXY_BEDROCK_DIAGNOSTICS=1` as dev-only flag

### 6e: Config Migration Chain
- [ ] Add `.warp` as additional legacy path in migration chain
- [ ] Full chain: `.warp` → `.warp-core` → `.galaxy-ai`

### 6f: needs_create_task Detection
- [ ] Verify `task_context.tasks.is_empty()` can't false-negative with pre-populated tasks
- [ ] Add comment documenting the assumption

---

## Phase 7: Verification

**Status:** ⬜ Pending

- [ ] `cargo check --workspace`
- [ ] `cargo clippy --workspace`
- [ ] Full text grep for remaining "warp" references
- [ ] Verify no broken `use` imports
- [ ] Verify no orphaned module declarations
- [ ] Run integration tests

---

## Critical Fixes from REVIEW.md

These are bugs in the current branch that must be addressed regardless of phase:

| Priority | Issue | File | Phase |
|----------|-------|------|-------|
| 🔴 CRITICAL | `WARP_INTEGRATION`/`GALAXY_INTEGRATION` split | `app/src/lib.rs:658` | 3b |
| 🔴 CRITICAL | Windows TTY env vars entirely skipped | `app/src/terminal/local_tty/windows/environment.rs` | 3b |
| 🟠 HIGH | OAuth client name `"Warp"` sent to external providers | `app/src/ai/mcp/templatable_manager/oauth.rs` | 3c |
| 🟠 HIGH | Windows registry login item `"Warp"` keys | `app/src/login_item/windows.rs` | 3c |
| 🟠 HIGH | Config migration missing `.warp` origin | `crates/warp_core/src/paths.rs` | 6e |
| 🟠 HIGH | `needs_create_task` false negative | `app/src/ai/agent/api/impl.rs` | 6f |
| 🟡 MEDIUM | `TEXT_FLUSH_THRESHOLD` text discard | `app/src/ai/provider/stream.rs` | 6b |
| 🟡 MEDIUM | Empty synthesized tool results | `app/src/ai/provider/convert_request.rs` | 6c |
| 🟡 MEDIUM | 9-param `converse_stream()` | `app/src/ai/provider/client.rs` | 6a |
| 🟡 MEDIUM | Diagnostic logger privacy | `app/src/ai/provider/diagnostic.rs` | 6d |

---

## Decisions Log

| Decision | Choice | Rationale |
|----------|--------|-----------|
| URL scheme | `galaxy://` | Clean break from warp:// |
| TERM_PROGRAM | `GalaxyTerminal` | Matches product name |
| Bundle IDs | `dev.galaxy.*` | Clean break |
| GitHub org | `galaxydotdev` | Mirror original naming pattern |
| Data dirs | `~/.galaxy-ai/` | Already set in codebase |
| Env prefix | `GALAXY_*` | Consistent with product |
| Warp Drive | "Galaxy Drive" | Keep feature, rebrand |
| CLI agents | Remove ALL | Bedrock-only, no third-party |
| Server fallback | Remove entirely | Bedrock-only |
| Oz/cloud agent | Remove | Not applicable |
| Rules storage | `~/.galaxy-ai/rules/*.md` | Pure filesystem, no cloud |
| Project rules pattern | `["GALAXY.md", "AGENTS.md"]` | Rebrand |
| System prompt | Dynamic from context + meta-tool | Compact + on-demand docs |
| Tool docs | `get_tool_documentation` meta-tool | Reduces base prompt size |
| Crate names | Rename all to `galaxy_*` | Full rebrand |
| Directory renames | Yes, where name contains "warp" | Consistency |
