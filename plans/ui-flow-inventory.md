# Galaxy UI Flow Inventory

> **Started:** 2026-08-04
> **Status:** First-pass surface classification; action-level trace audit in progress
> **Architecture:** [`galaxy-local-first-rig.md`](galaxy-local-first-rig.md)

## How this ledger is used

This is the source of truth for deciding what Galaxy keeps, rebuilds, or removes. A directory name is
not a product decision. Each user intent is traced from every entry point through state, persistence,
runtime/network dependencies, and rendered outcomes.

Audit sources include:

- root/onboarding states in `app/src/root_view.rs`;
- registered workspace actions in `app/src/workspace/action.rs`;
- app menus, command palette, keybindings, context menus, URI handlers, and toolbar buttons;
- settings navigation and widgets under `app/src/settings_view`;
- left/right panels and terminal/agent input modes;
- existing integration-test modules under `app/src/integration_testing` and `crates/integration`;
- feature flags that make otherwise hidden flows reachable in OSS/dogfood builds.

For each kept or rebuilt flow, completion means checking:

- [ ] every mouse, keyboard, command-palette, menu, URI, startup, and programmatic entry point;
- [ ] empty, loading, success, partial-stream, denied, cancelled, offline, error, and retry states;
- [ ] close/reopen, restart, and session-restore behavior where state persists;
- [ ] focus, hover, accessibility, and context-flag behavior;
- [ ] local writes and migration behavior;
- [ ] every network destination and the user intent that authorizes it;
- [ ] unit and integration coverage;
- [ ] removal of obsolete actions, flags, settings, assets, and service code after migration.

Status values:

- **Keep/local:** core behavior remains and must require no service.
- **Keep/explicit:** remote behavior remains only behind explicit user/admin configuration or action.
- **Rebuild:** preserve the intent/UI value but replace its backing service or state model.
- **Remove:** the intent belongs to Warp's hosted product and should disappear completely.
- **Audit:** disposition or reachability still needs code/runtime validation.

## Flow ledger

| ID | Surface and user intent | Current coupling observed | Target disposition | Status |
|---|---|---|---|---|
| BOOT-01 | Launch app and reach a usable workspace | Root auth/onboarding state, server API provider, auth manager, cloud/update models | Launch directly into local workspace; provider setup is optional and non-blocking | Rebuild |
| BOOT-02 | First-run education and appearance setup | Agent onboarding, login slide, server `is_onboarded` state | Local onboarding focused on terminal mode, privacy boundary, and provider/ACP choices | Rebuild |
| BOOT-03 | Restore windows, tabs, panes, CWDs, and agent conversations | SQLite plus cloud-shaped conversation/object state | Local SQLite restore only | Keep/local |
| BOOT-04 | Sign in, sign out, reauth, SSO, anonymous user | Firebase/Warp auth and account UI | No account in OSS | Remove |
| WS-01 | Create, close, reorder, rename, pin, group, and color tabs | Workspace action/controller mega-file | Preserve behavior; split state ownership later | Keep/local |
| WS-02 | Split, close, focus, rename, maximize, and navigate panes | PaneGroup, Workspace, terminal model | Preserve | Keep/local |
| WS-03 | Save/launch tab configurations and worktrees | Local TOML/repo plus some telemetry/cloud vocabulary | Preserve as local templates | Keep/local |
| WS-04 | Open settings, resource center, logs, and diagnostic panes | Mixed local and server/account actions | Preserve local pages; remove hosted links/actions | Rebuild |
| TERM-01 | Run shell commands and view structured blocks | Terminal/UI core | Preserve | Keep/local |
| TERM-02 | Search command history, blocks, commands, files, and palettes | SQLite/local index plus cloud object sources | Preserve local sources; remove hosted sources | Rebuild |
| TERM-03 | Use SSH, remote shells, and Wormhole/wormholing | Remote host and remote-server components | Keep only explicit remote-host behavior; audit branding and hidden service calls | Keep/explicit |
| TERM-04 | Share a terminal/session by URL or QR code | Warp session-sharing service | No hosted replacement in local-first scope | Remove |
| TERM-05 | Sync terminal input across panes/tabs | Local workspace state | Preserve | Keep/local |
| AGENT-01 | Start an agent conversation in a tab/pane | Blocklist controller, Warp proto request, provider/ACP branch | Route through `AgentRuntime` | Rebuild |
| AGENT-02 | Select provider, model, profile, and context limits | LLM preferences, Bedrock/OpenAI settings, ACP special cases | Unified provider registry; capability-aware controls | Rebuild |
| AGENT-03 | Compose prompts with files, selections, images, rules, and project context | Context chips, cloud-shaped rules, provider-specific request fields | Galaxy domain content/context builder | Rebuild |
| AGENT-04 | Watch text, reasoning, status, usage, and stop state stream | Provider translators emit Warp response events | Render `AgentEvent` stream | Rebuild |
| AGENT-05 | Review/approve/deny shell, file, MCP, and other tool calls | Blocklist action model and permissions; ACP has a parallel policy | One Galaxy tool/permission lifecycle shared by Rig and ACP | Rebuild |
| AGENT-06 | Cancel, interrupt, queue, send-now, or steer a running turn | ResponseStream/PendingResponseStreams and ACP steering | Provider-neutral turn control | Rebuild |
| AGENT-07 | Rename, pin, resume, fork, summarize, rewind, or delete conversations | SQLite plus server/cloud conversation vocabulary | Preserve meaningful local operations; remove cloud handoff/link actions | Rebuild |
| AGENT-08 | Inspect context usage, costs, and progressive summary | Bedrock-specific history fields and usage mapping | Provider-neutral usage; cost shown only when pricing is known/configured | Rebuild |
| AGENT-09 | Spawn and inspect child agents/orchestration | Warp MAA task schema, blocklist orchestration, some cloud assumptions | Defer until single-agent Rig tools are stable; local-only implementation | Audit |
| AGENT-10 | Start/restore an ACP-backed conversation | ACP runtime + separate ResponseStream branch | `galaxy_agent_acp` peer runtime with shared transcript and permissions | Rebuild |
| AGENT-11 | Detect/manage CLI agents and notifications | Agent SDK, Codex/OpenCode/Claude/Gemini harness/plugin code | Keep only ACP configuration and explicitly requested local integrations; remove Warp plugin cruft | Audit |
| AGENT-12 | Run Oz/cloud/ambient/scheduled agents and hand off local/cloud work | Agent SDK, cloud environments, Warp APIs, RTC | Hosted intent is out of scope | Remove |
| AGENT-13 | Configure/use MCP servers and resources | Local files, OAuth, managed/server MCP, tool execution | Keep local/explicit remote MCP; remove managed Warp gallery/secrets dependencies | Rebuild |
| AGENT-14 | Create/use global and project rules and skills | CloudModel AIFacts plus local rule/skill files | Local repository/filesystem only | Rebuild |
| AGENT-15 | Use voice input/transcription | Local capture plus Warp transcription endpoint or provider assumptions | Keep only with an explicit local/configured transcription backend | Audit |
| CODE-01 | Browse project files and global search | Local filesystem/index plus remote indexing branches | Preserve local; remote only for explicit SSH session | Keep/local |
| CODE-02 | Edit files with LSP completion, diagnostics, actions, rename, and signature help | Local filesystem/LSP/runtime downloads | Preserve; downloads are explicit product-maintenance egress | Keep/local |
| CODE-03 | Review local Git diffs, comments, stage/revert, commit | Local Git plus optional remote/GitHub models | Preserve local Git review | Keep/local |
| CODE-04 | Fetch PR metadata, push, or authenticate GitHub | Git/GitHub/server integration paths | Keep ordinary explicit Git operations; remove Warp-mediated GitHub auth | Rebuild |
| DRIVE-01 | Open Galaxy Drive/content library and navigate folders | Drive UI backed by CloudModel/UpdateManager/GraphQL | Local content library over SQLite | Rebuild |
| DRIVE-02 | Create/edit/import/export notebooks | Cloud object ownership/sync around useful local editors | Preserve editor; replace repository | Rebuild |
| DRIVE-03 | Create/edit/run/import/export workflows | Cloud object ownership/sync around useful local runner/UI | Preserve runner/editor; replace repository | Rebuild |
| DRIVE-04 | Manage environment-variable collections and external secrets | Cloud objects, server-managed secrets, local execution | Local encrypted/OS-keychain-backed storage; never cloud sync | Rebuild |
| DRIVE-05 | Manage profiles, rules, prompts, and MCP objects | Cloud object polymorphism | Local typed repositories | Rebuild |
| DRIVE-06 | Share objects, team folders, team roles, and sync conflicts | Warp cloud/team services | No hosted replacement in current scope | Remove |
| SET-01 | Change appearance, fonts, themes, terminal behavior, keyboard shortcuts | Local settings plus some cloud preference sync | Local settings only | Keep/local |
| SET-02 | Configure AI providers, models, profiles, ACP, MCP, rules, and experiments | One 8,500-line page with provider/hosted modes interleaved | Split by intent and capability; remove hosted modes | Rebuild |
| SET-03 | Configure privacy, telemetry, crash reporting, and cloud storage | No-op telemetry plus hosted-setting vocabulary | Replace with a read-only local-first network/privacy status page | Rebuild |
| SET-04 | Teams, billing, usage plans, referrals, upgrades | Warp account/services | Remove | Remove |
| SET-05 | About, update check, release notes, diagnostics | Local info plus remote release/service URLs | Keep; network operations separately disclosed/configured | Keep/explicit |
| NET-01 | Emit telemetry, analytics, remote logs, or crash reports | Most send macros are no-op, but schemas and hooks remain | Delete runtime path and eventually schemas/call sites | Remove |
| NET-02 | Discover models and call inference | Bedrock SDK, OpenAI client, provider routing map | Rig provider registry; selected provider only | Rebuild |
| NET-03 | Open web links, web fetch/search, browser/computer use | External URLs and agent tools | Explicit user action/policy with visible destination class | Keep/explicit |
| NET-04 | Check/download updates, fonts, LSPs, runtimes, or plugins | Several independent download paths, including inherited server-root usage | Audit each destination; allow only signed/pinned, explicit maintenance paths | Audit |
| UI-01 | Use command palette, menus, keybindings, context menus, toolbar, and URI routes | Hundreds of action variants include both local and hosted intents | Retain as entry-point layer; remove every obsolete registered action | Audit |
| UI-02 | Receive notifications, toasts, modals, and banners | Local status mixed with billing/login/Oz/agent marketing | Preserve local status; remove hosted/marketing state machines | Rebuild |
| UI-03 | Accessibility, focus, mouse/hover, themes, and responsive panels | GalaxyUI view state | Preserve and cover while splitting views | Keep/local |

## First reachability findings

1. The OSS binary enabled dogfood flags, including ACP and multiple experimental local/remote UI
   features. Audit cannot assume a `DOGFOOD_FLAGS` item is unreachable in OSS.
2. Telemetry send macros and collectors are no-ops, but thousands of telemetry event definitions and
   call-site dependencies remain architectural glue.
3. The settings sidebar exposes Agents, Code, Appearance, Features, Keyboard shortcuts, Wormhole,
   Galaxy Drive, Privacy, About, and optionally Galaxy Control. The Agents page combines Galaxy
   Agent, Profiles, MCP servers, Knowledge, third-party CLI agents, Bedrock, OpenAI/LiteLLM, and
   Experiments.
4. The left panel combines Project Explorer, Global Search, Galaxy Drive, and Conversation List.
   Code Review is a separate right panel. This is a useful UI shell, but both panels currently import
   cloud/telemetry vocabulary.
5. `WorkspaceAction` still registers login, upgrade, sharing, team Drive creation, cloud handoff,
   cloud-agent setup, Oz install/launch, ambient agents, and other hosted actions alongside core tab,
   pane, terminal, editor, and local-agent actions.

## Audit order

The action-level audit proceeds in this order because each later surface depends on the earlier
state boundary:

1. boot/onboarding and network initialization;
2. workspace/tabs/panes and session restoration;
3. terminal input, blocks, history, and search;
4. provider-backed agent conversation happy path;
5. tool permissions, errors, cancellation, queueing, and restore;
6. ACP parity;
7. local content library and settings;
8. editor/code review/remote development;
9. removal sweep across menus, palette, URI routes, banners, modals, flags, and tests.

The ledger is complete only when every user-visible action variant has a flow ID or has been deleted.
