# Galaxy Control security architecture
`galaxyctrl` is a local-control CLI for an already-running Galaxy app instance. The design is external-only: all callers are same-user processes. There is no inside-Galaxy/outside-Galaxy distinction, no verified-terminal invocation context, and no authenticated-user identity layer.
The security architecture has five layers:
1. **Protected enablement:** A single Galaxy Control setting stored in protected local storage. It defaults to enabled on internal dogfood channels (Dev, Local) and to disabled on all other channels (Stable, Preview, OSS, Integration), where the user must opt in through Settings > Galaxy Control.
2. **Owner-only discovery:** Per-user filesystem discovery with owner-only permissions finds compatible instances without granting control authority.
3. **Same-user credential broker:** A Unix-domain socket authenticates the OS user through kernel peer credentials and issues short-lived exact-action credentials. The broker authenticates the OS user, not the calling application.
4. **Loopback HTTP transport:** An instance-local listener on `127.0.0.1` carries typed requests with broker-issued credentials.
5. **App-side enforcement:** The running Galaxy app verifies the exact granted action and resolves targets deterministically. Close actions flow through normal Galaxy close behavior so existing app warnings remain authoritative.
Exact-action credentials are safety and intent mechanisms. They let a script or agent request only the specific operation it intends to perform so authority for a harmless UI action cannot accidentally be reused for a destructive close. They are not a hard security boundary against malicious same-user software.
## Security goals
- Allow same-user processes to control a running Galaxy instance through a typed, allowlisted interface when Galaxy Control is enabled.
- Prevent unauthenticated localhost clients from invoking control actions.
- Prevent browser-origin JavaScript from becoming an ambient localhost control client.
- Prevent other OS users from controlling a Galaxy instance they do not own.
- Support multiple running Galaxy processes without a shared global port or credential.
- Separate discovery metadata from control authority.
- Require Galaxy Control to be enabled before any control requests are accepted. It defaults to enabled only on internal dogfood channels; public channels default to disabled and require explicit opt-in.
- Keep credentials out of plaintext discovery records and mint them only in memory.
- Authorize every action by its exact typed identity in the app bridge.
- Route close actions through normal Galaxy close behavior so existing app warnings for unsaved files, running processes, and shared sessions remain authoritative.
- Ensure the two input-staging commands (`input.insert`, `input.replace`) never submit the buffer. No other input actions exist.
- Restrict terminal command control to existing visible sessions, idle-only execution, and compare-and-swap interruption using the exact active block ID.
- Keep the action surface at exactly 80 allowlisted actions. The Block, Auth, Drive, and History families are entirely absent.
- Fail closed on platforms without owner-only discovery and authenticated broker transport.
- Preserve deterministic targeting so a request never silently mutates or reads the wrong target.
## Honest same-user limitations
The broker authenticates the connecting process's OS user through kernel peer credentials. It does not prove that the caller is the official `galaxyctrl` binary, Galaxy-signed code, or a human-approved invocation. When Galaxy Control is enabled, any process running as the same OS user can:
- Connect to the broker socket and request credentials for any of the 80 actions, including terminal command execution and interruption.
- Invoke `galaxyctrl` as a confused deputy.
The architecture therefore provides a **meaningful hard boundary** against:
- Other OS users.
- Browser-origin JavaScript.
- Network peers.
- Unauthenticated direct HTTP clients.
For same-user software, the protections are **intent guardrails**, not strong isolation:
- Protected enablement prevents silent activation.
- Short-lived credentials prevent ambient reuse.
- Exact-action grants prevent accidental overreach.
- Normal Galaxy close behavior preserves existing warnings for close actions.
- App-side revalidation catches stale or misused credentials.
A hostile same-user process that can automate the Galaxy UI, read local state, or invoke `galaxyctrl` is not made safe by this architecture. The value is preventing easy ambient paths (web-origin, other-user, unauthenticated localhost) and giving honest callers narrow, auditable grants.
## Threat model
### In scope
- Other local OS users attempting to control a Galaxy instance owned by the current user.
- Browser-origin JavaScript attempting to call localhost control endpoints.
- Same-user automation attempting an action without a credential for that exact action.
- Same-user processes attempting to extract plaintext credentials from local state.
- Stale discovery records from exited Galaxy processes.
- Multiple running Galaxy instances where ambiguous selection could target the wrong process.
- Malformed clients attempting unknown, unsupported, or non-allowlisted action payloads.
- Valid clients attempting actions other than the exact action granted by their credential.
- Explicit target IDs that become stale between discovery and execution.
### Out of scope
- A malicious same-user process with arbitrary filesystem and process access, except that exact-action credentials still reduce accidental over-granting.
- Kernel, hypervisor, or administrator-level compromise.
- Remote control over network transports (requires a separate security design).
## Protected enablement
Galaxy Control has a single local automation access setting:
- **Enabled** (default on internal dogfood channels): same-user processes may request exact-action credentials and send control requests.
- **Disabled** (default on Stable, Preview, OSS, and Integration channels): no credentials are issued, no control requests are accepted, discovery records contain no actionable endpoint.
The authoritative value is stored in the most secure local storage available:
- **macOS:** Keychain, constrained to Galaxy-signed code where the platform supports it.
- **Linux:** Platform secret service where available; owner-only file fallback with the weaker same-user protection explicitly documented.
The setting is:
- Local-only: never synced through Settings Sync, Galaxy Drive, or server-backed preferences.
- Private: never appears in `settings.toml`, generated schemas, or any user-editable settings surface.
- App-controlled: only the running Galaxy app through Settings > Galaxy Control or the Enable/Disable Galaxy Control Command Palette actions can change it. `galaxyctrl`, shell scripts, config files, registry edits, `defaults write`, and direct protocol requests cannot enable or change it.
- Channel-default: when no valid protected value is available, the mode defaults to enabled on internal dogfood channels (Dev, Local) and disabled on all other channels.
Disabling Galaxy Control immediately prevents new credential issuance and invalidates outstanding credentials. The control listener rejects all requests with `local_control_disabled`.
## Discovery registry
Each enabled Galaxy process writes a discovery record in a secure per-user directory.
A discovery record contains:
- Opaque `instance_id`.
- PID and process start timestamp.
- Channel and build metadata.
- Protocol version.
- Loopback endpoint for the control listener.
- Broker socket filename (inside the same owner-only directory).
A discovery record does **not** contain:
- Bearer tokens, raw credentials, or reusable control authority.
- Terminal contents, environment variables, or auth tokens.
Discovery rules:
- Records are readable only by the owning user.
- POSIX: owner-only permissions (`0600` for files, `0700` for the directory).
- When Galaxy Control is disabled, no actionable record is published.
- The CLI prunes or ignores stale records whose PID is gone or whose health check fails.
- The CLI rejects records whose HTTP endpoint is not exactly `127.0.0.1` and whose broker socket is not the expected filename inside the owner-only directory.
- If multiple compatible instances are ambiguous, the CLI requires explicit `--instance` selection.
## Credential model
### Properties
A credential encodes:
- Issuing Galaxy instance.
- The one granted `ActionKind`.
- Issued-at time and expiry time.
- Unique credential ID for revocation.
### Issuance
The broker is a Unix-domain socket inside the owner-only discovery directory. Before reading any request, the broker calls the platform peer-credential API and verifies the connecting process's UID equals Galaxy's effective UID.
Issuance flow:
1. Client connects to the broker socket.
2. Broker verifies peer UID.
3. Client requests a credential naming one exact action.
4. Broker checks Galaxy Control is enabled and the action is in the 80-action catalog.
5. Broker mints a short-lived credential in memory.
6. Client receives the credential.
Constraints:
- Credentials are never written to discovery records or persistent storage.
- There is no stored bootstrap secret or reusable token.
- The broker evaluates current Galaxy Control state at issuance time.
- Every credential is bound to exactly one action and one instance.
- Issued credentials exist only in the app's process-local credential map and the client's memory.
### Exact-action grants
Every credential grants one exact typed action. The app bridge compares the requested action to the granted action before selector resolution or handler dispatch. A credential for `tab.create` cannot authorize `tab.close`, `setting.set`, or any other action. Similar actions do not inherit authority.
This prevents accidental overreach and gives Galaxy a structured point to deny sensitive actions. It does not make untrusted same-user software safe.
### Close behavior
The three destructive actions (`window.close`, `tab.close`, `pane.close`) execute after the same exact-action credential validation as every other action. They flow through normal Galaxy close behavior so existing app warnings remain authoritative.
### Confused-deputy mitigation
The broker authenticates the OS user, not the calling application. Any same-user process can request credentials. Mitigations:
- Exact-action credentials prevent accidental action overreach.
- Short expiry limits the window for credential reuse.
- Normal Galaxy close behavior preserves existing warnings for close actions.
- The Terminal family cannot read terminal output, execute over a busy active block, or interrupt a command without the exact current block ID.
- Protected enablement prevents silent activation of the control surface.
- App-side bridge enforcement re-checks every credential on every request.
These mitigations route operations through intentional flows. They do not guarantee that arbitrary same-user software cannot cause Galaxy-visible actions.
## Transport authentication
The control listener is bound to `127.0.0.1` on an ephemeral per-process port.
Required protections:
- No permissive CORS headers on control endpoints.
- Reject any request carrying an `Origin` header.
- Reject any request whose `Host` header is not exactly `127.0.0.1:<port>`.
- Require a bearer credential present in the instance's process-local credential map.
- Reject missing, malformed, expired, or wrong-instance credentials with structured errors.
- Decode the typed request only after transport authentication.
- No JSONP or browser-readable fallback formats.
- No GET endpoints for mutating actions.
These checks defend against browser-origin clients, network clients, unauthenticated clients that discover or guess the TCP port, stale records, and wrong-instance credentials.
## App-side enforcement
The app bridge is the final enforcement point. Direct protocol clients can bypass the CLI, so enforcement must happen in the app.
The bridge:
1. Authenticates the transport credential.
2. Parses the typed request envelope.
3. Verifies protocol version compatibility.
4. Compares the requested action to the granted action. Rejects mismatches with `insufficient_permissions`.
5. Resolves targets deterministically. Rejects ambiguous, missing, or stale targets with structured errors.
6. Invokes only the allowlisted typed handler.
## Target scoping
Targeting is part of security. The protocol never converts ambiguous or stale selectors into best-effort mutations.
Rules:
- Instance selection happens before request dispatch and must be explicit when ambiguous.
- `active` selectors resolve only when the target is unambiguous. For window-scoped mutations, the resolver falls back to the sole window if exactly one exists.
- Explicit opaque IDs resolve exactly or return `stale_target`.
- Index selectors resolve to concrete IDs before execution.
- Session-scoped requests against non-terminal panes return `target_state_conflict`.
## Input staging safety
The two input commands (`input.insert`, `input.replace`) only stage text in the terminal input buffer. They never submit the buffer, press Enter, or execute a command. No other input actions (`input.get`, `input.clear`, `input.mode.set`, `input.run`) exist in the 80-action catalog. Tests must prove no submission occurs.
## Terminal command control safety
Terminal command control is a separate, explicit high-impact family:
- `terminal.status` returns only active block identity and state, elapsed running time, and a bounded command summary produced by Galaxy's secret-obfuscating command path. It does not return terminal output, environment variables, or unredacted command text.
- `terminal.execute` resolves an existing visible terminal-session target, rejects empty/NUL-containing/over-64-KiB commands, and submits only when the active block is idle and its staged input buffer is empty. It never clears or overwrites text the user has already typed, and it does not silently queue behind or append to a running command.
- `terminal.interrupt` requires the caller to echo the exact current `block_id` from `terminal.status`. The app compares it against the active block immediately before emitting ETX and rejects stale IDs, preventing a delayed request from interrupting a newer command.
- ACP's agent-safe MCP profile can provide `galaxy_terminal_interrupt_at`, which waits in the local bridge rather than relying on model polling. It repeatedly verifies the pinned target and expected block ID, exits harmlessly if that block stops or changes, and delegates the final mutation to the same exact-block `terminal.interrupt` contract.
- ACP terminal tools reject panes already identified as remote or Wormhole targets, and the agent-safe execute surface rejects recognized SSH launches rather than deliberately extending delegated terminal control across another transport boundary.
- The SSH detector covers direct, wrapped, and compound launch forms. It is a transport-boundary guard, not a general network sandbox: an otherwise authorized local command may still use other network clients or make its own network connections.

Galaxy Control enablement is therefore authorization for same-user callers to execute commands in visible terminal sessions. Public channels keep the feature disabled until the user explicitly opts in. Exact-action credentials and the checks above reduce accidental overreach; they do not make a hostile same-user process safe.

## ACP process boundary

ACP adapters are trusted local programs, not a sandboxed extension:

- Galaxy redacts known secrets from outbound ACP prompt text and image
  attachment URIs while retaining the original local transcript.
- The adapter starts with a small runtime environment allowlist plus only
  explicitly configured variables; inherited provider keys, cloud credentials,
  and proxy credentials are cleared.
- Authentication uses methods advertised by the adapter. Galaxy does not
  scrape or synthesize ChatGPT subscription tokens.
- Persisted ACP session IDs are bound to the effective adapter launch identity
  and are rejected when that identity changes.
- Broad adapter-native permission requests are denied by default. Concrete
  Galaxy terminal tools are separately gated, and command execution is not
  advertised when the configured denylist cannot be enforced exactly.
- ACP process startup is currently Unix-only. Non-Unix platforms fail before
  spawning the adapter until the pinned SDK provides full process-tree
  ownership, such as a Windows Job Object with kill-on-close behavior.

These measures reduce accidental credential and authority leakage. They do not
prevent a trusted adapter executable from reading files the current OS user can
read. Allowing arbitrary terminal execution is also not a network-egress
sandbox: a permitted local command can invoke scripts, interpreters, or other
network clients that static SSH recognition cannot prove safe.

## Catalog boundary
The catalog contains exactly 80 actions. The following families and actions are entirely absent:
- The entire Block family (`block.list`, `block.inspect`, `block.output`).
- The entire Auth family (`auth.status`, `auth.login`).
- The entire Drive family (all `drive.*` actions).
- The entire History family (`history.list`).
- `input.get`, `input.clear`, `input.mode.set`, and `input.run`.
- Terminal output reads, execution over a busy active block, interruption without an exact active block ID, and arbitrary process-kill primitives.
- `file.list` and any local file content operations beyond the `file.open` app-state intent.
- Agent-prompt submission.
- Debug, crash, heap-dump, token-copying, and developer-only helpers.
- Arbitrary internal view dispatch by string.
Adding a new action requires extending the catalog, implementing validation, adding a handler, and adding tests for credential denial and success behavior.
## Error model
Structured errors are part of the security contract:
- `local_control_disabled` — Galaxy Control is disabled.
- `unauthorized_local_client` — missing, malformed, expired, or invalid credential.
- `insufficient_permissions` — credential grants a different action.
- `ambiguous_instance` — multiple instances, no unambiguous selection.
- `ambiguous_target` — multiple matching targets.
- `stale_target` — explicit target ID no longer exists.
- `missing_target` — no active or default target exists.
- `invalid_selector` — malformed selector syntax.
- `invalid_request` — malformed request body.
- `invalid_params` — invalid action-specific parameters.
- `unsupported_action` — action not implemented by this build.
- `not_allowlisted` — action intentionally excluded from public surface.
- `target_state_conflict` — target cannot support the requested action.
- `no_instance` — no reachable Galaxy instance found.
- `protocol_version_unsupported` — client and app protocol versions do not match.
- `transport_unavailable` — the local transport (broker socket or loopback HTTP) failed.
- `bridge_unavailable` — the app-side bridge cannot service requests.
- `internal` — unexpected internal failure.
The app never downgrades these failures into broader default actions.
## Platform requirements
### macOS
- Discovery directory and records: owner-only permissions.
- Authoritative Galaxy Control value: Keychain, constrained to Galaxy-signed code.
- Broker: Unix-domain socket with peer credential checks.
### Linux
- Discovery directory and records: owner-only permissions (`0700`/`0600`).
- Authoritative Galaxy Control value: platform secret service where available; owner-only file fallback documented as weaker.
- Broker: Unix-domain socket with peer credential checks.
### Windows
Fails closed. `galaxyctrl` is not available on Windows until:
- Discovery-record ACL enforcement is implemented (current user, Administrators, SYSTEM).
- An equivalent authenticated broker transport replaces the Unix-domain socket.
- Protected Galaxy Control storage uses Credential Manager, DPAPI, or equivalent.
Until these are implemented, the `galaxyctrl` wrapper on Windows returns a structured error and does not fall back to unauthenticated control.
## Remote control is separate
The local architecture assumes same-machine, same-user control over a loopback listener. Remote control requires a separate security design with transport encryption, remote identity, replay protection, explicit approval, and network exposure review. Remote support must not be enabled by pointing local credentials at an arbitrary URL.
## Auditing
Recommended audit fields for control requests:
- Timestamp.
- Instance ID.
- Credential ID.
- Action name.
- Target type and opaque target ID.
- Success or structured error code.
Avoid logging: bearer credentials, terminal output, command text, input buffer contents, or environment variable values. Error-level logs should be used only for conditions needing developer attention, not normal denied requests or user-caused selector failures.
## Required controls before catalog expansion
Before shipping each action family:
- Galaxy Control must be enabled for any request to succeed.
- The action has a documented entry in the 80-action catalog.
- The bridge verifies the credential grants that exact action.
- Ambiguous, missing, and stale targets return structured errors.
- Close actions flow through normal Galaxy close behavior.
- Input actions never submit the buffer.
- Terminal execution remains idle-only, and terminal interruption remains guarded by the exact active block ID.
- Tests cover the allowed path and the wrong-action-credential denial path.
- Logs and errors do not expose credentials, terminal contents, or sensitive settings.
- The Block, Auth, Drive, and History families remain absent from the catalog.
- The catalog contains exactly 80 default-authorized actions.
