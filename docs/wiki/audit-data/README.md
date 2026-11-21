# Warp website reference inventory

This inventory records Warp-owned website URLs and public support/security/feedback contacts in tracked Galaxy text, plus upstream Warp GitHub issue/security references. It inventories links and source call sites; it does not reproduce upstream documentation content.

## Reproduce the frozen baseline

Requirements: Git and Python 3 (standard library only). Run the portable collector from any directory:

```bash
python3 collect.py \
  --repo /path/to/galaxy \
  --ref 600d7970c55e6f388ea61ab6d9f95fc8e33cd2ca \
  --output /tmp/galaxy-warp-reference-audit
```

The reference is resolved once to a commit. Searches use `git grep COMMIT` and file reads use `git show COMMIT:path`. The live worktree, ignored files, and untracked files are not input. A dirty working tree therefore does not change the audit for a fixed commit.

The following generated paths are explicitly excluded to prevent a future audit from counting its own wiki tables and copied source links:

- `docs/wiki/**`
- `docs/generated/warp-reference-audit/**`
- `warp-reference-audit/**`

If the output or a generated audit copy is committed at a different location, add that location to `EXCLUDED_PATHS` before auditing the newer commit. Running against a newer source is an updated audit, not a reproduction of this baseline.

## Baseline counts

At source `600d7970c55e6f388ea61ab6d9f95fc8e33cd2ca`:

- 531 literal URL/contact occurrences; 361 distinct references across 142 tracked files.
- The `docs.warp.dev` subset contains 115 URL occurrences and 83 unique URLs.
- A focused runtime/UI/config-template/public-repository slice contains 156 occurrences, 109 distinct references, across 57 files.
- The focused slice is 91 runtime/message candidates, 50 retained hosted/gated UI references, 13 public repository documentation/contact references, and two user-visible configuration templates.
- Ten central constant symbol references are recorded separately. These include imports as well as uses and must not be described as ten independently reachable UI actions.
- 731 tracked lines mention the searched domains or upstream repository. This includes bare-domain mentions that are not complete URLs. For example, three old `REVIEW.md` table rows mention `docs.warp.dev` without an `https://` URL.

The portable frozen-snapshot run was compared with the initial tracked-worktree baseline: every source path, line, literal reference, source context, domain-mention record, and indirect-constant record matched. Proposed destinations are editorial guidance refined during review, not additional source facts.

## Output files

| File | Purpose |
| --- | --- |
| `summary.json` | Source commit, exclusions, scope and aggregate counts |
| `inventory.json` | Complete literal-reference records, category/topic, proposed Galaxy destination and immutable source link |
| `inventory.csv` | Compact tabular form of the full inventory |
| `domain-mentions.json` | All matching tracked source lines, including bare domains and historical references |
| `indirect-central-links.json` | Consumers/imports of central docs/issues/Slack/privacy constants |
| `reference-appendix.md` | Complete Markdown appendix grouped by topic and URL/contact |
| `runtime-and-public-docs.json` | Focused publication/review subset |
| `runtime-and-public-docs-appendix.md` | Focused writer-ready tables with exact source links |

## Interpretation limits

Categories are static review aids. “Runtime candidate” means the reference is in runtime UI/message code; it is not proof that a given platform, flag, provider, build or current screen exposes it. Hosted/gated links must not be advertised as supported Galaxy services. Historical design documents, old workflows, regression fixtures and author metadata are kept distinct from active help.

Do not replace every Warp string. Download/service addresses, serialized names, provider protocol identifiers, compatibility literals and third-party authorship may need to remain stable. Legal/privacy policy links and private support/security contacts need project-owner decisions rather than an invented email address or blanket wiki substitution.

No app launch, build, container operation, website fetch or external publication is performed by this collector.
