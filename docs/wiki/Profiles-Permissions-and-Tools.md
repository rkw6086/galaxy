# Profiles, permissions, and tools

A profile combines model choices, instructions, and action permissions. Select the intended profile for the conversation before asking the agent to change files or run commands. A model's suggestion is not itself an authorization grant.

## Permission modes

Each profile has one permission mode. Explicit command/MCP denylists, protected paths, workspace restrictions, and execution ownership still apply.

| Mode | Normal behavior |
| --- | --- |
| Read Only | Allows inspection and search. Commands require an explicit command allowlist exception; other mutations, MCP calls, computer use, and delegation are rejected. |
| Ask First | Presents an approval pane for actions that require confirmation. Shell-command approval is tied to the command, directory, host/session, shell, and profile. |
| Galaxy Decides | Runs a separate tool-free risk assessment for actions that need it. Low-risk results can execute; higher-risk or invalid/unavailable assessments deny with a reason. It does not pause for human approval. |
| Allow All | Skips independent model review and human approval for Galaxy-owned actions, while explicit restrictions and ownership boundaries remain in effect. |

Ask First is a useful starting point while learning a project's command flow. Approval requests expire; a stale approval pane is not continuing authorization. Changing profile modes clears remembered command approvals. Exact command allowlist entries are separate from remembered approvals and remain subject to explicit restrictions.

Galaxy Decides currently uses a model for actions requiring risk assessment. It has narrow local fast paths for safe inspection and output polling; it should not be described as a completely local rule-only permission engine.

## Model and provider restrictions

Profiles can select base and specialized model roles. Their orchestration-provider selection limits the destinations child agents may use. A profile with no explicit restriction can include all configured providers; an explicitly empty selection permits none.

If only one provider should participate in a task, restrict orchestration providers as well as choosing the base model. See [Providers and models](https://github.com/rkw6086/galaxy/wiki/Providers-and-Models).

## MCP connections

MCP servers make external tools and resources available to the agent. Add/manage a connection through the MCP settings page; `/add-mcp` opens that setup entry point. Check the server's status, launch command or endpoint, and the active profile's tool restrictions before expecting calls to succeed.

File-based MCP configuration is also supported. The Galaxy home-level file is `~/.galaxy/.mcp.json` on the normal macOS/Linux channel. The parser retains project compatibility locations such as `.warp/.mcp.json`, `.mcp.json`, `.codex/config.toml`, and `.agents/.mcp.json`. These compatibility names are not evidence of a Warp service dependency.

For example, a JSON configuration for a server you have already installed can use:

```json
{
  "mcpServers": {
    "example": {
      "command": "/absolute/path/to/your-mcp-server",
      "args": []
    }
  }
}
```

Replace the placeholder command and arguments with that server's documented launch settings. MCP servers run code or contact their configured service; inspect their configuration and use the permissions appropriate to the project. Do not put real tokens in a public repository or issue.

## Rules, skills, and plans

Galaxy Drive contains local rules, profiles, notebooks, workflows, and MCP configuration. Project instructions and skills can provide additional task context; use only the sources relevant to the current task. The built-in editor and skill command can open local skill Markdown files.

Direct-provider plan turns expose plan read/create/edit tools when the profile/session permits them. A task list is separate from a plan document: unfinished list items are not automatically complete when the agent finishes a response. Completed local plans are archived only after the completion and idle-age checks; unfinished or reopened work should remain available.

## Background commands

Agent-requested shell commands run as independent, noninteractive background jobs. They have closed standard input and no PTY. User-typed terminal commands retain the normal interactive terminal path.

- Open `/background-jobs` to inspect command history, running status, logs, and process-tree termination controls.
- The inline command pane shows status; retained full logs are available through **View logs** while its private log file is readable.
- Logs supplied to a model are bounded. A shortened tool result is not a complete terminal transcript; truncation should be explicit.
- Jobs belong to the background-job manager. Cancelling a conversation or sending another message does not necessarily terminate an already launched process.
- Terminal job completion, including its exit status and bounded output, is delivered back to its owning conversation when it can be accepted.

Commands that require a password prompt, confirmation on standard input, or a full-screen interactive program cannot be completed by entering text into the current agent background job. Run those commands yourself in a normal terminal session when appropriate. Approving one command is not approval for a different command or an interactive terminal takeover.

Docker sandbox and WSL command handling have additional limitations; do not assume that a background command is running inside a container simply because a container terminal is visible. See [known limitations](https://github.com/rkw6086/galaxy/wiki/Troubleshooting#docker-and-hosted-environments).

## Inspect activity

Open `/ai-activity` or **Show AI Activity** in the Command Palette. The inspector can record model requests/results, compaction, tools, permission assessments, and background-job events across sessions. Use session labels to follow the conversation that owns an event.

Recording starts only when the inspector is first opened. Closing it does not stop capture; use Pause or Clear deliberately. Captures are bounded and may contain user-supplied prompt/tool/file content. They are local request snapshots, not an exact network packet capture. ACP captures show Galaxy's messages with the external agent, not the agent's hidden internal model traffic.

See [Data and privacy](https://github.com/rkw6086/galaxy/wiki/Data-and-Privacy) before sharing a capture.

## Implementation references

- [Profile permission and ownership invariants](https://github.com/rkw6086/galaxy/blob/master/AGENTS.md#profile-permissions)
- [Profile model/editor](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/execution_profiles/editor/mod.rs)
- [File-based MCP configuration](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/mcp/mod.rs)
- [Background job lifecycle](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/background_jobs/mod.rs)
- [Activity inspector](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/activity/mod.rs)
