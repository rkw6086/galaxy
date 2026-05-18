# Galaxy AI Agents - Ideas & Future Work

## Build Standards

The project must always have a **clean build with zero warnings and zero errors**. This applies to both `cargo check` and `cargo build`. Dead code warnings (`unused`, `dead_code`) should be resolved by either using the code, removing it, or adding targeted `#[allow(dead_code)]` annotations with a reason (e.g., code that's intentionally staged for upcoming work).

---

## LLM-Powered Predictive Autocomplete

**Idea:** As the user types in the code editor, stream the current context (surrounding code, file structure, recent edits) to an LLM and predict what they're about to write — offering inline ghost-text completions similar to GitHub Copilot.

**Scope options:**
- By line (predict the rest of the current line)
- By function (predict the full function body)
- By class/module (predict structural code)

**Challenges:**
- Latency: can't hit the LLM on every keystroke. Need aggressive debouncing (500ms+), speculative pre-fetching, and streaming partial results.
- Cost: high token volume. May need a small/fast model (Haiku) for inline suggestions with a larger model for multi-line predictions.
- Context window: need to efficiently pack relevant context (current file, imports, related types, recent edits) without blowing the token budget.
- Cancellation: must cancel in-flight requests when the user keeps typing past the prediction point.
- UX: ghost text rendering, Tab to accept, partial accept (word-by-word), dismiss on divergence.

**Possible approaches:**
- Debounce + streaming: wait 500ms after last keystroke, stream tokens as they arrive, render as ghost text
- Predictive pre-fetch: on function signature completion or newline, proactively request the likely next block
- Local model: run a small code model locally for instant line completions, use cloud model for multi-line
- Hybrid: use LSP completions for symbol-level, LLM for line/block-level predictions

**Integration points in Galaxy:**
- `app/src/code/completion.rs` — extend the completion state machine with an LLM provider
- `crates/ai/` — existing Bedrock/LLM infrastructure can be reused
- Editor decoration system — for rendering ghost text (similar to inlay hints)

---

## Inline Token/Cache/Cost Stats on LLM Responses

**Idea:** Display context window usage, cache hit percentage, and cost as a compact footer below each completed LLM response in the agent conversation view. This replaces the "context" button on the bottom-right of the input area.

**Data to display (per response):**
- Context usage: percentage used, input tokens / context window size (e.g., "Context: 45.2% (20.6k / 200k)")
- Cache hit stats: hit percentage with breakdown (e.g., "Cache Hit: 89.3% (R: 18.4k, W: 1.2k, M: 1.0k)")
- Cost: cumulative session cost (e.g., "Cost: $0.42")

**Data source:**
- Bedrock `InvokeModel`/`Converse` response metadata contains:
  - `usage.input_tokens` — tokens sent (cache misses)
  - `usage.cache_read_input_tokens` — tokens served from cache
  - `usage.cache_creation_input_tokens` — tokens written to cache
  - `usage.output_tokens` — tokens generated
- Cache hit % = `cache_read / (cache_read + cache_write + input_tokens) * 100`

**Reference implementation:**
- `~/.claude/statusline-command.sh` — shell script that formats these exact metrics for Claude Code's status line. Same formula and human-readable formatting (k/M suffixes) should be used.

**UI approach:**
- Render as a single-line or two-line muted footer below each AI response block
- Use dimmed/secondary text color, monospace font, compact layout
- Remove the "context" icon button from the input area bottom-right since this replaces it

**Integration points in Galaxy:**
- Find where Bedrock response `usage` metadata is captured after each streaming response completes
- Find the conversation block rendering (where each AI response ends) to add the footer element
- `app/src/ai/blocklist/` — likely where response blocks are rendered
- `crates/ai/` — where Bedrock API responses are parsed

---

## LSP Rename (Phase 3 - App Wiring)

**Status:** LSP layer is complete (`prepare_rename` + `rename` methods exist on LspServerModel). Needs app-layer wiring.

**Implementation needed:**
- F2 keybinding triggers `prepareRename` at cursor position
- If valid, show an inline text input overlay at the symbol location pre-filled with the current name
- On confirm (Enter), send `rename` request with the new name
- Apply the resulting `WorkspaceEdit` to the editor (single-file for now)
- On cancel (Escape), dismiss the input overlay
