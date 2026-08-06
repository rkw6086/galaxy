# Galaxy

Galaxy is a local-first developer terminal with an agent that works alongside your shell. It is
designed to keep your conversations, settings, and personal workspace data on your machine while
letting you explicitly connect the model providers and local agents you choose.

## What Galaxy provides

- A fast terminal, editor, and local workspace for development.
- Agent conversations with Galaxy-owned permissions for shell, file, and MCP actions.
- Provider connections for ChatGPT subscriptions, OpenAI-compatible APIs, Anthropic, Gemini,
  Vertex AI, AWS Bedrock, and ACP agent runtimes.
- Local Galaxy Drive content, including rules, profiles, notebooks, workflows, and MCP settings.
- Explicit network boundaries: model traffic goes only to configured providers, and network tools
  remain opt-in.

## Building from source

Galaxy is a Rust workspace. Platform setup and the common development tools are installed with:

```bash
./script/bootstrap
```

Then build or run the client with:

```bash
cargo run
```

Before submitting changes, run the repository checks:

```bash
./script/format
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
cargo test
```

See [AGENTS.md](AGENTS.md) for architecture notes, platform setup, coding conventions, and the
focused commands used by the project.

## Contributing

Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a change. Bug reports should include
reproduction steps, expected and actual behavior, platform details, and relevant logs or
screenshots that do not contain secrets. Security issues must be reported privately according to
[SECURITY.md](SECURITY.md).

UI changes should include manual verification, and visual changes should include before-and-after
screenshots when practical. Changes to local persistence, provider boundaries, or agent execution
should include focused automated coverage.

## Licensing

The Galaxy application and most workspace crates are licensed under
[AGPL-3.0-only](LICENSE-AGPL). The GalaxyUI crates are licensed under the
[MIT License](LICENSE-MIT). Individual third-party components retain the licenses required by
their respective notices and source files.

## Project direction

The migration plan in [plans/galaxy-local-first-rig.md](plans/galaxy-local-first-rig.md) records the
local-first architecture, provider boundary, and remaining work. Contributions should preserve
those boundaries: no inherited Warp service is required for a fresh install, and no provider or
agent may bypass Galaxy's permission and egress policy.
