# Long-Running Command Side-Agent Plan

## Goal

When an Agent Mode shell command runs longer than three seconds, automatically open a side-agent chat that monitors the process, reports meaningful status, and feeds its observations back into the main agent conversation.

The side agent should use the configured profile's lower-cost **coding/typing model** where appropriate. It must not silently rotate across arbitrary models.

## Current architecture

Relevant existing paths:

- `app/src/ai/blocklist/action_model/execute/shell_command.rs`
  - `ShellCommandExecutor` emits `ShellCommandExecutorEvent::ExecuteCommand` when an agent command starts.
  - `action_result_future` polls/waits for command completion and snapshots.
- `app/src/terminal/view.rs`
  - `handle_shell_command_executor_event` executes the command.
  - It already schedules a delayed check using `LONG_RUNNING_COMMAND_DURATION_MS` and detects `block.is_active_and_long_running()`.
  - It currently locks the input in Agent Mode after the command becomes long-running.
- `app/src/ai/blocklist/agent_view/controller.rs`
  - `AgentViewController` enters fullscreen or inline Agent View.
  - `AgentViewEntryOrigin::LongRunningCommand` already exists.
- `app/src/ai/blocklist/context_model.rs`
  - Owns conversation selection and pending query state.
- `app/src/ai/blocklist/controller.rs`
  - `BlocklistAIController::send_request_input` sends requests for an existing conversation.
  - `RequestInput` carries both `model_id` and `coding_model_id`.
- `app/src/ai/blocklist/controller/response_stream.rs`
  - Resolves provider configuration from a model ID.
  - Retry/failure behavior is centralized here.
- `app/src/ai/agent/mod.rs`
  - `AIAgentInput::UserQuery` supports `running_command: Option<RunningCommand>`.
  - `RunningCommand` carries command text, block ID, terminal snapshot, cursor, and alt-screen state.
- `app/src/ai/agent/api/convert_to.rs`
  - Converts local `AIAgentInput` into request API input.
- `app/src/ai/agent/api/convert_conversation.rs`
  - Converts shell command snapshots/results back into agent context.

## Product behavior

### Trigger

- Start a three-second timer when an Agent Mode command begins executing.
- If the command completes, is cancelled, or the block is no longer active before the timer fires, do nothing.
- If it is still active and long-running at three seconds, start monitoring automatically.
- Do not start a second monitor for the same command/block.

### Side-agent conversation

- Create a new local Agent View conversation associated with the same terminal surface.
- Enter the side conversation in the existing fullscreen/subagent chat UI, using `AgentViewEntryOrigin::LongRunningCommand`.
- The side agent must not steal or destroy the main conversation state.
- The UI should clearly indicate that the conversation is monitoring a running command.
- The side agent should use the active profile's `coding_model_id` as its model, because this is the configured typing/lower-cost role.

### Initial monitoring prompt

Generate a structured prompt containing:

- The exact command text.
- Working directory, when available.
- Start time and elapsed time.
- Current output snapshot.
- Exit status: unavailable while running.
- Explicit instructions to:
  - identify normal progress,
  - describe observable success signals,
  - describe observable failure signals,
  - flag suspicious stalls or lack of progress,
  - flag dangerous behavior or unexpected writes,
  - avoid declaring success without evidence,
  - avoid taking actions unless explicitly authorized.

Example guidance:

> Monitor the running command below. Do not assume it succeeded merely because output stopped. Look for concrete completion or success indicators, explicit errors, non-zero exit status, repeated retries, blocked input, lock waits, and no-progress intervals. For database operations, flag a statement that appears stuck while updating a small number of rows, but distinguish a legitimate long-running query from a deadlock or lock wait when possible. Report observations and uncertainty; do not interrupt or modify the process.

### Ongoing updates

- Feed fresh command snapshots/output to the side conversation while it remains active.
- Prefer existing `RunningCommand`/long-running snapshot representations over inventing a second terminal-output protocol.
- Throttle updates to avoid flooding the model and UI. Initial proposal: no more than one update every 2 seconds, and only when output or command state changes.
- Include elapsed time on every update or enough metadata for the side agent to calculate it.
- On completion, send a final update containing:
  - exit code,
  - final output,
  - completion timestamp,
  - whether the command was cancelled or preempted.

### Main-agent context integration

Every side-agent observation must become available to the main agent.

Preferred design:

1. Store a compact monitor transcript on the command's main conversation/action state.
2. When the side agent emits a meaningful observation, append a system/tool-style context item to the main conversation.
3. On command completion, append a final monitor summary to the main conversation before the main agent receives the command result.
4. Preserve the monitor transcript across retries/session restoration where the command conversation is persisted.
5. Avoid duplicating every token from the side-agent stream; persist completed observations and periodic summaries instead.

The main agent should receive explicit provenance, e.g.:

> Side-agent monitor update for command `<id>` at `00:07`: no completion signal observed; output unchanged for 5 seconds; process may be waiting on a database lock. This is an observation, not a confirmed failure.

### Cleanup

- Stop monitor updates when the command finishes, is cancelled, or the terminal view closes.
- Close or mark the side conversation complete without losing its transcript.
- If the user manually opens another conversation, keep the side monitor associated with the original command rather than replacing the main conversation.
- If monitoring fails, report the monitor failure to the main agent but never terminate the user command because the monitor failed.

## Model fallback behavior

The profile has two explicit model roles:

- `RequestInput.model_id`: thinking/base model.
- `RequestInput.coding_model_id`: typing/coding model.

For ordinary requests:

1. Try the selected thinking model.
2. If the provider reports a rate-limit or budget-exhausted error before any client/tool action has occurred, retry once with the profile's configured coding model.
3. Do not retry if both model IDs are equal.
4. Do not rotate through arbitrary models or provider entries.
5. Do not switch models after tools/actions have already executed.
6. If both roles fail, return a clear user-facing rate-limit/budget message.

For the side agent specifically:

- Start directly with `coding_model_id`.
- If it rate-limits, do not silently switch to the thinking model unless a future product decision explicitly allows that.
- Surface that monitoring is unavailable while leaving the main command running.

## Proposed implementation phases

### Phase 1: Model fallback state

Files:

- `app/src/ai/blocklist/controller/response_stream.rs`
- `app/src/ai/blocklist/controller/response_stream_tests.rs`
- `app/src/ai/agent/api/impl.rs`
- `app/src/server/server_api.rs`

Tasks:

- Add a structured provider-budget/rate-limit classification instead of matching only formatted strings at the final UI boundary.
- Add `coding_model_id` to the retry/fallback state available to `ResponseStream`.
- Add a one-time `retry_with_coding_model` path.
- Re-resolve provider configuration using the coding model ID.
- Ensure the retry does not occur after client actions or when IDs match.
- Add tests for:
  - thinking model succeeds,
  - thinking model rate-limits and coding model succeeds,
  - both models rate-limit,
  - same model IDs do not retry,
  - non-rate-limit errors retain existing behavior,
  - post-action rate limits do not silently switch models.

### Phase 2: Command monitor state

Files:

- `app/src/terminal/view.rs`
- `app/src/ai/blocklist/action_model/execute/shell_command.rs`
- likely a new monitor model/module under `app/src/ai/blocklist/`

Tasks:

- [x] Add a terminal-view-scoped monitor registry keyed by command/block ID.
- Replace the current one-off delayed input-lock check with a monitor-start event or extend the existing event with a long-running transition.
- Capture command metadata and snapshots without holding `TerminalModel` locks across async work.
- [x] Add cancellation/completion cleanup paths.
- [x] Add focused unit tests for timer race cases.
- [x] Add deterministic unit coverage for duplicate monitor registration and cleanup.

### Phase 3: Side-agent conversation creation

Files:

- `app/src/ai/blocklist/agent_view/controller.rs`
- `app/src/ai/blocklist/agent_view/conversation_selection.rs`
- `app/src/ai/blocklist/context_model.rs`
- `app/src/ai/blocklist/controller.rs`
- `app/src/terminal/view.rs`

Tasks:

- Add an explicit API for creating a monitoring conversation without treating it as a user navigation action.
- Use `AgentViewEntryOrigin::LongRunningCommand`.
- Ensure the main active conversation ID is not overwritten unintentionally.
- Add a monitor-specific conversation association record.
- Select the coding model for monitor requests.

### Phase 4: Monitoring prompt and updates

Files:

- `app/src/ai/agent/mod.rs`
- `app/src/ai/agent/api/convert_to.rs`
- `app/src/ai/blocklist/controller.rs`
- new monitor prompt/context helpers

Tasks:

- Build the initial structured monitoring prompt.
- Reuse `RunningCommand` and shell snapshot conversion where possible.
- Add throttled snapshot updates.
- Add explicit completion/cancellation summaries.
- Redact secrets using the existing input redaction path before sending snapshots to the side agent.

### Phase 5: Main-agent context bridge

Files:

- `app/src/ai/blocklist/controller.rs`
- `app/src/ai/blocklist/agent_view/` monitor conversation code
- `app/src/ai/agent/conversation.rs`
- persistence models/snapshot code as needed

Tasks:

- Define a structured side-agent observation type.
- Append meaningful observations to the main conversation context.
- Persist completed summaries and association metadata.
- Avoid token-by-token duplication.
- Add tests proving the main request receives monitor observations before and after command completion.

### Phase 6: UI polish and failure handling

Tasks:

- Add a visible “Monitoring command…” indicator in the side-agent header or conversation metadata.
- Show monitor unavailable/rate-limited state without stopping the command.
- Ensure user takeover and command cancellation are represented in both conversations.
- Add telemetry only for high-level lifecycle events, not command output or secrets.

## Safety and correctness requirements

- Never terminate or modify the user's process because the side agent failed.
- Never send unredacted command output or secrets to the side agent.
- Never acquire nested `TerminalModel` locks across async boundaries.
- Do not let side-agent tool calls execute arbitrary commands by default.
- Do not claim success based solely on inactivity.
- Preserve the main agent's existing action ordering and tool-result pairing invariants.
- Keep monitoring optional at the implementation boundary so it can be disabled if the UI or provider is unavailable, while the three-second trigger remains automatic when the feature is enabled.

## Validation plan

Targeted checks:

- `cargo test -p galaxy --lib response_stream`
- `cargo test -p galaxy --lib shell_command`
- `cargo test -p galaxy --lib agent_view`
- `cargo test -p galaxy --lib blocklist`
- `cargo check -p galaxy --lib`

Manual verification:

1. Run `sleep 1`: no side agent opens.
2. Run `sleep 5`: side agent opens automatically at approximately three seconds.
3. Run a command with periodic output: side agent reports progress.
4. Run a command with no output but successful completion: side agent does not claim failure prematurely and receives the final exit code.
5. Run a command that exits non-zero: side agent identifies failure and main agent receives the summary.
6. Run a simulated database lock/wait: side agent flags suspicious lack of progress with uncertainty.
7. Trigger a provider budget limit on the thinking model: coding model is tried once.
8. Trigger limits on both profile models: clear rate-limit message is shown and no crash occurs.
