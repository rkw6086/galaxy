# Contributing to Galaxy

Thanks for helping improve Galaxy. This guide describes the local-first workflow for reporting
issues, developing changes, and preparing a reviewable contribution.

## Before you start

- Read [AGENTS.md](AGENTS.md) for the architecture, development commands, and coding conventions.
- Search the repository issue tracker before opening a duplicate report.
- For a bug, include reproduction steps, expected and actual behavior, platform details, and logs
  or screenshots that do not contain secrets.
- For a larger feature, describe the user problem and agree on the shape of the change before
  implementing it. Keep product and technical plans under `plans/` or `specs/` as appropriate.
- Report security vulnerabilities privately using [SECURITY.md](SECURITY.md). Do not publish
  exploitable details in an issue or pull request.

## Development setup

```bash
./script/bootstrap
cargo run
```

The project is a Rust workspace with the main client in `app/`, GalaxyUI in `crates/galaxyui/`,
and shared libraries under `crates/`. The default build must work without a Galaxy account or a
remote service.

## Making changes

Keep changes focused and preserve the local-first boundaries:

- Model traffic may use only a provider explicitly configured by the user.
- Galaxy owns permission prompts and execution for shell, file, and MCP actions.
- Local content should use the local repository instead of cloud identity or sync services.
- New UI should reuse existing GalaxyUI components, theme tokens, and button themes.
- Never log credentials, prompts, command output, or provider responses unless a diagnostic mode
  explicitly documents that behavior.

For user-facing changes, verify the complete flow manually. Include screenshots for small visual
changes and a short recording for larger interactive flows when practical. For persistence,
provider, or agent-runtime changes, add restart, failure, cancellation, or boundary coverage as
appropriate.

## Checks

Run the focused checks for the code you touched, then run the required formatting and lint checks:

```bash
./script/format
cargo clippy --workspace --all-targets --all-features --tests -- -D warnings
cargo test
```

Useful narrower commands include:

```bash
cargo check -p galaxy
cargo test -p galaxy --lib
cargo test -p galaxy_agent_core
cargo test -p galaxy_agent_rig
```

If a check cannot run locally, explain why in the change description and include the closest
available validation.

## Pull requests

Use a branch named for the change, keep commits focused, and include:

1. A concise summary of the user-visible behavior.
2. The design or architectural boundary affected.
3. Automated checks and manual verification performed.
4. Screenshots or recordings for visual and interactive changes.
5. Any follow-up work added to the relevant plan or noticed-bugs list.

Reviewers should be able to build the branch from a clean checkout and understand why the change
belongs in Galaxy. Do not include unrelated formatting or generated-file churn.

## Code of conduct

Galaxy follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Please keep issue, review, and
community discussions respectful, constructive, and focused on the work.

## License

The Galaxy application and most workspace crates are licensed under
[AGPL-3.0-only](LICENSE-AGPL). The GalaxyUI crates are licensed under the
[MIT License](LICENSE-MIT). Contributions are accepted under the license applicable to the code
they modify. Review the existing file headers and third-party notices before copying code into a
different crate.
