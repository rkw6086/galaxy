---
name: pull_warp_feature
description: Pull a feature from the upstream Warp codebase into the Galaxy fork. Use this skill whenever the user wants to port, backport, or bring over a feature from Warp into Galaxy. This skill handles cloning the Warp source, analyzing the feature for Warp-specific dependencies, planning safe replacements, and implementing the port.
---

# Pull Warp Feature into Galaxy

This skill orchestrates porting a feature from the upstream Warp terminal (github.com/warpdotdev/warp) into the Galaxy fork. It is intentionally cautious — Galaxy MUST NOT contain any Warp-proprietary service dependencies, Warp API calls, or Warp cloud infrastructure ties.

---

## Phase 1: Acquire Warp Source

Before anything else, ensure a clean copy of the Warp source exists locally for reference.

### Steps

1. Check if `.galaxy/warp-upstream/` already exists in the project root:

```bash
ls -d .galaxy/warp-upstream/.git 2>/dev/null
```

2. **If it does NOT exist**, clone the Warp repo:

```bash
mkdir -p .galaxy
git clone --depth 1 https://github.com/warpdotdev/warp.git .galaxy/warp-upstream
```

   Use `--depth 1` to keep it lightweight. If deeper history is needed for a specific feature, deepen later with `git fetch --unshallow`.

3. **If it DOES exist**, pull latest:

```bash
git -C .galaxy/warp-upstream pull --ff-only
```

4. **Ensure `.galaxy/` is gitignored.** Check `.gitignore` for a `.galaxy/` entry. If missing, append it:

```bash
echo ".galaxy/" >> .gitignore
```

   IMPORTANT: Verify the line doesn't already exist before appending. Use `grep -q "^\.galaxy/" .gitignore` first.

5. **NEVER commit the `.galaxy/` directory or its contents.** It is a local-only reference checkout.

---

## Phase 2: Identify the Feature

Once the Warp source is available, ask the user:

> **What feature are you looking at bringing over?**

Wait for the user's response. Do NOT proceed until you have a clear answer.

### Interpreting the response

- The user may describe the feature by name (e.g. "tab drag and drop", "AI suggestions", "voice input").
- The user may reference a specific file, module, or PR number.
- The user may describe behavior they saw in Warp and want in Galaxy.

Accept any of these as valid starting points.

---

## Phase 3: Deep Feature Analysis

Once you know the target feature, perform a thorough investigation. You need to build a complete dependency map before making any promises.

### 3a. Locate the feature in Warp source

Search the Warp source at `.galaxy/warp-upstream/` for the feature:

- Use `grep`, `codebase_semantic_search`, and `file_glob` against `.galaxy/warp-upstream/`
- Read the relevant source files in full
- Identify the entry points, data models, UI components, and backend calls
- Map out every file the feature touches

### 3b. Locate the corresponding code in Galaxy

Search the Galaxy codebase to understand:

- Does Galaxy already have a partial implementation of this feature?
- What Galaxy modules correspond to the Warp modules this feature touches?
- Are there naming differences? (e.g. `warp_core` → `galaxy_core`, `WarpUI` → `GalaxyUI`)

### 3c. Build the dependency inventory

For EVERY dependency the feature has, categorize it into one of these buckets:

**✅ SAFE — No changes needed:**
- Pure UI logic (elements, views, event handlers)
- Local computation (parsers, formatters, algorithms)
- Terminal emulation logic
- Platform-native APIs (macOS, Windows, Linux)
- Local filesystem operations
- Open-source crate dependencies already in Galaxy's `Cargo.toml`

**⚠️ REQUIRES REPLACEMENT — Can be ported with modification:**
- Warp AI / LLM calls → Must be replaced with Amazon Bedrock via `BedrockClient`
- Warp API HTTP endpoints → Must be removed or replaced with local storage
- Warp Drive cloud sync → Must be replaced with local `.galaxy/` storage or removed
- Warp authentication/identity → Must be removed or replaced with Galaxy auth
- Warp telemetry/analytics → Must be removed entirely
- Warp-specific feature flags → Must be converted to Galaxy `FeatureFlag` enum variants
- GraphQL queries to Warp server → Must be removed or rerouted

**🚫 BLOCKED — Cannot be ported:**
- Features that fundamentally require Warp's proprietary backend to function
- Features that require Warp's authentication tokens with no Bedrock/local alternative
- Features requiring real-time sync with Warp's cloud that cannot be made local
- Warp billing/subscription gating logic

### 3d. Ask follow-up questions

Based on your analysis, ask the user clarifying questions. These might include:

- "This feature uses Warp's X service — do you want me to replace it with Y, or skip that part?"
- "There are two sub-features here: A and B. A is clean to port, B requires major rework. Want both?"
- "The Warp version uses cloud storage for Z. Should I store this in `~/.galaxy-ai/` or `.galaxy/`?"
- "This depends on crate X which isn't in Galaxy yet. OK to add it?"

Do NOT proceed until the user has answered your follow-up questions and you are confident you understand the scope.

---

## Phase 4: Compatibility Assessment

This is the most critical phase. You must produce a detailed assessment. Go through EVERY dependency from Phase 3c and make a concrete determination.

### 4a. AI/LLM Dependencies

If the feature uses Warp AI in any way:

1. **Identify every AI call site** — What prompts are sent? What models are used? What's the expected response format?
2. **Determine if Bedrock can handle it** — Galaxy uses `BedrockClient::converse_stream` (see `app/src/ai/bedrock/client.rs`). The feature's AI usage MUST be expressible as Bedrock Converse API calls.
3. **Check for Warp-specific prompt engineering** — System prompts in Warp may reference Warp-specific context. These must be rewritten for Galaxy.
4. **Check for Warp-specific tool use** — If the feature defines custom AI tools, verify they don't call Warp APIs internally.
5. **Verdict**: Can it be "Bedrockified"? If NO → the feature CANNOT be ported. Stop and inform the user.

Key files for Bedrock integration reference:
- `app/src/ai/bedrock/client.rs` — Client implementation
- `app/src/ai/bedrock/convert_request.rs` — Request construction and system prompts
- `app/src/ai/bedrock/convert.rs` — Wire format conversion
- `app/src/ai/bedrock/tool_docs.rs` — Tool documentation
- `app/src/ai/bedrock/stream.rs` — Response stream processing

### 4b. Cloud Storage / Warp API Dependencies

If the feature calls Warp API endpoints or uses Warp Drive:

1. **List every HTTP/GraphQL call** the feature makes to Warp servers
2. **For each call, determine:**
   - Can it be removed entirely without breaking the feature?
   - Can it be replaced with local file storage in `~/.galaxy-ai/` or project-local `.galaxy/`?
   - Can it be replaced with a different API (e.g. direct AWS call)?
3. **If it requires Warp server and there's no local alternative** → that specific sub-feature CANNOT be ported
4. **Local storage patterns to use:**
   - User-scoped data: `~/.galaxy-ai/<feature>/`
   - Project-scoped data: `.galaxy/<feature>/` (ensure gitignored)
   - SQLite via Diesel: `app/src/persistence/` (for data that fits Galaxy's existing DB)

### 4c. Authentication Dependencies

If the feature requires Warp authentication:

1. Does Galaxy have its own auth that can substitute?
2. If the feature gates on "is the user logged in" — can this gate be removed?
3. If the feature requires user identity — can it use a local config value instead?

### 4d. Telemetry / Analytics

Any Warp telemetry, analytics, or tracking code MUST be stripped entirely. Do not replace it — remove it.

### 4e. Naming and Branding

All references to "Warp" in user-facing strings, comments, and identifiers must be changed to "Galaxy":
- `warp` → `galaxy`
- `Warp` → `Galaxy`
- `WARP` → `GALAXY`
- `warp_core` → `galaxy_core`
- `WarpUI` → `GalaxyUI`
- etc.

This includes:
- Rust module names and paths
- Struct/enum/function names
- String literals shown to users
- Comments and documentation
- Environment variable prefixes (`WARP_` → `GALAXY_`)
- Config file paths (`~/.warp/` → `~/.galaxy-ai/`)

---

## Phase 5: Present Findings and Plan

Present the user with a structured summary using the `create_plan` tool. The plan MUST include:

1. **Feature summary**: What the feature does in Warp, in 2-3 sentences
2. **Files to port**: Exact list of files from `.galaxy/warp-upstream/` and where they map in Galaxy
3. **Dependency assessment table** (as a list, not a markdown table):
   - For each dependency: what it is, its category (SAFE / REQUIRES REPLACEMENT / BLOCKED), and the replacement strategy
4. **AI assessment**: Does it need AI? Can it be Bedrockified? What changes are needed?
5. **Cloud/API assessment**: Does it call Warp servers? What's the local replacement?
6. **Storage assessment**: Where will data live? What gets gitignored?
7. **Risk areas**: What might break? What needs extra testing?
8. **Estimated scope**: How many files are touched? Is this a 1-hour or 1-week port?

**CRITICAL**: Do NOT proceed to implementation until the user explicitly approves the plan.

If any part of the feature is BLOCKED, clearly state:
> "The following parts of this feature CANNOT be ported because they fundamentally require Warp's proprietary infrastructure: [list]. I recommend porting only the parts that are SAFE or REQUIRES REPLACEMENT."

---

## Phase 6: Implementation

Only after the user approves the plan, begin implementation.

### Implementation Rules

These rules are NON-NEGOTIABLE:

1. **NEVER copy Warp API URLs, tokens, or endpoint paths into Galaxy code.**
2. **NEVER leave Warp telemetry/analytics calls in the code, even commented out.**
3. **NEVER leave `warp` branding in user-facing strings.** Internal code comments referencing the upstream origin (e.g. "Ported from Warp's X module") are acceptable.
4. **ALL AI calls MUST go through `BedrockClient`** — no direct OpenAI, Anthropic, or other provider calls.
5. **ALL cloud storage MUST be local** — `~/.galaxy-ai/` for user data, `.galaxy/` for project data, or Diesel/SQLite for persistent structured data.
6. **ALL feature flags MUST use Galaxy's `FeatureFlag` enum** in `galaxy_features/src/lib.rs`.
7. **ALL environment variables MUST use the `GALAXY_` prefix.**
8. **ALL config paths MUST use `~/.galaxy-ai/`** not `~/.warp/`.

### Implementation Procedure

1. **Create a TODO list** with discrete steps for the port.

2. **Port files one module at a time**, in dependency order (deepest dependencies first, UI last):
   - Copy the file from `.galaxy/warp-upstream/` to the correct Galaxy location
   - Rename all Warp references to Galaxy equivalents
   - Replace all REQUIRES REPLACEMENT dependencies with Galaxy alternatives
   - Remove all BLOCKED dependencies and any code paths that depend on them
   - Ensure all `use` / `mod` statements point to Galaxy crate names

3. **After each module**, verify it compiles:
   ```bash
   cargo check -p <crate_name>
   ```

4. **After all modules are ported**, run full workspace checks:
   ```bash
   cargo fmt
   cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
   ```

5. **If the feature has tests in Warp**, port the tests too:
   - Place tests in `${filename}_tests.rs` per Galaxy convention
   - Update test assertions to reflect Galaxy behavior (no Warp API mocking)
   - Run tests:
     ```bash
     cargo nextest run --no-fail-fast -p <crate_name>
     ```

6. **Update documentation**:
   - Update `GALAXY.md` if the feature changes architecture or adds commands
   - Update `WARP.md` if it exists and needs corresponding changes
   - Update `CLAUDE.md` if it exists in the project

7. **Final validation**:
   ```bash
   cargo build
   cargo nextest run --no-fail-fast --workspace --exclude command-signatures-v2
   ```

### Post-Implementation Audit

After implementation, perform a final audit. Search the entire diff for:

```bash
# In the changed files, search for any Warp leaks
grep -rn "warp\.dev" <changed_files>
grep -rn "api\.warp" <changed_files>
grep -rn "warpdotdev" <changed_files>
grep -rn "warp-server" <changed_files>
grep -rn "WARP_API" <changed_files>
grep -rn "warp_api" <changed_files>
```

Any hits (other than comments documenting the port origin) are bugs that must be fixed before completion.

Also verify no `.galaxy/` files were staged:

```bash
git status --porcelain | grep "^A.*\.galaxy/"
```

---

## Reference: Galaxy ↔ Warp Name Mapping

Common renames when porting:

- `warp` → `galaxy` (crate names, binary names)
- `warp_core` → `galaxy_core`
- `warpui` → `galaxyui`
- `warpui_core` → `galaxyui_core`
- `warpui_extras` → `galaxyui_extras`
- `warp_terminal` → `galaxy_terminal`
- `warp_util` → `galaxy_util`
- `warp_features` → `galaxy_features`
- `warp_completer` → `galaxy_completer`
- `warp_graphql_schema` → `galaxy_graphql_schema`
- `warp_cli` → `galaxy_cli`
- `WARP_` prefix env vars → `GALAXY_`
- `~/.warp/` → `~/.galaxy-ai/`
- Warp AI server endpoints → `BedrockClient::converse_stream`
- Warp Drive → local storage in `~/.galaxy-ai/` or `.galaxy/`
- `WarpAI` / `warp_ai` → `GalaxyAI` / `galaxy_ai`

## Reference: Bedrock Integration Points

When replacing Warp AI calls with Bedrock:

- Client: `app/src/ai/bedrock/client.rs` — `BedrockClient::converse_stream`
- Request building: `app/src/ai/bedrock/convert_request.rs`
- Response parsing: `app/src/ai/bedrock/stream.rs`
- Tool definitions: `app/src/ai/bedrock/tool_docs.rs`
- AWS credentials: `app/src/ai/aws_credentials.rs`
- Model selection: `app/src/ai/llms.rs`

All AI features MUST flow through these modules. Direct HTTP calls to any LLM provider are forbidden.

## Reference: Local Storage Patterns

When replacing Warp cloud storage:

- **User preferences / global state**: `~/.galaxy-ai/<feature_name>/`
- **Project-scoped state**: `<project_root>/.galaxy/<feature_name>/` (must be gitignored)
- **Structured persistent data**: Use Diesel ORM + SQLite via `app/src/persistence/`
- **Cached/temporary data**: `$TMPDIR/galaxy_<feature_name>/`

---

## Failure Modes — When to STOP

STOP the port and inform the user if:

1. The feature's core functionality requires real-time communication with Warp's servers and there is no local alternative.
2. The feature's AI usage cannot be expressed as Bedrock Converse API calls (e.g. it requires fine-tuned Warp-specific models with no public equivalent).
3. The feature depends on Warp-proprietary data formats or protocols that are not documented in the open-source repo.
4. Porting the feature would require modifying more than 30% of Galaxy's existing codebase — this suggests architectural incompatibility.
5. The feature's tests all depend on Warp server mocks that have no Galaxy equivalent, making it untestable.

In these cases, present the user with:
- WHY the port is blocked
- WHICH specific dependency is the blocker
- WHETHER a partial port (subset of functionality) is viable
- WHAT alternative approaches might achieve similar UX without the blocked dependency
