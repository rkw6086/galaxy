#!/usr/bin/env python3
"""Reconcile Galaxy branches without checking out code or rewriting history.

Gitea owns release tags. Rolling beta and its staging tags remain on Gitea.
Clean divergent histories receive a merge commit, with Gitea as its first parent,
published to Gitea before GitHub. Conflicts and unrelated histories are preserved
for maintainer review. This is deliberately not `git push --mirror`: deletions
are not propagated, and ordinary non-force pushes protect concurrent work.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

REMOTES = {
    "gitea": "https://git.ryserve.net/public/galaxy.git",
    "github": "git@github.com:rkw6086/galaxy.git",
}
OBJECT_ID = re.compile(r"[0-9a-f]{40}\Z")
MAX_REFS = 10_000
MAX_OUTPUT = 4 * 1024 * 1024


class SyncError(Exception):
    pass


def report(result, **details):
    # JSON also escapes unusual ref names instead of emitting terminal controls.
    print(json.dumps({"result": result, **details}, sort_keys=True), flush=True)


class Repository:
    def __init__(self, directory):
        self.directory = directory
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.startswith("GIT_") or key == "GIT_SSH_COMMAND"
        }
        self.env.update({
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_ASKPASS": str(directory / "askpass.py"),
            "GIT_AUTHOR_NAME": "Galaxy Repository Sync",
            "GIT_AUTHOR_EMAIL": "galaxy-sync@users.noreply.git.ryserve.net",
            "GIT_COMMITTER_NAME": "Galaxy Repository Sync",
            "GIT_COMMITTER_EMAIL": "galaxy-sync@users.noreply.git.ryserve.net",
            "LC_ALL": "C",
        })
        askpass = directory / "askpass.py"
        askpass.write_text(
            "#!/usr/bin/env python3\n"
            "import os, sys\n"
            "prompt = sys.argv[1].lower() if len(sys.argv) > 1 else ''\n"
            "if 'git.ryserve.net' not in prompt:\n"
            "    sys.exit(1)\n"
            "if 'username' in prompt:\n"
            "    print('x-access-token')\n"
            "elif 'password' in prompt:\n"
            "    print(os.environ.get('GITEA_TOKEN', ''))\n"
            "else:\n"
            "    sys.exit(1)\n"
        )
        askpass.chmod(0o700)
        self.git("init", "--bare", "repository", operation="initialize")
        self.directory = directory / "repository"

    def git(self, *args, operation, allowed=(0,), timeout=300):
        # Never expose transport output: credential helpers or remote hook errors
        # can contain credentials. There is no shell interpolation or checkout.
        command = [
            "git", "-c", "core.hooksPath=" + os.devnull,
            "-c", "credential.helper=", "-c", "protocol.file.allow=never",
            "-c", "protocol.ext.allow=never", "-c", "http.followRedirects=false",
            *args,
        ]
        with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
            try:
                result = subprocess.run(
                    command, cwd=self.directory, env=self.env,
                    stdin=subprocess.DEVNULL, stdout=output, stderr=errors,
                    timeout=timeout, check=False,
                )
            except subprocess.TimeoutExpired as error:
                raise SyncError(f"{operation}: timed out; retry the next scheduled sync.") from error
            if result.returncode not in allowed:
                raise SyncError(f"{operation}: Git failed with exit status {result.returncode}.")
            output.seek(0)
            data = output.read(MAX_OUTPUT + 1)
            if len(data) > MAX_OUTPUT:
                raise SyncError(f"{operation}: Git output exceeded the safe limit.")
        try:
            return result.returncode, data.decode("utf-8")
        except UnicodeDecodeError as error:
            raise SyncError(f"{operation}: Git returned non-UTF-8 ref data.") from error

    def refs(self, remote, ref=None):
        patterns = [ref] if ref is not None else ["refs/heads/*", "refs/tags/*"]
        _, output = self.git(
            "ls-remote", "--refs", REMOTES[remote], *patterns,
            operation=f"read {remote} refs", timeout=60,
        )
        result = {}
        for line in output.splitlines():
            fields = line.split("\t")
            if len(fields) != 2 or not OBJECT_ID.fullmatch(fields[0]):
                raise SyncError(f"{remote}: malformed ref advertisement.")
            oid, name = fields
            if not name.startswith(("refs/heads/", "refs/tags/")):
                raise SyncError(f"{remote}: unexpected advertised ref namespace.")
            self.git("check-ref-format", name, operation=f"validate {remote} ref")
            if name in result:
                raise SyncError(f"{remote}: duplicate advertised ref.")
            result[name] = oid
            if len(result) > MAX_REFS:
                raise SyncError(f"{remote}: too many refs for one synchronization.")
        # ls-remote patterns are globs, so callers still select an exact ref.
        return result

    def fetch(self, remote, refs):
        if not refs:
            return
        self.git(
            "fetch", "--no-tags", "--no-recurse-submodules",
            "--no-write-fetch-head", REMOTES[remote],
            *(f"+{ref}:refs/galaxy-sync/{remote}/{ref[5:]}" for ref in sorted(refs)),
            operation=f"fetch {remote}", timeout=600,
        )
        for ref, expected in refs.items():
            _, actual = self.git(
                "rev-parse", "--verify", f"refs/galaxy-sync/{remote}/{ref[5:]}",
                operation=f"verify fetched {remote} ref",
            )
            if actual.strip() != expected:
                raise SyncError(f"{remote}: refs changed while fetching; defer to the next sync.")

    def ancestor(self, older, newer):
        status, _ = self.git(
            "merge-base", "--is-ancestor", older, newer,
            operation="compare branch ancestry", allowed=(0, 1),
        )
        return status == 0

    def merge(self, ref, gitea, github, dry_run):
        status, _ = self.git(
            "merge-base", gitea, github,
            operation="find branch merge base", allowed=(0, 1),
        )
        if status == 1:
            report("conflict", ref=ref, reason="unrelated-history", gitea=gitea, github=github)
            return False
        # merge-tree uses the built-in merge engine in the bare object database.
        # No remote worktree, hooks, custom merge driver or source code is run.
        status, output = self.git(
            "merge-tree", "--write-tree", "--no-messages", gitea, github,
            operation="merge branch histories", allowed=(0, 1),
        )
        if status == 1:
            report("conflict", ref=ref, reason="merge-conflict", gitea=gitea, github=github)
            return False
        tree = output.strip()
        if not OBJECT_ID.fullmatch(tree):
            raise SyncError("Merge engine returned an unexpected tree identity.")
        if dry_run:
            report("would-merge", ref=ref, first_parent=gitea, second_parent=github, tree=tree)
            return True
        _, output = self.git(
            "commit-tree", tree, "-p", gitea, "-p", github,
            "-m", f"Merge GitHub {ref[11:]} into Gitea",
            operation="record clean merge",
        )
        commit = output.strip()
        if not OBJECT_ID.fullmatch(commit):
            raise SyncError("Merge commit returned an unexpected identity.")
        # A successful first publication is durable even if GitHub races or is
        # unavailable; the next job sees the commit and resumes reconciliation.
        self.push("clean-merge", "gitea", ref, commit, gitea, False)
        self.push("clean-merge", "github", ref, commit, github, False)
        report("merged", ref=ref, oid=commit)
        return True

    def push(self, source, destination, ref, oid, expected, dry_run):
        current = self.refs(destination, ref).get(ref)
        if current == oid:
            report("already-synchronized", ref=ref, destination=destination)
            return
        if current != expected:
            raise SyncError(f"{destination}: destination changed before push; defer to the next sync.")
        if destination == "gitea" and not self.env.get("GITEA_TOKEN"):
            if not dry_run:
                raise SyncError("GITEA_TOKEN is required to import GitHub changes.")
        if dry_run:
            report("would-copy", source=source, destination=destination, ref=ref, oid=oid)
            return
        # An ordinary non-force push is the final server-side ancestry guard.
        # A racing divergent update is rejected. A racing fast-forward that is
        # still an ancestor of oid is harmless and may be advanced normally.
        self.git(
            "push", "--porcelain", "--no-follow-tags", REMOTES[destination],
            f"{oid}:{ref}", operation=f"push to {destination}", timeout=600,
        )
        actual = self.refs(destination, ref).get(ref)
        if actual != oid:
            raise SyncError(f"{destination}: ref changed after push; verify on the next sync.")
        report("copied", source=source, destination=destination, ref=ref, oid=oid)


def included_tag(ref):
    return ref.startswith("refs/tags/") and ref != "refs/tags/beta" and not ref.startswith("refs/tags/beta-stage-")


def synchronize(repository, dry_run):
    snapshots = {remote: repository.refs(remote) for remote in REMOTES}
    branches = sorted(
        {ref for refs in snapshots.values() for ref in refs if ref.startswith("refs/heads/")},
        # On an empty GitHub repository the first pushed branch becomes default.
        key=lambda ref: (ref != "refs/heads/master", ref),
    )
    tags = sorted(ref for ref in snapshots["gitea"] if included_tag(ref))
    pending = {remote: {} for remote in REMOTES}
    differing_branches = []
    differing_tags = []
    for ref in branches:
        left, right = (snapshots[remote].get(ref) for remote in REMOTES)
        if left != right:
            differing_branches.append(ref)
            for remote in REMOTES:
                if ref in snapshots[remote]:
                    pending[remote][ref] = snapshots[remote][ref]
    for ref in tags:
        if snapshots["gitea"][ref] != snapshots["github"].get(ref):
            differing_tags.append(ref)
            if ref not in snapshots["github"]:
                pending["gitea"][ref] = snapshots["gitea"][ref]
    if not differing_branches and not differing_tags:
        report("synchronized", branches=len(branches), release_tags=len(tags), copied=0)
        return 0
    for remote, refs in pending.items():
        repository.fetch(remote, refs)
    failures = 0
    for ref in differing_branches:
        left, right = (snapshots[remote].get(ref) for remote in REMOTES)
        try:
            if right is None or (left is not None and repository.ancestor(right, left)):
                source, destination, oid, expected = "gitea", "github", left, right
            elif left is None or repository.ancestor(left, right):
                source, destination, oid, expected = "github", "gitea", right, left
            else:
                if not repository.merge(ref, left, right, dry_run):
                    failures += 1
                continue
            repository.push(source, destination, ref, oid, expected, dry_run)
        except SyncError as error:
            report("failed", ref=ref, reason=str(error))
            failures += 1
    for ref in differing_tags:
        try:
            if ref in snapshots["github"]:
                report("conflict", ref=ref, reason="immutable-tag-mismatch",
                       gitea=snapshots["gitea"][ref], github=snapshots["github"][ref])
                failures += 1
                continue
            repository.push("gitea", "github", ref, snapshots["gitea"][ref], None, dry_run)
        except SyncError as error:
            report("failed", ref=ref, reason=str(error))
            failures += 1
    report("finished", differing_branches=len(differing_branches), differing_tags=len(differing_tags), failures=failures, dry_run=dry_run)
    return 1 if failures else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true", help="Show safe updates without pushing.")
    args = parser.parse_args()
    try:
        with tempfile.TemporaryDirectory(prefix="galaxy-github-sync-") as directory:
            return synchronize(Repository(Path(directory)), args.dry_run)
    except (OSError, SyncError) as error:
        # Do not include arbitrary OS/subprocess error text or environment values.
        reason = str(error) if isinstance(error, SyncError) else "Local Git synchronization setup failed."
        report("failed", reason=reason)
        return 1


if __name__ == "__main__":
    sys.exit(main())
