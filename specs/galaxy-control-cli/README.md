# Galaxy Control operator README
`galaxyctrl` is the CLI entrypoint for controlling an already-running local Galaxy app instance. It is intended for scripts, demos, agent workflows, and developer automation that need to perform allowlisted Galaxy UI actions through the installed channel-specific Galaxy binary without launching the GUI.
The implemented command surface contains exactly 80 typed, allowlisted actions. All 80 actions execute after exact-action credential validation. Close actions flow through normal Galaxy close behavior, so existing app warnings remain authoritative. The Block, Auth, Drive, and History families are absent; `input.insert` plus `input.replace` stage text without submitting it, while the separate Terminal family provides guarded control of commands in existing visible sessions.
## Packaging model
`galaxyctrl` is packaged as a thin channel wrapper rather than a standalone Rust binary. The wrapper resolves the installed channel-specific Galaxy executable and invokes it with the hidden `--galaxyctrl` control-mode flag:
- `crates/local_control` owns discovery records, local authentication material, client transport, protocol envelopes, action names, and error types.
- `crates/galaxy_cli` owns command parsing conventions for local-control subcommands.
- the channel-specific app binary owns the hidden `--galaxyctrl` dispatch path and exits before normal GUI startup.
- the app-side bridge owns the per-process loopback listener and dispatches supported actions onto the live Galaxy UI context.
The control-mode path should initialize only the work needed for CLI parsing, instance discovery, local authentication loading, request serialization, HTTP transport, and output formatting. It should not initialize GUI state, terminal models, rendering, workspaces, or main-app startup paths.
Release artifacts and helper names may be channelized, but operator docs and examples use `galaxyctrl` unless an integration branch explicitly documents a channel-specific alias.
This branch wires the core hidden dispatch contract through the existing Galaxy binary. Platform packaging creates wrapper scripts that call the channel binary with `--galaxyctrl` instead of producing or selecting a separate `galaxyctrl` binary.

## MCP stdio bridge

`galaxyctrl mcp` exposes the current allowlisted Galaxy Control catalog to a
local MCP client over newline-delimited JSON-RPC on stdin/stdout. The bridge
does not implement app actions or hold a broad credential. Each tool call
still resolves the pinned instance, requests a short-lived credential for the
one requested action, and sends the normal authenticated `/v1/control`
request.

Launch the installed wrapper for a specific running Galaxy process:

```bash
galaxyctrl mcp --pid <Galaxy PID>
```

An in-process ACP runtime that launches the current Galaxy executable directly
should use:

```text
<current_exe> --galaxyctrl mcp --pid <Galaxy PID>
```

`--instance <instance_id>` is also supported. The selector is resolved once to
an opaque instance ID when the MCP server starts. Every later tool call
re-discovers only that exact ID, and MCP tool arguments cannot override it. An
unqualified launch succeeds only when exactly one compatible instance is
discoverable; multiple instances fail with `ambiguous_instance` instead of
silently choosing one. The remaining `galaxyctrl` target flags can provide
default window, tab, pane, and session selectors for invoked actions.

In normal catalog mode, the MCP server advertises two tools:

- `galaxy_control_capabilities` invokes the authenticated `capability.list`
  action and returns the exact actions advertised by the selected app.
- `galaxy_control_invoke` accepts one implemented catalog action, an optional
  in-app target, and action-specific parameters. Its JSON Schema is generated
  from the shared action catalog and typed parameter contracts.

The bridge adds no capabilities beyond the shared catalog. In particular,
terminal command control is exposed only through the catalog's typed
`terminal.status`, `terminal.execute`, and `terminal.interrupt` contracts and
their app-side idle/race checks. Filesystem mutation, authentication, and cloud
API surfaces remain absent unless deliberately added to the shared catalog.

Galaxy's in-process ACP integration starts the bridge in a hidden
`--agent-safe` mode. That mode requires opaque `--window` and `--tab`
selectors plus exactly one opaque `--pane` or `--session` selector; it never
falls back to whichever pane is focused later. It advertises
`galaxy_terminal_status` for the delegated pane and only advertises
`galaxy_terminal_execute`, `galaxy_terminal_interrupt`, and
`galaxy_terminal_interrupt_at` when the user's ACP permission profile allows
the corresponding mutations. `galaxy_terminal_interrupt_at` is an MCP
composition over the existing status and interrupt actions: the bridge waits
outside the model loop, verifies that the same block is still running, and
then performs the exact-block interrupt. It is not an additional public
Galaxy Control catalog action.

The ACP host remains separate from Galaxy's LiteLLM and Bedrock provider
dispatch. It launches a configured local adapter, negotiates a standard ACP
authentication method, and gives that process only the agent-safe MCP bridge
for the exact local pane. Galaxy scrubs inherited environment variables before
launch, redacts secrets from outbound prompts, and binds persisted ACP session
IDs to a fingerprint of the effective adapter launch. A restored session is
not sent to an adapter whose launch identity no longer matches.

ACP process management is currently enabled only on Unix platforms, where the
pinned SDK terminates the adapter's complete process group. Galaxy fails closed
before spawning an adapter on Windows until the SDK can retain a Job Object
that kills launcher descendants as well as the direct child.

## Install and invocation guidance
### macOS
For local development checks, build the local Galaxy binary and invoke it with the hidden control-mode flag:
```bash
cargo run -p galaxy --bin galaxy-oss -- --galaxyctrl instance list
```
For distributable checks, use the installed `galaxyctrl` wrapper. The wrapper execs the app bundle's channel-specific executable with `--galaxyctrl`.
Release builds publish a channel-specific macOS `galaxyctrl` archive containing the wrapper, forwarded Galaxy binary, and required resources.
### Linux
For local development checks, build the local Galaxy binary and invoke it with the hidden control-mode flag:
```bash
cargo run -p galaxy --bin galaxy-oss -- --galaxyctrl instance list
```
For distributable checks, use the packaged `galaxyctrl` wrapper. The wrapper execs the packaged channel-specific Galaxy executable with `--galaxyctrl`.
The standalone `script/linux/bundle --artifact galaxyctrl` validation artifact includes that wrapper and compiles the forwarded channel binary with `galaxy_control_cli`. Release builds publish matching x86_64 and aarch64 Linux archives. Normal Linux app packages install channel-specific `galaxy-ai` and `galaxyctrl` launchers alongside the terminal launcher.
Run `galaxyctrl --version` after installation to confirm the shell is resolving the expected build.
### Windows
Until the wrapper installer lands, build the local Galaxy binary and invoke it with the hidden control-mode flag for development checks:
```powershell
cargo run -p galaxy --bin galaxy-oss -- --galaxyctrl instance list
```
Windows release archives and installer integration remain unavailable until the authenticated broker transport is implemented.
## End-to-end local test flow
Use matching app and CLI bits from the same branch or release artifact so the protocol version and action catalog agree.
1. Start Galaxy and leave at least one window open.
2. Open **Settings > Galaxy Control**, or run **Enable Galaxy Control** from the Command Palette. Local automation access is enabled by default on internal dogfood builds and disabled by default on public channels (Stable, Preview, OSS). Enabling it allows same-user scripts and agents to perform approved actions in Galaxy.
3. Confirm that the local-control server registered the running same-channel process:
   ```bash
   galaxyctrl instance list
   ```
4. Confirm app health and inspect protocol and build identity metadata:
   ```bash
   galaxyctrl app ping
   galaxyctrl app version
   galaxyctrl surface list
   ```
5. If exactly one compatible same-channel instance is listed, create a new terminal tab:
   ```bash
   galaxyctrl tab create
   ```
6. If multiple compatible same-channel instances are listed, copy the desired `instance_id` and target it explicitly:
   ```bash
   galaxyctrl app ping --instance <instance_id>
   galaxyctrl app version --instance <instance_id>
   galaxyctrl tab create --instance <instance_id>
   ```
7. Verify the running app receives focus for the selected instance and a new terminal tab appears according to Galaxy's normal new-tab placement behavior. The success response includes the created tab's opaque ID.
8. Inspect state before and after the mutation:
   ```bash
   galaxyctrl tab list --instance <instance_id>
   ```
9. To control a command in a specific visible terminal, discover the session and active block first, then use the returned IDs:
   ```bash
   galaxyctrl session list --instance <instance_id>
   galaxyctrl terminal status --instance <instance_id> --session <session_id>
   galaxyctrl terminal execute "sleep 30" --instance <instance_id> --session <session_id>
   galaxyctrl terminal status --instance <instance_id> --session <session_id>
   galaxyctrl terminal interrupt --block-id <active_block_id> --instance <instance_id> --session <session_id>
   ```
   `terminal.execute` refuses busy sessions, and `terminal.interrupt` refuses a stale block ID so a delayed request cannot stop a newer command.
Expected failures:
- `galaxyctrl instance list` with no running compatible app: exits zero with an empty list;
- a command that needs a selected app when no compatible app is running: exits non-zero with a no-instance error;
- multiple ambiguous instances: exits non-zero and asks for `--instance`;
- an explicit instance or PID from another channel: exits non-zero with a no-instance error;
- unsupported app build or stale discovery record: exits non-zero with a protocol, stale-target, or transport error;
## Security model
The local-control protocol is designed for same-user scripting, not cross-user or network access. The trust boundary is the local user account.
- **Loopback-only listener.** Each Galaxy process binds its control server to `127.0.0.1` on an ephemeral port. The listener is not reachable from the network.
- **Brokered scoped credentials.** Discovery records contain instance metadata, loopback control-endpoint information, and an instance-bound Unix-domain-socket broker reference when Galaxy Control is enabled. The broker authenticates the connecting OS user with kernel peer credentials before decoding the credential request or issuing an action-scoped credential. Records do not contain bearer tokens or reusable full-access credentials.
- **Short-lived grants.** `galaxyctrl` requests an action-scoped credential over the owner-authenticated broker socket for the selected instance, then presents that credential to `/v1/control`. Grants are instance-bound, expired entries are pruned, and the in-memory grant set is capped. Missing, invalid, expired, revoked, or wrong-instance credentials are rejected before request decoding. After decoding identifies the requested action, insufficient-scope credentials are rejected before selector resolution or handler dispatch.
- **Protected local state.** The authoritative Galaxy Control setting uses platform secure storage, never imports a value from ordinary or private preferences, and defaults to enabled only on internal dogfood channels (disabled by default on public channels). On POSIX platforms, discovery records and broker sockets use owner-only permissions. On Windows, local-control publication remains disabled until equivalent ACL and broker protections are implemented.
- **Channel-scoped discovery.** Each channel-specific CLI considers only records from its own Galaxy channel. Listing, implicit selection, and explicit instance or PID selection cannot target another channel.
- **Stale-record pruning.** On each `instance list` or implicit discovery call, same-channel records whose PID is no longer alive are deleted automatically. Candidates are also health-probed and accepted only when the live app reports the expected instance identity.
- **No CORS.** The control endpoints do not set permissive CORS headers, so browser-origin JavaScript cannot read responses even if it guesses the port. The credential requirement provides a second layer since browsers cannot read the brokered credential material.
```mermaid
sequenceDiagram
    participant CLI as galaxyctrl
    participant FS as ~/.galaxy/local-control/
    participant Broker as Credential broker
    participant HTTP as Galaxy loopback server<br/>(127.0.0.1:ephemeral)
    participant Bridge as App bridge

    CLI->>FS: Read discovery records (user-only permissions / ACL)
    FS-->>CLI: instance_id, loopback endpoint, broker socket reference
    CLI->>CLI: Prune stale PIDs, select instance
    CLI->>Broker: Connect to Unix socket<br/>exact action + selected instance
    Broker->>Broker: Authenticate peer OS user before decode;<br/>check Galaxy Control enabled + catalog action
    alt Disabled, invalid, or insufficient scope
        Broker-->>CLI: Structured denial
    else Grant allowed
        Broker-->>CLI: Short-lived scoped credential
        CLI->>HTTP: POST /v1/control<br/>Authorization: Bearer <scoped credential>
        HTTP->>HTTP: Verify credential expiry + instance binding before decode
        HTTP->>HTTP: Decode typed request; verify action scope
        HTTP->>Bridge: Dispatch action to app context
        Bridge-->>HTTP: Structured result or error
        HTTP-->>CLI: JSON response envelope
    end
```
**Known limitations and future hardening:**
- Windows local-control publication is disabled until discovery-record ACL creation and validation are implemented.
- Unexpired exact-action credentials may be reused for their granted action. Close actions flow through normal Galaxy close behavior and may trigger existing app warnings.
- Same-user malicious software can still invoke trusted wrappers or automate the desktop, so brokered credentials are least-privilege guardrails rather than a complete hostile same-user sandbox.
- Future catalog expansion should consider per-request nonces, stricter platform secure-storage constraints, and stronger approval or policy gates.
## Documentation review notes
- Keep examples scoped to the authoritative 80-action catalog and explicitly call out that close actions use normal Galaxy close behavior.
- Do not document excluded families or actions as usable just because internal app implementations exist.
- Windows packaging may initially follow the existing helper-wrapper pattern. Update this README when that decision is final.
