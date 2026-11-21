# Galaxy documentation

Galaxy is a local-first developer terminal with an agent that works alongside your shell. Conversations, preferences, and personal workspace content are kept on your machine; you choose the model providers, agent runtimes, and external tools to connect.

This wiki documents Galaxy's behavior. Inherited links to Warp documentation do not establish that a Warp-hosted feature is available in Galaxy. See the [Warp documentation reference audit](https://github.com/rkw6086/galaxy/wiki/Warp-Documentation-Reference-Audit) for the remaining links and migration priorities.

## Start here

1. [Install an official build](https://github.com/rkw6086/galaxy/wiki/Installation-and-Updates) for Windows x64, Linux x64, or Apple Silicon macOS.
2. [Connect a provider](https://github.com/rkw6086/galaxy/wiki/Providers-and-Models) in Settings → Providers and enable the models you intend to use.
3. [Choose a profile and permissions](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools). Start with Ask First while learning how actions are presented.
4. Open a project directory and begin a conversation. Use [background jobs and the activity inspector](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools#background-commands) to follow execution.

## Guides

| Topic | What it covers |
| --- | --- |
| [Installation and updates](https://github.com/rkw6086/galaxy/wiki/Installation-and-Updates) | Packages, application launchers, stable/Nightly tracks, verification, and automatic updates |
| [Terminal and workspace](https://github.com/rkw6086/galaxy/wiki/Terminal-and-Workspace) | Commands, tabs/panes, shortcuts, appearance, and launch configurations |
| [Providers and models](https://github.com/rkw6086/galaxy/wiki/Providers-and-Models) | OpenAI, LiteLLM, ChatGPT subscription, Anthropic, Bedrock, ACP, model discovery, and context budgets |
| [Profiles, permissions, and tools](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools) | Action approval, provider restrictions, MCP, rules, plans, and background commands |
| [Data and privacy](https://github.com/rkw6086/galaxy/wiki/Data-and-Privacy) | Local storage, credentials, model traffic, diagnostics, and backups |
| [Troubleshooting](https://github.com/rkw6086/galaxy/wiki/Troubleshooting) | Startup, shell, provider, context, command, update, and diagnostic problems |
| [Technical overview](https://github.com/rkw6086/galaxy/wiki/Technical-Overview) | Native app structure, one-turn provider runtimes, durable runs, tools, and compaction |
| [Building from source](https://github.com/rkw6086/galaxy/wiki/Building-from-Source) | Native prerequisites, Cargo targets, packaging, and contributor workflow |
| [Warp documentation reference audit](https://github.com/rkw6086/galaxy/wiki/Warp-Documentation-Reference-Audit) | Inherited help/support URLs, exact callsites, compatibility references, and replacement guidance |

## Product boundaries

The normal Galaxy desktop build disables the inherited Warp server connection. Galaxy does not require a Warp account for a fresh local installation. Model providers still need their own applicable authentication, and an ACP agent controls its own authentication and runtime.

Local Docker Sandbox is an unfinished prototype. Its presence in a menu does not mean that project provisioning, agent commands, file access, or cleanup are fully container-aware. It should not be treated as a verified isolation boundary. The inherited hosted environment wizard is not a supported Galaxy cloud-provisioning service. See [known limitations](https://github.com/rkw6086/galaxy/wiki/Troubleshooting#docker-and-hosted-environments).

## Project and support

- [GitHub source and public contributions](https://github.com/rkw6086/galaxy)
- [Official downloads](https://github.com/rkw6086/galaxy/releases)
- [Report an ordinary bug](https://github.com/rkw6086/galaxy/issues)
- [Primary development repository on Gitea](https://git.ryserve.net/public/galaxy)

For a bug report, include the exact version or Nightly commit from About, your operating system and architecture, reproduction steps, expected and actual behavior, and sanitized diagnostics. The [support and security note](https://github.com/rkw6086/galaxy/wiki/Troubleshooting#reporting-a-problem) explains the current private-reporting gap; do not send Galaxy security reports to inherited Warp contacts.

Documentation baseline: [source `600d7970c`](https://github.com/rkw6086/galaxy/commit/600d7970c55e6f388ea61ab6d9f95fc8e33cd2ca). Code inspection establishes implementation details; platform installers and UI behavior still require separate runtime verification. Check the release notes and About page for the build you are using.
