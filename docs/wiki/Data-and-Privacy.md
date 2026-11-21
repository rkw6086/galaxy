# Data and privacy

Galaxy's normal desktop build is local-first. Your conversations, preferences, and personal workspace content are stored locally. Connecting a model provider, ACP agent, MCP server, or network tool adds the corresponding external destination; local-first does not mean that a remote model can answer without receiving the context sent to it.

## Local files

On the normal macOS/Linux channel, Galaxy uses `~/.galaxy` for configuration and local data. Developer/integration/data-profile variants can use a different suffix. Windows uses platform application-data directories; use the app's settings-file action to locate the active configuration rather than assuming a Unix home path.

Important categories include:

| Data | Location or owner |
| --- | --- |
| TOML settings | `settings.toml` in the active configuration directory |
| Legacy preference compatibility | `user_preferences.json` in that directory |
| Galaxy MCP home configuration | `.mcp.json` in the Galaxy home configuration directory |
| Themes, launch configurations, workflows, skills | Local Galaxy data/configuration directories |
| Conversations, workspace state, local Drive objects, profiles | Local persistence, including SQLite-backed state |
| Background job history | The `background-jobs` directory under local app data |
| Retained background process output | Private temporary files owned by the background-job manager |

Legacy names such as `Warp`, `.warp`, protocol identifiers, or `ShellLaunchData` can remain in configuration compatibility and source code. Renaming those fields indiscriminately can break old data. They do not, by themselves, indicate a live connection to a Warp-hosted service.

Back up the app's local configuration/data while Galaxy is closed if you need a recoverable snapshot. Treat the backup as sensitive. Do not delete the local data directory as a first troubleshooting step: it may contain conversations, profiles, rules, configuration, and credentials or credential references.

## Credentials

Use the provider's setup UI for authentication. Credential handling differs by provider. Direct Claude subscription/OAuth credentials use Galaxy secure storage with owner-only fallback files and references in settings; ACP agents own their own authentication stores. Other provider entries/settings can contain local credentials, so settings are not a safe public diagnostic attachment.

Galaxy does not need to read Claude Code's private credential store to launch a Claude Code ACP session. Do not copy an external agent's token store into an issue or wiki example.

## Model and tool traffic

A configured direct provider receives the request Galaxy constructs: selected history, instructions, tools, and relevant content attached to the request. Model tools may receive file or command results needed for the task. ACP sends data to the external agent, whose own runtime determines its internal provider calls.

MCP and network tools have their own configured destinations. Profiles and permission boundaries control which actions are available and executable. A provider connection is not permission for every tool.

Other explicit product actions also use the network: official update checks/downloads use GitHub releases, ACP setup can download its pinned runtime packages, and help links open the browser. Some inherited help links still point to Warp. The providers/MCP list is therefore not an exhaustive egress inventory; see the [reference audit](https://github.com/rkw6086/galaxy/wiki/Warp-Documentation-Reference-Audit) for the remaining upstream links.

## Local activity capture

The AI Activity inspector starts capture only after first opening. It is an in-memory diagnostic view, independent of remote logging. It retains at most 2,000 events and 16 MiB, with a per-capture content bound; eviction/truncation is explicit.

Transport headers and provider credentials are excluded from the structured capture, but user-supplied model/tool content may contain secrets or private files. Review the captured content before sharing screenshots or copying details. Closing the inspector leaves recording active until paused; Clear removes retained events without changing running work.

## Remote diagnostics

Remote logging is a separate opt-in, disabled by default. The `ai.remote_logging` settings select the endpoint and credential. The ordinary opt-in can upload native warnings/errors and AI diagnostic events. Raw model request/response payload capture is a further separate setting and is also disabled by default.

Raw payloads can include prompts, generated text, tool arguments, and file content. A remote diagnostic service is an external destination for that data. Configure it only when you intend to send diagnostics there, and review any exported material before adding it to a public issue.

Delivery is bounded and best-effort: queue limits, application exit, a process crash before logging is initialized, or a network interruption can leave gaps. Missing remote events are not proof that an action did not occur. Check local logs and correlate the relevant session/run identifiers when investigating a specific request.

## Implementation references

- [Local storage paths and migration](https://github.com/rkw6086/galaxy/blob/master/crates/galaxy_core/src/paths.rs)
- [Local settings paths](https://github.com/rkw6086/galaxy/blob/master/app/src/settings/mod.rs)
- [Provider and diagnostic settings](https://github.com/rkw6086/galaxy/blob/master/app/src/settings/ai.rs)
- [Credential references](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/claude_subscription.rs)
- [Activity capture bounds](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/activity/mod.rs)
- [Remote diagnostic implementation](https://github.com/rkw6086/galaxy/blob/master/app/src/ai/remote_logging.rs)
