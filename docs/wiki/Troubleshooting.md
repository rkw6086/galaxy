# Troubleshooting

Start by recording the exact Galaxy version or Nightly commit from **Settings → About**, your OS/version/architecture, and the smallest sequence that reproduces the problem. A provider/model name alone is often insufficient when several connections or profiles are configured.

## Install or update problems

- Confirm the package matches your architecture. Current official packages are Mac ARM, Linux x64, and Windows x64.
- Use the release's exact installer filename and checksum entry. A Nightly filename identifies one build; a newer rolling Nightly may replace its previous assets.
- Keep the portable Linux executable and its `resources` directory together. A portable archive does not install an application-list entry.
- For Debian/Ubuntu, install the downloaded `.deb` with APT to resolve declared dependencies and register the desktop launcher.
- The current Windows installer is unsigned. Mac signing does not supply a Windows publisher certificate.
- Stable builds follow stable updates; Nightly/older Beta builds follow Nightly. Local builds and old versions without updater support may require a manual official installation.
- Linux automatic package updates need the canonical packaged installation, `galaxy-oss` package registration, `apt-get`, `pkexec`, and a functioning desktop authorization agent. A password is handled by the OS, not read by Galaxy. If that path is unavailable, use the offered download/manual-install path.

See [Installation and updates](https://github.com/rkw6086/galaxy/wiki/Installation-and-Updates) for the normal procedure. Do not remove quarantine or security checks merely because an installer failed; first identify whether you have the current official package and its exact error.

## Shell cannot start / `openpty failed`

A red shell-start banner indicates that the shell/terminal path failed to initialize; it is not, by itself, a model-provider error. Record the full OS error text, selected shell, and whether it happens in a new normal tab, a split pane, or a restored workspace.

Check that the selected shell executable still exists. Compare a new terminal tab with the failing restored/split session and restart Galaxy once after collecting the failure details. Do not delete conversation or workspace data as a first response. An `openpty` error alone does not identify which resource or session transition caused it.

If remote diagnostics are already enabled, the shell-start banner has a dedicated `terminal_pty_spawn_failed` event containing relevant terminal/session/split identifiers. Local logs remain important if delivery failed.

## Provider test, discovery, or requests fail

1. Check the active connection's endpoint, authentication, and enabled models.
2. Check the active profile and its model roles/provider restrictions.
3. Use the connection test for discovery/access errors; then try a small request with the intended model.
4. Open `/ai-activity` before reproducing a turn so it can record the request, retries, tools, and terminal outcome.

For compatible gateways, use the intended API base path. A model listed by discovery does not guarantee tool/vision support or access to every backend route. For Bedrock, verify the exact AWS account/profile, region, entitlement/agreement, and inference-profile/model access implicated by the error.

Repeated provider retries are bounded. Preserve the final error and run/session identifiers rather than diagnosing the retry count alone. The finish diagnostic can retain a short history of sanitized model failures, including failures whose individual events were lost during a connection interruption.

## Unexpected provider or model selection

Review both the enabled provider connections and the active profile. The base model, terminal-use model, computer-use model, coding model, and allowed orchestration providers can differ. A profile without an explicit orchestration-provider restriction can include all configured providers; an explicitly empty provider selection permits none.

If the selected route remains unexpected, capture the conversation/session label, provider connection name, model ID, active profile, and activity entries. Do not enable credentials for an unwanted provider merely to make the error go away.

## Context limits or early compaction

Maximum context, ordinary/default input budget, profile cap, fixed prompt/tool cost, and reply reservation are separate values. Compaction can start early enough to accommodate a smaller role-specific model and output reservation. A percentage of the provider's largest advertised window is not the entire scheduling decision.

For a concrete failure, capture the discovered default/input/maximum/output limits and active profile cap. Older builds can reject a gateway that advertises an entire context window as its output limit before the request reaches the server. Current request projection repairs an impossible fixed output reservation when it can, while still rejecting requests whose actual fixed overhead cannot fit. Refresh the catalog or correct server/model metadata as appropriate; do not blindly raise every limit to the maximum.

See [Providers and models](https://github.com/rkw6086/galaxy/wiki/Providers-and-Models#understand-context-and-reply-limits).

## Background command problems

Open `/background-jobs` and inspect the owning job's actual status, exit code, and **View logs** output. A shortened inline/model result is not a complete log. Cancelling a conversation is different from terminating the job's owned process tree.

Agent background jobs have no interactive input or PTY. Password prompts and full-screen interactive tools need a normal user-controlled terminal. For a missing program or variable, compare the selected shell/environment and working directory with a normal terminal; include the missing executable/variable name and relevant non-secret PATH details in the report. Never attach the full environment if it contains credentials.

## MCP server cannot connect

Check the configured launch command or URL, server logs in MCP settings, and whether the server needs authentication. A relative command can resolve differently from a full path. Check the active profile's MCP restrictions and the source configuration file detected by Galaxy. Legacy project filenames may be supported for compatibility even though Galaxy's home data directory is `.galaxy`.

## Logs and activity

The normal desktop log is `galaxy.log`, with rotated files alongside it:

| Platform | Normal log directory |
| --- | --- |
| macOS | `~/Library/Logs/` |
| Linux | Active Galaxy state directory, normally `~/.galaxy/` |
| Windows | `logs` under the active platform state directory |

Developer/data profiles can change directories. The AI Activity inspector is separate: it starts recording only when opened, remains active after closing until paused, and is bounded in memory. It shows Galaxy snapshots, not exact upstream HTTP payloads. For ACP, external-agent internal model calls remain outside Galaxy's capture.

Remote diagnostics are another independent opt-in. Raw model payload capture can contain prompts, file content, and tool data. Missing remote events are not proof of missing work because upload queues and shutdown delivery are bounded. Review [Data and privacy](https://github.com/rkw6086/galaxy/wiki/Data-and-Privacy) before sharing any capture or log.

Maintainers investigating Bedrock failures can enable `GALAXY_BEDROCK_DIAGNOSTICS=1`; verbose local tool queue diagnostics use `GALAXY_TOOL_DIAGNOSTICS=1`. These are diagnostic modes, not routine performance settings. Bedrock snapshots can contain serialized context and tool definitions and must be handled as sensitive.

## Docker and hosted environments

Local Docker Sandbox is not ready to be treated as a complete, verified isolated agent environment. The reviewed implementation has these gaps:

- It requires a separately installed compatible `sbx`; Docker Engine alone is insufficient, and missing-helper onboarding is incomplete.
- Initialization still looks up a fixed hosted environment rather than the selected user's image/project configuration.
- Bootstrap loses the sandbox launch identity in a way that affects later command routing.
- Agent commands, file access, and cleanup are not fully container-aware. Correctly identified Docker sessions explicitly reject unsupported background commands.
- Windows sandbox shell launch is unsupported.

The inherited `/create-environment`/hosted cloud form is a separate, incomplete upstream flow. It is not an alternative Galaxy cloud-provisioning service. A shell opening inside Docker would verify only shell startup, not the agent's file/command isolation or project provisioning.

Inherited hosted billing, account/team, sharing, and cloud-trigger screens are also not a promise that Galaxy operates those upstream services. A link to Warp billing or hosted help should not be used as a Galaxy setup instruction.

## Reporting a problem

Use [Galaxy GitHub Issues](https://github.com/rkw6086/galaxy/issues) for ordinary bugs. Include:

- Version/Nightly commit and OS/architecture.
- Reproduction steps, expected result, and actual result.
- Whether it occurs in a normal tab, split, restored session, remote session, or Docker prototype.
- Provider connection name/model ID and active profile when relevant.
- Sanitized error text, timestamps with timezone, and session/run IDs available in the activity capture.

Review attachments before submitting them. Remove credentials, authentication headers/tokens, private file content, and unrelated conversation data. Do not post a security exploit or secret publicly to establish a private contact.

**Private security reporting is a known documentation gap.** At this audit, GitHub private vulnerability reporting was disabled for the Galaxy repository, while `SECURITY.md` and the inherited FAQ still pointed to Warp's security contacts/advisories. Those upstream contacts are not verified Galaxy reporting channels. A maintainer must establish and publish Galaxy's private reporting route; this wiki does not invent an email address or promise an enabled reporting button.

## Implementation references

- [Logging paths](https://github.com/rkw6086/galaxy/blob/master/crates/galaxy_logging/src/native.rs)
- [Desktop log name](https://github.com/rkw6086/galaxy/blob/master/app/src/bin/oss.rs)
- [Activity and remote logging invariants](https://github.com/rkw6086/galaxy/blob/master/AGENTS.md#local-ai-activity-inspector)
- [Background command execution](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/background_jobs/runtime.rs)
- [Docker initialization](https://github.com/rkw6086/galaxy/blob/master/app/src/terminal/view/docker_sandbox/mod.rs)
- [Docker launch identity](https://github.com/rkw6086/galaxy/blob/master/app/src/terminal/local_tty/terminal_manager.rs)
