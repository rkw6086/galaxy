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
