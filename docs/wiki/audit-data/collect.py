from pathlib import Path
import subprocess, re, json, csv, collections
from urllib.parse import urlsplit
import argparse
parser=argparse.ArgumentParser(description='Inventory Warp website/help references from a frozen tracked Git commit; never read the live worktree.')
parser.add_argument('--repo', required=True, type=Path, help='Git checkout or repository path')
parser.add_argument('--ref', required=True, help='Commit, branch or tag resolved once to a commit')
parser.add_argument('--output', required=True, type=Path, help='Output directory for JSON/CSV/Markdown artifacts')
args=parser.parse_args()
ROOT=args.repo.resolve()
OUT=args.output.resolve()
OUT.mkdir(parents=True,exist_ok=True)
SHA=subprocess.check_output(['git','rev-parse','--verify','--end-of-options',args.ref+'^{commit}'],cwd=ROOT,text=True).strip()
# Generated wiki/audit copies are deliberately excluded so a later audit does not count itself.
EXCLUDED_PATHS=['docs/wiki/**','docs/generated/warp-reference-audit/**','warp-reference-audit/**']
pat=r'warp[.]dev|warp[.]com|warp[.]ai|warpcommunity[.]slack[.]com|github[.]com/warpdotdev/[Ww]arp'
def snapshot_files(pattern,path='.'):
    command=['git','grep','-I','-l','-z','-i','-E',pattern,SHA,'--',path]
    command += [':(exclude)'+name for name in EXCLUDED_PATHS]
    found=subprocess.run(command,cwd=ROOT,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False)
    if found.returncode not in (0,1):
        raise RuntimeError('Git snapshot search failed.')
    prefix=(SHA+':').encode()
    return [(value[len(prefix):] if value.startswith(prefix) else value).decode() for value in found.stdout.split(b'\0') if value]
blob_cache={}
def read_snapshot(file):
    if file not in blob_cache:
        blob_cache[file]=subprocess.check_output(['git','show',SHA+':'+file],cwd=ROOT)
    return blob_cache[file]
files=snapshot_files(pat)
url_re=re.compile(r'(?:https?://|mailto:)[^\s\"\'`<>\)\]\\]+',re.I)
mention_re=re.compile(pat,re.I)
email_re=re.compile(r'(?:support|security|feedback|warp-coc)@warp\.dev|warp-coc at warp\.dev',re.I)
refs=[]; mentions=[]
HOSTED_PATHS=('app/src/ai/agent_management/cloud_setup','app/src/terminal/view/ambient_agent/','app/src/billing/','app/src/auth/','app/src/settings_view/platform','app/src/settings_view/admin_actions.rs','app/src/settings_view/teams_page.rs','app/src/terminal/enable_auto_reload_modal.rs','app/src/ai/auth_secret_types.rs','app/src/workspace/view/free_tier_limit_hit_modal.rs','app/src/workspace/view/build_plan_migration_modal.rs','app/src/workspace/view/openwarp_launch_modal/','app/src/workspace/view/launch_modal/oz_launch.rs','app/src/workspace/view/orchestration_launch_modal/','app/src/workspace/hoa_onboarding/','app/src/wasm_nux_dialog.rs')
def topic(url):
    s=url.lower()
    if 'docs.warp.dev' in s:
        path=urlsplit(url).path
        if '/agent-platform/cloud-agents' in s or '/reference/cli/' in s or '/reference/api' in s: return 'Hosted agents and hosted CLI'
        if 'troubleshoot' in s or 'known-issues' in s or 'supported-shells' in s or '/help/' in s or 'updating-warp' in s: return 'Troubleshooting, shells and updates'
        if 'plans-and-billing' in s or 'credits' in s or 'billing' in s: return 'Hosted billing'
        if 'privacy' in s or 'security' in s: return 'Privacy and security'
        if 'warp-drive' in s or 'yaml-workflows' in s: return 'Local Drive and workflows'
        if '/changelog' in s: return 'Changelog'
        if '/agent-platform/' in s or '/code/' in s: return 'Agents, providers and editor'
        if 'launch-configuration' in s or 'tab-config' in s: return 'Launch and tab configuration'
        if path in ('','/'): return 'General documentation'
        return 'Terminal and appearance'
    if 'github.com/warpdotdev/warp' in s and ('/issues' in s or '/security/' in s): return 'Upstream issue/security reporting'
    if 'slack' in s or 'go.warp.dev' in s: return 'Upstream community'
    if s.startswith('mailto:') or '@warp.dev' in s or ' at warp.dev' in s: return 'Upstream support/contact'
    if '/privacy' in s or 'terms-of-service' in s: return 'Legal and privacy'
    if 'pricing' in s or 'contact-sales' in s: return 'Hosted billing'
    if 'oz.warp.dev' in s: return 'Hosted agents and hosted CLI'
    if 'app.warp.dev' in s or 'staging.warp.dev' in s or 'releases.warp.dev' in s or 'rtc.' in s: return 'Service/update endpoint or compatibility identifier'
    if 'blog' in s: return 'Upstream blog'
    return 'Upstream website or source reference'
def classify(file,line,url,lineno):
    test=bool(re.search(r'(^|/)(tests?|integration_testing)(/|$)|(_tests?|tests)\.(rs|[cm]?[jt]sx?)$',file))
    if test: return 'test_or_test_documentation','Test fixture, regression reference or integration-only code.'
    if file.startswith('.github/upstream-workflows/') or file.startswith('specs/') or file in ('REVIEW.md','plans/galaxy-refactor.md'):
        return 'historical_or_upstream_record','Retained upstream workflow, design or historical review; not normal Galaxy help.'
    if file in ('FAQ.md','SECURITY.md','CODE_OF_CONDUCT.md'):
        return 'repository_documentation_or_contact','Public repository document still links to upstream support/community/security.'
    if file.startswith(('.agents/','.claude/','.github/skills/')) or file.startswith('AGENTS'):
        return 'maintainer_skill_or_instruction','Maintainer instruction or inherited skill reference.'
    if file.endswith(('Cargo.toml','Cargo.lock','package.json','yarn.lock','package-lock.json','pnpm-lock.yaml')) or file.startswith(('resources/linux/','script/','.github/actions/')) or file in ('flake.nix',):
        return 'packaging_or_infrastructure','Packaging metadata, build scripts, endpoint or author identity; not a help link.'
    if url and (topic(url)=='Service/update endpoint or compatibility identifier' or file=='crates/galaxyui_extras/src/secure_storage/linux.rs'):
        return 'endpoint_or_compatibility_identifier','Service address, download source or stable compatibility literal; do not mass relabel.'
    if line.lstrip().startswith(('//','/*','*','#','///','//!')):
        # user_config's raw-string template has a # comment that ships to users.
        if file=='app/src/user_config/mod.rs': return 'user_visible_configuration_template','Link is written into a generated user configuration template.'
        if file=='app/resources/tab_configs/new_tab_config_template.toml': return 'user_visible_configuration_template','Link ships in a tab-configuration template.'
        return 'source_comment_or_developer_reference','Comment/example/research context; preserve historical provenance where relevant.'
    if file.startswith(HOSTED_PATHS):
        return 'retained_hosted_or_gated_ui','Hosted/upstream auth, billing, platform or cloud-agent surface; unsupported/unreachable states require explicit treatment.'
    if file.startswith(('app/src/','crates/onboarding/','crates/galaxyui/','crates/galaxy_cli/')):
        return 'runtime_ui_or_message_candidate','Runtime UI, help action, diagnostic or CLI text. Static reference; individual visibility may depend on platform/flags.'
    if file.endswith(('.md','.rst','.txt')): return 'repository_or_developer_documentation','Repository prose/documentation reference.'
    return 'other_tracked_reference','Tracked reference requiring contextual review.'
def replacement(t,reference=None,category=None):
    value=(reference or '').lower()
    if 'security@warp.dev' in value or '/security/advisories' in value or 'warp-coc' in value:
        return 'Maintainer-approved private Galaxy reporting route; currently unpublished/disabled. Never a public issue or unverified Warp contact.'
    if 'support@warp.dev' in value or 'feedback@warp.dev' in value:
        return 'Maintainer-approved Galaxy support contact; only sanitized routine non-sensitive bugs belong in Galaxy GitHub Issues.'
    if re.search(r'github\.com/warpdotdev/warp/issues/[0-9]+',value):
        return 'Preserve the numbered upstream issue as historical/technical context; do not redirect its issue ID into Galaxy.'
    return {'General documentation':'Home','Troubleshooting, shells and updates':'Troubleshooting','Agents, providers and editor':'Configuration-and-Providers / Technical-Overview','Launch and tab configuration':'Getting-Started / Configuration-and-Providers','Terminal and appearance':'Getting-Started','Local Drive and workflows':'Configuration-and-Providers','Changelog':'GitHub Releases / CHANGELOG.md','Upstream issue/security reporting':'Galaxy GitHub Issues for routine bugs only; private/security channels need maintainer approval, and numbered upstream history stays intact','Upstream community':'Galaxy GitHub Issues; do not claim a Galaxy Slack exists','Upstream support/contact':'Maintainer-approved Galaxy contact; private/security/CoC reports never belong in public issues','Hosted agents and hosted CLI':'Known-Limitations; do not advertise inherited hosted features','Hosted billing':'Known-Limitations; remove or hide unsupported hosted billing UI','Legal and privacy':'Galaxy licensing and privacy policy review; not a blind wiki substitution','Privacy and security':'Galaxy privacy and security documentation'} .get(t,'Retain if required by upstream compatibility/integration; contextual review')
for file in files:
    data=read_snapshot(file)
    if b'\0' in data: continue
    for n,line in enumerate(data.decode('utf8',errors='replace').splitlines(),1):
        if not mention_re.search(line): continue
        source=f'https://github.com/rkw6086/galaxy/blob/{SHA}/{file}#L{n}'
        mentions.append({'path':file,'line':n,'context':line.strip()[:600],'source_url':source})
        values=[]
        for m in url_re.finditer(line):
            value=m.group().rstrip('.,;:')
            host=urlsplit(value).hostname or ''
            if host.lower().endswith(('warp.dev','warp.com','warp.ai','warpcommunity.slack.com')) or re.search(r'^https?://github\.com/warpdotdev/warp(?:/|$)',value,re.I):
                values.append(value)
        values += [m.group() for m in email_re.finditer(line) if 'mailto:'+m.group() not in values and not any(m.group() in v for v in values)]
        for value in values:
            category,reason=classify(file,line,value,n)
            t=topic(value)
            refs.append({'path':file,'line':n,'reference':value,'topic':t,'category':category,'classification_reason':reason,'proposed_galaxy_destination':replacement(t,value,category),'source_url':source,'context':line.strip()[:600]})
# Resolve centrally defined links at call sites (no literal URL on those lines).
constants={'USER_DOCS_URL':'https://docs.warp.dev/','GITHUB_ISSUES_URL':'https://github.com/warpdotdev/Warp/issues','SLACK_URL':'http://go.warp.dev/join-preview','PRIVACY_POLICY_URL':'https://www.warp.dev/privacy'}
indirect=[]
for file in snapshot_files('|'.join(constants),'app/src'):
    data=read_snapshot(file)
    if b'\0' in data: continue
    for n,line in enumerate(data.decode('utf8',errors='replace').splitlines(),1):
        for symbol,url in constants.items():
            if re.search(r'\b'+symbol+r'\b',line) and not re.search(r'pub const '+symbol+r'\b',line):
                indirect.append({'path':file,'line':n,'symbol':symbol,'resolved_reference':url,'source_url':f'https://github.com/rkw6086/galaxy/blob/{SHA}/{file}#L{n}','context':line.strip()[:600]})
summary={'source_commit':SHA,'excluded_generated_paths':EXCLUDED_PATHS,'scope':'All tracked text at the resolved Git commit (generated wiki/audit paths excluded), Warp-owned website URLs and support/security/feedback contacts, plus upstream Warp issue/security URLs. Domain mentions and indirect central constants recorded separately. No external links fetched; static visibility classifications are not a runtime reachability claim.','literal_occurrences':len(refs),'unique_literal_references':len({r['reference'] for r in refs}),'files_with_literal_references':len({r['path'] for r in refs}),'domain_mention_lines':len(mentions),'indirect_central_constant_uses':len(indirect),'by_category':dict(collections.Counter(r['category'] for r in refs)),'by_topic':dict(collections.Counter(r['topic'] for r in refs)),'docs_warp_dev_occurrences':sum('docs.warp.dev' in r['reference'] for r in refs),'docs_warp_dev_unique_urls':len({r['reference'] for r in refs if 'docs.warp.dev' in r['reference']})}
for name,value in [('inventory.json',{'summary':summary,'references':refs}),('domain-mentions.json',mentions),('indirect-central-links.json',indirect),('summary.json',summary)]:
    (OUT/name).write_text(json.dumps(value,indent=2,ensure_ascii=False)+'\n')
with (OUT/'inventory.csv').open('w',newline='') as f:
    fields=['path','line','reference','topic','category','proposed_galaxy_destination','source_url']
    writer=csv.DictWriter(f,fieldnames=fields,extrasaction='ignore',lineterminator='\n');writer.writeheader();writer.writerows(refs)
# Full docs/support topic appendix grouped by literal reference, avoiding repeated prose.
rows=['# Warp website reference inventory','',f'Audited source: `{SHA}`. This is a static source inventory, not an endorsement of Warp-hosted services or a runtime test. No upstream documentation text was copied.','',f"Found **{summary['literal_occurrences']}** literal link/contact occurrences in **{summary['files_with_literal_references']}** tracked files, **{summary['unique_literal_references']}** unique literal references. The `docs.warp.dev` subset contains **{summary['docs_warp_dev_occurrences']}** occurrences and **{summary['docs_warp_dev_unique_urls']}** unique URLs. Domain mentions and indirect constant consumers are recorded separately.",'','## Categories','', '| Category | Occurrences |','| --- | ---: |']
rows += [f'| {k} | {v} |' for k,v in sorted(summary['by_category'].items())]
rows += ['','## URL and contact appendix','']
for t in sorted({r['topic'] for r in refs}):
    rows += [f'### {t}','',f'Suggested Galaxy destination: {replacement(t)}.','']
    grouped=collections.defaultdict(list)
    for r in refs:
        if r['topic']==t: grouped[r['reference']].append(r)
    for url, occurrences in sorted(grouped.items()):
        rows += [f'- `{url}`',f"  - Suggested treatment: {occurrences[0]['proposed_galaxy_destination']}"]
        rows += ['  - '+f"[{r['path']}:{r['line']}]({r['source_url']}) — {r['category']}" for r in occurrences]
    rows += ['']
rows += ['## Indirect central link consumers','']
for r in indirect:
    rows.append(f"- `{r['symbol']}` → `{r['resolved_reference']}`: [{r['path']}:{r['line']}]({r['source_url']})")
(OUT/'reference-appendix.md').write_text('\n'.join(rows).rstrip()+'\n')
# Focused publication appendix: retain runtime candidates, config templates,
# hosted/gated UI and public repository contact documents, not old tests/specs.
focused_categories={'runtime_ui_or_message_candidate','user_visible_configuration_template','retained_hosted_or_gated_ui','repository_documentation_or_contact'}
focused=[r for r in refs if r['category'] in focused_categories]
focused_summary={'source_commit':SHA,'scope':'Literal runtime/UI/config-template/public-repository help, hosted UI, legal/privacy and contact references; static candidates, not a visibility test. Full inventory remains authoritative for excluded tests/history/infrastructure.','occurrences':len(focused),'unique_references':len(set(r['reference'] for r in focused)),'files':len(set(r['path'] for r in focused)),'by_category':dict(collections.Counter(r['category'] for r in focused)),'references':focused}
(OUT/'runtime-and-public-docs.json').write_text(json.dumps(focused_summary,indent=2)+'\n')
focused_rows=['# Runtime and public documentation reference appendix','',f"Audited source `{SHA}`. **{len(focused)} literal references**, **{len(set(r['reference'] for r in focused))} distinct links/contacts**, across **{len(set(r['path'] for r in focused))} files**. {len(indirect)} indirect central-constant symbol references are recorded separately in `indirect-central-links.json`.",'','“Runtime candidate” means the reference is used by code, not that every platform/feature exposes it. Retained hosted UI is not a supported Galaxy cloud service. Legal/privacy/support contacts need project-owner review, not blind substitution.','']
for category in sorted(set(r['category'] for r in focused)):
    focused_rows += [f'## {category}','']
    grouped=collections.defaultdict(list)
    for reference in focused:
        if reference['category']==category: grouped[reference['reference']].append(reference)
    focused_rows += ['| Existing reference | Topic / proposed Galaxy destination | Source call sites |','| --- | --- | --- |']
    for url,occurrences in sorted(grouped.items()):
        callsites='<br>'.join(f"[{r['path']}:{r['line']}]({r['source_url']})" for r in occurrences)
        focused_rows.append(f"| `{url.replace('|','%7C')}` | {occurrences[0]['topic']} / {occurrences[0]['proposed_galaxy_destination']} | {callsites} |")
    focused_rows += ['']
(OUT/'runtime-and-public-docs-appendix.md').write_text('\n'.join(focused_rows).rstrip()+'\n')
print(json.dumps(summary,indent=2))
