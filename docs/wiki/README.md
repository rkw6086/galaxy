# Galaxy documentation sources

These are the versioned sources for the [Galaxy GitHub wiki](https://github.com/rkw6086/galaxy/wiki).
They describe the inspected implementation and distinguish it from features that still need
runtime verification. The audit records its exact source revision.

## Guides

- [Home](Home.md)
- [Installation and updates](Installation-and-Updates.md)
- [Terminal and workspace](Terminal-and-Workspace.md)
- [Providers and models](Providers-and-Models.md)
- [Profiles, permissions, and tools](Profiles-Permissions-and-Tools.md)
- [Data and privacy](Data-and-Privacy.md)
- [Troubleshooting](Troubleshooting.md)
- [Technical overview](Technical-Overview.md)
- [Building from source](Building-from-Source.md)
- [Warp documentation reference audit](Warp-Documentation-Reference-Audit.md)
- [Detailed help and support references](Warp-Reference-Appendix.md)

## Audit data

The [audit-data](audit-data) directory contains the complete JSON/CSV inventory, source
locations, central-link consumers, and domain mentions. The readable audit distinguishes
runtime candidates from tests, historical records, hosted code, and compatibility identifiers.
Static classification does not prove that a particular screen is reachable in a given build.

## Maintaining the wiki

Edit these sources through the primary Gitea repository so changes remain reviewable and
reach the public mirror. The wiki is a separate Git repository; ordinary source pushes do
not publish it automatically. Publish the reviewed Markdown pages and `_Sidebar.md` to
`https://github.com/rkw6086/galaxy.wiki.git`, preserving its history. This source index and
the audit data stay in the main repository; the wiki links to the full inventory here.

An empty wiki must first receive its initial page through the GitHub website before it can
be cloned or updated through Git. See [GitHub's wiki editing instructions](https://docs.github.com/en/communities/documenting-your-project-with-wikis/adding-or-editing-wiki-pages).

Do not turn the audit into a blanket domain replacement. Service endpoints, legal links,
stored identifiers, and upstream provenance need individual review. User-facing help links
should move only after their Galaxy destination is published and the feature is supported.
