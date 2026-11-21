# Warp documentation reference audit

This is a source audit of inherited Warp website, help/documentation, community, support, and upstream issue/security references. It is also the migration map for replacing user-facing help with Galaxy documentation. It does not copy Warp's documentation or claim that a Warp-hosted feature is available in Galaxy.

Audit baseline: [source `600d7970c55e6f388ea61ab6d9f95fc8e33cd2ca`](https://github.com/rkw6086/galaxy/commit/600d7970c55e6f388ea61ab6d9f95fc8e33cd2ca).

## Scope and results

All tracked text was scanned. Literal Warp-owned website/contact references and upstream Warp issue/security URLs were recorded; domain mentions and indirect central-constant symbol references (including imports) were recorded separately. Dynamic documentation URLs were recorded as their construction callsites/formats, not as every path a model might produce. No external documentation page was fetched or copied. Static classifications identify where a value occurs, not proof that a particular platform, feature gate, or screen exposes it at runtime.

| Measure | Result |
| --- | ---: |
| Literal website/contact/upstream issue references | 531 occurrences |
| Distinct literal references | 361 |
| Tracked files containing those references | 142 |
| `docs.warp.dev` references | 115 occurrences / 83 distinct URLs |
| Focused help/support/public-documentation slice | 156 occurrences / 109 distinct references / 57 files |
| Indirect central-link symbol references (including imports) | 10 |

The 531 total is deliberately broader than live help links: it includes tests, source comments, skills, historical records, packaging templates, and endpoint/compatibility values. They must not all be presented as live Galaxy help buttons.

The focused slice contains 91 runtime UI/message candidates, 50 retained hosted/gated UI occurrences, 13 public repository prose/contact occurrences, and two generated user-configuration template occurrences. The [callsite inventory](https://github.com/rkw6086/galaxy/wiki/Warp-Reference-Appendix) lists their URLs and exact source locations. Complete machine-readable files are retained with the [versioned documentation audit data](https://github.com/rkw6086/galaxy/tree/master/docs/wiki/audit-data).

## Highest-impact findings

| Surface | Reviewed source | Finding / migration target |
| --- | --- | --- |
| Help menu documentation | `app/src/app_menus.rs:934`, `app/src/util/links.rs:3` | “Galaxy Documentation” resolves to Warp docs. Target this wiki's [Home](https://github.com/rkw6086/galaxy/wiki/Home). |
| Help menu issues / feedback | `app/src/util/links.rs:5,10` | Routes to the upstream Warp issue repository/form. Ordinary Galaxy bugs belong in [Galaxy Issues](https://github.com/rkw6086/galaxy/issues). |
| Community item | `app/src/util/links.rs:6`, `app/src/app_menus.rs:936` | A Galaxy-labeled Slack item still uses the upstream preview/community link. Do not claim a Galaxy Slack exists without an owner-provided destination. |
| Model documentation citations | `app/src/terminal/view.rs:20560` | `WarpDocumentation` paths form `https://docs.warp.dev/{path}`. This needs a topic mapping and correct Galaxy content, not only a hostname substitution. |
| Provider/profile/MCP tips | `app/src/ai/agent_tips.rs`, `app/src/settings_view/ai_page.rs`, `app/src/settings_view/mcp_servers/list_page.rs` | Help points to upstream capability pages. Use the Galaxy [provider](https://github.com/rkw6086/galaxy/wiki/Providers-and-Models) and [permissions/tools](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools) guides. |
| Terminal/setup/error help | `app/src/terminal/view.rs`, `app/src/terminal/view/shell_terminated_banner.rs`, `app/src/settings_view/features_page.rs` | Replace with verified Galaxy terminal/settings/troubleshooting content and preserve relevant anchors. |
| Generated launch-config comments | `app/src/user_config/mod.rs:35`, `app/src/launch_configs/save_modal.rs:535` | New user files can inherit an upstream docs link. Review generated comments as well as clickable UI. |
| Public FAQ/security policy | `FAQ.md:137–144`, `SECURITY.md:11–12` | Still directs users to Warp help/security contacts. Galaxy private vulnerability reporting is not enabled; a verified private channel needs a maintainer decision. |
| Privacy / terms / support contacts | Various auth, onboarding, billing and repository files | These require project-owner policy/contact review. A wiki page must not silently substitute a new legal policy or invent a support address. |

## Topic mapping

| Inherited topic | Galaxy documentation target |
| --- | --- |
| General help, keyboard/terminal orientation | [Home](https://github.com/rkw6086/galaxy/wiki/Home), [Troubleshooting](https://github.com/rkw6086/galaxy/wiki/Troubleshooting) |
| Downloads and application updates | [Installation and updates](https://github.com/rkw6086/galaxy/wiki/Installation-and-Updates) |
| Models, provider discovery, context limits | [Providers and models](https://github.com/rkw6086/galaxy/wiki/Providers-and-Models) |
| Agent permissions, rules, MCP, plans, jobs | [Profiles, permissions, and tools](https://github.com/rkw6086/galaxy/wiki/Profiles-Permissions-and-Tools) |
| Local files, diagnostics, credential handling | [Data and privacy](https://github.com/rkw6086/galaxy/wiki/Data-and-Privacy) |
| Runtime architecture and source navigation | [Technical overview](https://github.com/rkw6086/galaxy/wiki/Technical-Overview) |
| Compiling and packaging | [Building from source](https://github.com/rkw6086/galaxy/wiki/Building-from-Source) |
| Upstream changelog | [Galaxy Releases](https://github.com/rkw6086/galaxy/releases) and [Galaxy CHANGELOG](https://github.com/rkw6086/galaxy/blob/master/CHANGELOG.md) |
| Hosted environments, cloud triggers, hosted billing/sharing | [Known limitations](https://github.com/rkw6086/galaxy/wiki/Troubleshooting#docker-and-hosted-environments); do not advertise an unsupported Galaxy service |
| Ordinary bug reports | [Galaxy Issues](https://github.com/rkw6086/galaxy/issues) |
| Security reports, legal terms, community contacts | Maintainer-owned decisions; do not replace with an unrelated page or public issue form |

## What should not be blindly renamed

- Third-party product/package names, protocol identifiers, serialized enum/settings names, migration filenames, and upstream attribution can be compatibility requirements.
- A test fixture or historical upstream record is different from a user-facing help destination.
- Packaging templates can contain upstream URLs that Galaxy's build scripts replace. Inspect the generated artifact before concluding that an installed Galaxy package contacts the source-template URL.
- Provider authentication and service/update endpoints are not documentation links. Changing their domain can break authentication, installation, or protocol behavior rather than fix help text.
- Inherited hosted UI should be removed, disabled, or implemented as deliberate Galaxy functionality; relabeling its documentation does not make the hosted service exist.

## Completion criteria for the later link migration

1. Establish verified Galaxy support, private security, legal/privacy, and community destinations where needed.
2. Replace central documentation/issues constants and generated-file help links with reviewed Galaxy targets.
3. Map topic-specific links and anchors to real wiki sections; keep unsupported hosted features clearly unavailable.
4. Check menu items, settings help, error banners, tips, generated configs, repository prose, and packaged metadata independently.
5. Verify a native build and exercise the changed user-facing links on the supported desktop platforms. A static URL search alone is not UI verification.
6. Regenerate the inventory, recording which remaining references are intentional compatibility, tests, attribution, or history.

This audit and the initial wiki are documentation work. The application links are not claimed to have been migrated by publishing these pages.
