# Galaxy AI — Codebase Review

> **Generated:** 2026-05-07  
> **Branch state:** HEAD detached at `f4e2475` ("Rebasing, going about this another way")  
> **Scope:** ~55 files with uncommitted modifications reviewed for gaps, inconsistencies, and improvement opportunities.  
> **Purpose:** Reference document for agents continuing this work. No code changes were made during this review.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Branding Rebrand — Warp → Galaxy AI](#2-branding-rebrand--warp--galaxy-ai)
3. [Environment Variable Renames — WARP_ → GALAXY_](#3-environment-variable-renames--warp_--galaxy_)
4. [Config & Path Changes](#4-config--path-changes)
5. [Bedrock AI Feature](#5-bedrock-ai-feature)
6. [Samsung Themes & UI Font](#6-samsung-themes--ui-font)
7. [Autoupdate Changes](#7-autoupdate-changes)
8. [Unreviewed Files](#8-unreviewed-files)
9. [Crate Naming Inconsistency](#9-crate-naming-inconsistency)
10. [Prioritized Gap List](#10-prioritized-gap-list)
11. [Improvement Opportunities](#11-improvement-opportunities)

---

## 1. Executive Summary

This branch implements three major workstreams:

1. **Rebrand** — Surface-level rename from "Warp" → "Galaxy AI" for Samsung product identity.
2. **Bedrock AI** — New direct AWS Bedrock call path as an alternative/fallback to the Warp server AI path, including new slash command handlers, stream normalization, and a diagnostic logger.
3. **Samsung Themes & UI Font** — New `SamsungDark`/`SamsungLight` themes and a `ui_font_name` setting for brand presets.

The rebrand is **partially complete**. Several environment variable renames are missing on Windows code paths, and many `warp.dev` URLs remain untouched. There is a **critical runtime inconsistency** in integration test detection (`WARP_INTEGRATION` vs `GALAXY_INTEGRATION`) that will break integration test mode. The Bedrock feature is substantive and generally well-constructed, but has several reliability and API design concerns documented below.

---

## 2. Branding Rebrand — Warp → Galaxy AI

### ✅ Completed Correctly

| File | Change |
|---|---|
| `app/src/app_menus.rs` | Menu title `"Warp"` → `"Galaxy AI"` |
| `app/src/root_view.rs` | `WINDOW_TITLE` → `"Galaxy AI"`, welcome message updated |
| `app/src/ai_assistant/mod.rs` | `AI_ASSISTANT_FEATURE_NAME` → `"Galaxy AI"`, `ASK_AI_ASSISTANT_TEXT` updated |
| `app/src/ai_assistant/panel.rs` | `"Restart Warp AI"` → `"Restart Galaxy AI"` (2 places) |
| `app/src/ai/harness_display.rs` | `"Warp Agent"` → `"Galaxy Agent"` |
| `app/src/settings_view/ai_page.rs` | Agent labels updated throughout |
| `app/src/settings_view/about_page.rs` | Logo SVG → `bedrock.svg`, copyright → Samsung Electronics Co., Ltd. |
| `app/src/workspace/mod.rs` | `"Warp Drive"` → `"Galaxy Drive"` throughout |
| `app/src/workspace/view.rs` | Update prompt, Drive references updated |
| `app/src/search/data_source.rs` | Drive filter label updated |
| `app/src/uri/browser_url_handler.rs` | `DEFAULT_TITLE` → `"Galaxy AI"` |
| `app/src/bin/local.rs` | CFBundle* keys → Samsung/GalaxyAI |
| `app/src/bin/oss.rs` | CFBundle* keys → Samsung/GalaxyAI, AppId updated |
| `crates/warp_core/src/channel/mod.rs` | CLI command names `oz` → `galaxy-ai` |

### ❌ Gaps — Remaining "Warp" References

#### UI / Window Titles
- **`app/src/root_view.rs` lines 797, 840, 892, 1264, 1449** — Still use `title: Some("Warp".to_owned())`. These are dialog/modal window titles. Need: `"Galaxy AI"`.
- **`app/src/workflows/local_workflows.rs:227`** — `author: Some("Warp".into())` for local workflows author attribution.

#### Enum Values (May Be Intentional — Verify)
- **`app/src/terminal/view/ambient_agent/host_selector.rs:48`** — `Host::Warp => "Warp"` — Display label for an enum variant. If the `Host::Warp` variant is kept for protocol compatibility, the display label may still need updating.
- **`app/src/ai/mcp/mod.rs:76`** — `MCPProvider::Warp => "Warp"` — MCP provider display name. If Warp MCP is still a valid provider, this may be intentional.
- **`app/src/settings_view/appearance_page.rs:3459`** — `RadioButtonItem::text("Warp")` — Context unclear; needs investigation.

#### External Editor / OAuth
- **`app/src/settings_view/features/external_editor.rs:157,179`** — `"Warp"` as an editor choice. Refers to using Warp itself as the external editor; likely intentional but should be verified.
- **`app/src/ai/mcp/templatable_manager/oauth.rs:306,324`** — `"Warp"` as OAuth client name. If this is sent to an external OAuth provider, it must be updated to `"Galaxy AI"`.

#### Windows Startup Registry
- **`app/src/login_item/windows.rs`** — Multiple `"Warp"` registry key names for Windows startup login item. These control Windows startup behavior and must be updated to avoid conflicts with the actual Warp app if both are installed.

#### URLs — All Still Point to `warp.dev`
These are all unmodified and need new Samsung/Galaxy AI target URLs:

| File | URL(s) |
|---|---|
| `app/src/util/links.rs` | All doc, privacy, slack URLs — `warp.dev` |
| `app/src/resource_center/sections.rs` | `docs.warp.dev` |
| `app/src/resource_center/section_views/changelog_section.rs` | `docs.warp.dev/changelog` |
| `app/src/auth/auth_view_shared_helpers.rs:29` | `https://warp.dev/privacy` |
| `app/src/launch_configs/save_modal.rs:533` | `docs.warp.dev` |
| `app/src/wasm_nux_dialog.rs` | `app.warp.dev`, `www.warp.dev` |
| `app/src/integration_testing/agent_mode/mod.rs` | `staging.warp.dev` |
| `app/src/cloud_object/mod.rs` | `warp.dev/drive/` in comment, `joan@warp.dev` example email |

#### Stale Comment
- **`app/src/workspace/view.rs:546`** — Comment still reads: *"Use the word 'Warp'..."* — should be updated or removed.

#### Referral Theme Display Names
- **`app/src/themes/theme.rs`** — `ThemeKind::SentReferralReward` displays as `"Warp Referral"` and `ThemeKind::ReceivedReferralReward` displays as `"Referred to Warp"`. Not updated.

---

## 3. Environment Variable Renames — WARP_ → GALAXY_

### ✅ Completed Correctly

| Old Name | New Name | File |
|---|---|---|
| `WARP_SERVER_ROOT_URL` | `GALAXY_SERVER_ROOT_URL` | `crates/warp_cli/src/lib.rs` |
| `WARP_WS_SERVER_URL` | `GALAXY_WS_SERVER_URL` | `crates/warp_cli/src/lib.rs` |
| `WARP_SESSION_SHARING_SERVER_URL` | `GALAXY_SESSION_SHARING_SERVER_URL` | `crates/warp_cli/src/lib.rs` |
| `WARP_API_KEY` | `GALAXY_API_KEY` | `crates/warp_cli/src/lib.rs` |
| `WARP_OUTPUT_FORMAT` | `GALAXY_OUTPUT_FORMAT` | `crates/warp_cli/src/lib.rs` |
| `WARP_SHELL_PATH` | `GALAXY_SHELL_PATH` | `crates/warp_util/src/path.rs` |
| `WARP_INTEGRATION` | `GALAXY_INTEGRATION` | `crates/warp_logging/src/native.rs` |
| `WARP_EXTRA_HTTP_HEADERS` | `GALAXY_EXTRA_HTTP_HEADERS` | `crates/http_client/src/lib.rs` |
| `WARP_CLIENT_VERSION` | `GALAXY_CLIENT_VERSION` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_USE_SSH_WRAPPER` | `GALAXY_USE_SSH_WRAPPER` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_IS_LOCAL_SHELL_SESSION` | `GALAXY_IS_LOCAL_SHELL_SESSION` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_CLI_AGENT_PROTOCOL_VERSION` | `GALAXY_CLI_AGENT_PROTOCOL_VERSION` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_SHELL_DEBUG_MODE` | `GALAXY_SHELL_DEBUG_MODE` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_HONOR_PS1` | `GALAXY_HONOR_PS1` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_PATH_APPEND` | `GALAXY_PATH_APPEND` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_INITIAL_HISTFILESIZE` | `GALAXY_INITIAL_HISTFILESIZE` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_INITIAL_WORKING_DIR` | `GALAXY_INITIAL_WORKING_DIR` | `app/src/terminal/local_tty/unix.rs` |
| `WARP_CLOUD_MODE_DEFAULT_HOST` | `GALAXY_CLOUD_MODE_DEFAULT_HOST` | `app/src/terminal/view/ambient_agent/model.rs` |

### ❌ CRITICAL: `WARP_INTEGRATION` Split

**`app/src/lib.rs:658`** still reads `WARP_INTEGRATION`, but `crates/warp_logging/src/native.rs` was updated to `GALAXY_INTEGRATION`. These two reads must match or integration test detection will silently fail — the app won't recognize it's running in integration test mode even when `GALAXY_INTEGRATION=1` is set.

**Fix:** Update `app/src/lib.rs:658` to read `GALAXY_INTEGRATION`.

### ❌ Missing Renames

#### Windows TTY Path (Complete Gap)
`app/src/terminal/local_tty/windows/environment.rs` — **ALL** of these env vars are still using `WARP_*` names (lines 20–27, 105–116). The Unix path was updated but Windows was skipped entirely:
- `WARP_HONOR_PS1`
- `WARP_INITIAL_WORKING_DIR`
- `WARP_USE_SSH_WRAPPER`
- `WARP_SHELL_DEBUG_MODE`
- `WARP_IS_LOCAL_SHELL_SESSION`
- `WARP_PATH_APPEND`
- `WARP_CLIENT_VERSION`
- `WARP_CLI_AGENT_PROTOCOL_VERSION`

#### Other Missing Renames

| File | Env Var | Priority |
|---|---|---|
| `app/src/lib.rs:643` | `WARP_CLI_MODE` | High |
| `app/src/lib.rs:658` | `WARP_INTEGRATION` | **CRITICAL** |
| `app/src/lib.rs:866` | `WARP_ENABLE_WAYLAND` | Medium |
| `app/src/plugin/mod.rs:17` | `WARP_PLUGIN_HOST_ADDRESS` | High |
| `app/src/terminal/remote_tty/event_loop.rs:179` | `WARP_HONOR_PS1` | High |
| `app/src/autoupdate/channel_versions.rs:20` | `WARP_CHANNEL_VERSIONS_PATH` | Medium |
| `app/src/autoupdate/mac.rs:150-151` | `WARP_CHANNEL_VERSIONS_PATH` | Medium |
| `app/src/autoupdate/linux.rs:153-154` | `WARP_CHANNEL_VERSIONS_PATH` | Medium |
| `app/src/auth/auth_state.rs:107` | `WARP_USER_SECRET` (build-time) | Medium |
| `app/src/settings/debug.rs:5` | Comment only — `WARP_SHELL_DEBUG_MODE` | Low |

---

## 4. Config & Path Changes

### Config Directory Migration Chain

**`crates/warp_core/src/paths.rs`** was updated:
- `WARP_CONFIG_DIR` → `.galaxy-ai`
- `LEGACY_WARP_CONFIG_DIR` → `.warp-core`

**Problem:** The migration chain is now: `.warp-core` → `.galaxy-ai`

However, the original legacy path `.warp` needs to be in the migration chain. Users with an original Warp installation who never ran `.warp-core` will have their config in `.warp`. The current code only migrates from `.warp-core`, so original Warp users will lose their settings on first launch of Galaxy AI.

**Recommendation:** Add a third legacy entry for `.warp` (the original Warp config dir) so the full chain is preserved: `.warp` (original Warp) → `.warp-core` (intermediate) → `.galaxy-ai` (current). Verify if this was already handled via a separate legacy migration step before assuming it's missing.

### Server Config Cleared

**`crates/warp_core/src/channel/config.rs`** — All production server URLs have been cleared to empty strings:
- `server_root_url = ""`
- `rtc_server_url = ""`
- `session_sharing_server_url = ""`
- `firebase_auth_api_key` = `None`
- `oz_root_url = ""`

This appears intentional — the Samsung/Galaxy AI deployment won't use the Warp backend. The related fallback behavior in `channel/state.rs` and `server/server_api.rs` (silently falling back to `http://localhost` when URL is empty) handles this gracefully for Bedrock-only operation.

**Concern:** If any codepath still requires server connectivity (e.g., sync, Drive, auth), it will silently talk to localhost and fail without a clear error. Consider surfacing a more descriptive error when server config is intentionally empty and a server-dependent feature is attempted.

---

## 5. Bedrock AI Feature

This is the primary new feature. A complete AWS Bedrock direct-call path runs alongside (and as an alternative to) the Warp server AI path.

### New Files

| File | Purpose |
|---|---|
| `app/src/ai/provider/diagnostic.rs` | `BedrockDiagnosticLogger` — file-based diagnostic logging, activated via `GALAXY_BEDROCK_DIAGNOSTICS=1`, 10MB rotation |
| `app/src/ai/provider/e2e_tests.rs` | Comprehensive E2E integration tests (requires `BEDROCK_INTEGRATION_TEST` env var) |
| `app/src/ai/provider/integration_tests.rs` | Unit-level stream output collection tests |

### Modified Files

| File | Key Change |
|---|---|
| `app/src/ai/provider/client.rs` | Added `needs_create_task: bool` and `diagnostic_logger` params to `converse_stream()` |
| `app/src/ai/provider/convert.rs` | Added `Clone`, `Debug`, `PartialEq` derives to core types |
| `app/src/ai/provider/convert_request.rs` | New input type handlers: `InitProjectRules`, `CreateEnvironment`, `CreateNewProject`, `CloneRepository`, `AutoCodeDiffQuery`, `ResumeConversation`, `QueryWithCannedResponse`, `CodeReview` |
| `app/src/ai/provider/stream.rs` | Buffered text flush threshold, `needs_create_task` / `CreateTask` event, reasoning content discarded |
| `app/src/ai/agent/api/impl.rs` | `needs_create_task` detection, diagnostic logger wiring, verbose debug logging |
| `app/src/ai/llms.rs` | Bedrock models added to `coding` and `cli_agent` feature slots (previously only `agent_mode`) |

### Concerns & Gaps

#### 🔴 Critical

**`WARP_INTEGRATION` / `GALAXY_INTEGRATION` split** (also listed in §3) — The diagnostic logger in `diagnostic.rs` and the integration test detection in `app/src/lib.rs` use different env var names. This is already flagged under §3 but bears repeating here as it directly affects Bedrock E2E test execution.

#### 🟠 High

**`needs_create_task` detection logic** (`app/src/ai/agent/api/impl.rs`)  
Detected via `task_context.tasks.is_empty()`. If the tasks list is populated with a placeholder or shell task entry before the first real task is submitted, this flag will be `false` and `CreateTask` will never be emitted, silently breaking the task creation flow for Bedrock on the first turn.

**E2E test tool_use_id mismatch** (`app/src/ai/provider/e2e_tests.rs`)
In `test_agent_multi_turn_tool_use_produces_output`, the test synthesizes tool result IDs as `tool_{total_turns}`, but the actual tool call ID emitted from the stream is a different value. This means the tool_call_id round-trip assertion will fail or be skipped silently. The test may pass vacuously.

**`ensure_tool_results_paired()` synthesized results** (`app/src/ai/provider/convert_request.rs`)
This function inserts placeholder tool results for orphaned tool calls. The synthesized content is empty/default, which could confuse the model on subsequent turns. The synthesized result should include a meaningful message like `"Tool result not available"` rather than empty content.

#### 🟡 Medium

**`TEXT_FLUSH_THRESHOLD = 20`** (`app/src/ai/provider/stream.rs`)
Text fragments shorter than 20 characters before a tool call boundary are silently discarded. This can drop model preamble text such as `"Let me look at that..."` or `"OK."`. Consider buffering until a natural boundary (tool call or stop event) rather than a character threshold, or surface buffered content even if short.

**`converse_stream()` wide parameter surface** (`app/src/ai/provider/client.rs`)
The function now takes 9 parameters including `needs_create_task` in the middle of the list. This is fragile and hard to read at call sites. Recommend grouping into a `BedrockConversationConfig` struct.

**Diagnostic logger privacy concern** (`app/src/ai/provider/diagnostic.rs`)
`log_protobuf_input` uses Rust's `{:?}` debug format rather than structured JSON. This is inconsistent with the rest of the logger (which emits JSON) and may dump sensitive content (full conversation history, file contents) to disk in an unstructured format. Consider either JSON serialization or explicit redaction.

#### 🟢 Low

**Reasoning content silently discarded** (`app/src/ai/provider/stream.rs`)
Reasoning/thinking content from models that emit it (e.g., Claude 3.7 extended thinking) is currently discarded. This is an intentional product decision but should be documented in a comment so future contributors understand why.

**`diagnostic.rs` log path** — Uses `warp_logging::log_directory()`. The log file will land in the `warp_logging`-configured directory. Verify this resolves to the correct Galaxy AI log directory on all platforms.

---

## 6. Samsung Themes & UI Font

### New Additions

- **`app/src/themes/default_themes.rs`** — `samsung_dark()` and `samsung_light()` theme functions with full ANSI color palettes.
- **`app/src/themes/theme.rs`** — `ThemeKind::SamsungDark` and `ThemeKind::SamsungLight` enum variants.
- **`app/src/settings/font.rs`** — `DEFAULT_UI_FONT_NAME: &str = ""` constant; new `ui_font_name` setting persisted at `appearance.text.ui_font_name`.
- **`app/src/settings_view/appearance_page.rs`** — `UIFontWidget`, `BrandPresetWidget`, `SetUIFontFamily(String)` action, `ApplySamsungBrandPreset` action.
- **`app/src/appearance.rs`** — `FontSettingsChangedEvent::UIFontName` handler; `set_ui_font_family()` called at init and on change.

### Concerns

**`DEFAULT_UI_FONT_NAME = ""`** — Empty string used as sentinel for "system default font". This works but is a code smell. Recommend `Option<String>` or a named constant like `"system"` that is clearly documented as the system default sentinel.

**`apply_samsung_brand_preset()` not fully reviewed** — The function is referenced in the appearance page but its full implementation wasn't visible in the reviewed diffs. Ensure it sets both font and theme atomically (not partially applied).

**Referral theme display names not updated** — `ThemeKind::SentReferralReward` → `"Warp Referral"` and `ThemeKind::ReceivedReferralReward` → `"Referred to Warp"`. These should be updated to Galaxy AI equivalents.

---

## 7. Autoupdate Changes

`app/src/autoupdate/mac.rs` and `app/src/autoupdate/windows.rs` are modified per `git status` but were not fully reviewed. Key finding:

- **`WARP_CHANNEL_VERSIONS_PATH`** env var is NOT renamed in any of the autoupdate files (`channel_versions.rs`, `mac.rs`, `linux.rs`). This means the auto-update channel selection won't respect the renamed env var.

---

## 8. Unreviewed Files

The following files appeared in `git status` but were not reviewed during this pass:

| File | Likely Purpose |
|---|---|
| `crates/warpui/src/rendering/wgpu/mod.rs` | Unknown — possibly rendering/branding change |
| `crates/warpui/src/windowing/winit/delegate.rs` | Unknown — possibly window title or branding |
| `crates/integration/src/test.rs` | Integration test framework — possibly `GALAXY_INTEGRATION` wiring |
| `script/macos/bundle` | macOS bundle script — likely bundle ID / app name |
| `app/DockTilePlugin/Info.plist` | macOS Dock tile plugin — likely bundle ID / display name |
| `app/build.rs` | Cargo build script — likely binary name, bundle ID, version |
| `app/assets/bundled/svg/bedrock.svg` | New Bedrock/Samsung logo SVG asset |
| `app/assets/Galaxy_AI_Logo.jpg` | New logo asset |
| `app/assets/Galaxy_AI_Logo.png` | New logo asset |

**These files must be reviewed** before this branch is considered complete. The `build.rs` in particular often controls bundle identifiers and binary names critical to the rebrand.

---

## 9. Crate Naming Inconsistency

All internal crates retain `warp_*` naming:
- `warp_core`
- `warpui`
- `warp_logging`
- `warp_cli`
- `warp_util`
- `warp_multi_agent_api`

**Decision needed:** Is keeping `warp_*` crate names intentional (for internal stability / CI continuity) or an oversight? If Samsung intends to publish or open-source these crates under Galaxy AI branding, the crate names should be renamed. If they are purely internal, keeping `warp_*` is acceptable but should be documented explicitly in `WARP.md` or `README.md` to avoid future confusion.

---

## 10. Prioritized Gap List

### 🔴 Critical — Fix Before Ship

| # | Issue | File(s) |
|---|---|---|
| C1 | `WARP_INTEGRATION` / `GALAXY_INTEGRATION` split — breaks integration test detection at runtime | `app/src/lib.rs:658` |
| C2 | Windows TTY env vars completely skipped — Windows shell integration broken | `app/src/terminal/local_tty/windows/environment.rs` |
| C3 | E2E test tool_use_id mismatch — multi-turn tool use test may be vacuously passing | `app/src/ai/provider/e2e_tests.rs` |

### 🟠 High — Fix Before Beta

| # | Issue | File(s) |
|---|---|---|
| H1 | `needs_create_task` false negative if task list pre-populated | `app/src/ai/agent/api/impl.rs` |
| H2 | `WARP_CLI_MODE`, `WARP_ENABLE_WAYLAND`, `WARP_PLUGIN_HOST_ADDRESS` not renamed | `app/src/lib.rs`, `app/src/plugin/mod.rs` |
| H3 | Remote TTY `WARP_HONOR_PS1` not renamed | `app/src/terminal/remote_tty/event_loop.rs:179` |
| H4 | OAuth client name still `"Warp"` — sent to external providers | `app/src/ai/mcp/templatable_manager/oauth.rs` |
| H5 | Windows registry login item still uses `"Warp"` key names | `app/src/login_item/windows.rs` |
| H6 | Unreviewed files — `build.rs`, `DockTilePlugin/Info.plist`, `script/macos/bundle` | See §8 |
| H7 | Config migration chain may leave original `.warp` users without migrated settings | `crates/warp_core/src/paths.rs` |

### 🟡 Medium — Fix Before GA

| # | Issue | File(s) |
|---|---|---|
| M1 | All `warp.dev` URLs untouched — wrong privacy policy, docs, and external links | `app/src/util/links.rs` + 7 other files |
| M2 | `converse_stream()` 9-param signature — should use config struct | `app/src/ai/provider/client.rs` |
| M3 | `TEXT_FLUSH_THRESHOLD = 20` may silently discard model preamble text | `app/src/ai/provider/stream.rs` |
| M4 | `ensure_tool_results_paired()` synthesizes empty tool results — may confuse model | `app/src/ai/provider/convert_request.rs` |
| M5 | `WARP_CHANNEL_VERSIONS_PATH` not renamed in autoupdate | `app/src/autoupdate/` |
| M6 | Diagnostic logger `log_protobuf_input` uses debug format, not JSON | `app/src/ai/provider/diagnostic.rs` |
| M7 | `WARP_USER_SECRET` build-time env not renamed | `app/src/auth/auth_state.rs:107` |
| M8 | `DEFAULT_UI_FONT_NAME = ""` sentinel is a code smell | `app/src/settings/font.rs` |
| M9 | Referral theme display names still say `"Warp Referral"` | `app/src/themes/theme.rs` |
| M10 | Root view dialog titles still `"Warp"` (5 locations) | `app/src/root_view.rs` |
| M11 | Local workflow author still `"Warp"` | `app/src/workflows/local_workflows.rs:227` |

### 🟢 Low — Polish / Documentation

| # | Issue | File(s) |
|---|---|---|
| L1 | Stale comment: *"Use the word 'Warp'..."* | `app/src/workspace/view.rs:546` |
| L2 | Reasoning content discard should be documented with a comment | `app/src/ai/provider/stream.rs` |
| L3 | `warp_*` crate naming — document intent (internal stable vs. rebrand needed) | All `Cargo.toml` files |
| L4 | `apply_samsung_brand_preset()` full implementation not reviewed | `app/src/settings_view/appearance_page.rs` |
| L5 | `WARP.md` build commands still reference `cargo bundle --bin warp` — should be updated | `WARP.md` |
| L6 | `RadioButtonItem::text("Warp")` in appearance page — context unclear | `app/src/settings_view/appearance_page.rs:3459` |
| L7 | Comment-only `WARP_SHELL_DEBUG_MODE` ref | `app/src/settings/debug.rs:5` |

---

## 11. Improvement Opportunities

### Bedrock API Design

**`BedrockConversationConfig` struct** — The `converse_stream()` function now takes 9 parameters. Create a config struct:

```rust
pub struct BedrockConversationConfig {
    pub messages: Vec<ConversationMessage>,
    pub tools: Vec<ToolDefinition>,
    pub system_prompt: Option<String>,
    pub model_id: String,
    pub needs_create_task: bool,
    pub diagnostic_logger: Option<Arc<BedrockDiagnosticLogger>>,
    // ...
}
```

**Stream text buffering** — Instead of the character threshold approach (`TEXT_FLUSH_THRESHOLD = 20`), consider accumulating text until a semantic boundary (tool call start, stop event, or a configurable flush interval). This avoids the silent text discard problem.

**Synthesized tool results** — When `ensure_tool_results_paired()` synthesizes a tool result for an orphaned call, inject a meaningful error message in the content rather than empty content:
```rust
content: "Error: tool result was not provided for this tool call".to_string()
```

### Diagnostic Logger

**Structured logging** — Replace `{:?}` debug format in `log_protobuf_input` with proper JSON serialization. Consider using `serde_json` for consistent structured output across all log methods.

**Redaction** — The diagnostic logger logs the full conversation including user messages and file contents. Add a redaction layer or at minimum document that `GALAXY_BEDROCK_DIAGNOSTICS=1` is a developer-only flag not intended for production use.

### Settings & Themes

**`Option<String>` for UI font** — Replace `DEFAULT_UI_FONT_NAME = ""` with `Option<String>` where `None` means system default. This is more idiomatic Rust and avoids the empty string sentinel.

**Samsung brand preset atomicity** — Ensure `apply_samsung_brand_preset()` applies theme and font in a single transaction to avoid intermediate UI state where only one of the two is applied.

### Error Handling for Empty Server Config

When `server_root_url` is intentionally empty (the Galaxy AI deployment case), and a server-dependent feature (Drive, sync, auth) is attempted, the app currently falls back silently to `http://localhost`. Add a specific error variant or user-facing message for this case:
> *"This feature requires a Warp server connection which is not configured in this build."*

### Documentation

- Document the `warp_*` crate naming decision in `WARP.md` or `README.md`.
- Document `GALAXY_BEDROCK_DIAGNOSTICS=1` in `WARP.md` as a development diagnostic flag.
- Add a comment in `stream.rs` explaining why reasoning content is discarded.
- Update `WARP.md` build commands to reference the Galaxy AI binary name (if renamed).
