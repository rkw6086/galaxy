# Galaxy Project Architecture

Galaxy is a fork of Warp (the terminal emulator) rebranded under Samsung/Ryan Ward, using **Amazon Bedrock** as sole AI provider instead of Warp's server-side AI.

**Purpose:** Experiment to run a fully client-side AI terminal where the client calls Bedrock directly without going through the Warp server for AI. Rebranded from "Warp" to "Galaxy" at the crate/binary/config level.

## Key Facts

- **Language:** Rust (toolchain 1.92.0, edition 2021)
- **Workspace:** 56 member crates under `crates/` + main `app/`
- **Default binary:** `galaxy-oss` (OSS channel)
- **Channel binaries:** galaxy-oss, galaxy-local, galaxy-stable, galaxy-dev, galaxy-preview
- **Config dir:** `~/.galaxy-ai/` (channel-suffixed: `-dev`, `-oss`)
- **UI Framework:** GalaxyUI (custom, Entity-Component-Handle pattern, Flutter-inspired elements)
- **Database:** SQLite via Diesel ORM
- **Author:** Ryan Ward <ryan.ward@samsung.com>
- **GitLab:** gitlab.com:samnasbo/shared/galaxy

## Critical Architecture Decisions

1. **AI flows through Bedrock exclusively** — `app/src/ai/provider/client.rs` (`BedrockClient::converse_stream`)
2. **Models:** Default = Claude Opus 4.6 on Bedrock; also supports Claude Sonnet, Haiku, Nova Pro/Lite/Micro, DeepSeek R1
3. **Bedrock auth methods:** AWS Profile (default), Static Keys, SSO
4. **Cross-region inference:** Auto-prefixes model IDs (us., eu., jp., apac., au.) based on configured region
5. **Prompt caching:** CachePoints placed on system prompt and second-to-last message + tool config
6. **Tool set:** run_shell_command, read_files, apply_file_diffs, grep, file_glob, suggest_next_prompt
7. **Stream format:** Bedrock ConverseStream events → converted to `warp_multi_agent_api::ResponseEvent` (protobuf-based)
8. **OpenAI/LiteLLM support added** (as of v1.6.3) — new provider alongside Bedrock

## Directory Layout

- `app/` — Main binary and application logic
  - `app/src/ai/` — AI module (agent, bedrock, mcp, skills, ambient agents, etc.)
  - `app/src/ai/provider/` — Direct Bedrock client (client.rs, convert.rs, stream.rs, models.rs, convert_request.rs)
  - `app/src/settings/ai.rs` — AI settings (Bedrock config, permissions, autoexecution rules)
  - `app/src/terminal/` — Terminal emulation
  - `app/src/code/` — Code editor mode
  - `app/src/workspace/` — Workspace management
- `crates/galaxyui_core/` — Core UI framework (scene, elements, events, fonts, layout)
- `crates/galaxyui/` — Extended UI framework
- `crates/galaxyui_extras/` — UI extras (theming, preferences)
- `crates/galaxy_features/` — Feature flag system (`FeatureFlag` enum, DOGFOOD/PREVIEW/RELEASE tiers)
- `crates/galaxy_terminal/` — Terminal emulation core
- `crates/editor/` — Text editor
- `crates/ai/` — AI library (indexing, project context, codebase embedding)
- `crates/persistence/` — SQLite/Diesel persistence
- `crates/integration/` — Integration test framework

## Build & Test

```bash
# Dev run:
cargo run

# Bundle into .app:
cargo bundle --bin galaxy-oss --package galaxy

# Full distributable release DMG (compile + sign + notarize + package):
./build-release.sh

# Full distributable debug DMG:
./build-debug.sh

# Copy DMGs to ~/Downloads:
./copy-dmgs.sh

# Presubmit (fmt + clippy + nextest + doc tests):
./script/presubmit

# Tests only:
cargo nextest run --no-fail-fast --workspace --exclude command-signatures-v2

# Clean release build artifacts:
cargo clean --release

# Clean just bundle output:
rm -rf target/release/bundle/osx/Galaxy.app target/release/bundle/osx/Galaxy.dmg
```

## Settings Architecture

Settings defined via `define_settings_group!` macro. Bedrock settings in TOML at `~/.galaxy-ai/settings.toml`:
- `ai.bedrock.enabled` (default: true)
- `ai.bedrock.auth_method` (profile/static_keys/sso)
- `ai.bedrock.profile` (default: "default")
- `ai.bedrock.region` (auto-detect if empty)
- `ai.bedrock.cross_region_inference` (default: true)
- `ai.bedrock.models` (custom model list, overrides defaults)

## Feature Flags

Three-tier rollout: DOGFOOD_FLAGS → PREVIEW_FLAGS → RELEASE_FLAGS (or default in Cargo.toml features).
Runtime check: `FeatureFlag::YourFlag.is_enabled()`.
Defined in `crates/galaxy_features/src/lib.rs`.
