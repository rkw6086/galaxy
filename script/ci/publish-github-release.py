#!/usr/bin/env python3
"""Keep tagged releases and update one rolling Nightly without deleting releases.

Unique Nightly filenames allow complete verification before replacing its body
or source alias. Only verified older Nightly assets may be retired afterward.
"""
from __future__ import annotations

from dataclasses import dataclass, field
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
BUILD = re.compile(r"([0-9a-f]{40})_([1-9][0-9]{0,18})-([1-9][0-9]{0,8})\Z")
NIGHTLY = re.compile(r"<!-- galaxy-nightly:v1 commit=([0-9a-f]{40}) build=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}) -->")
TRANSITION = re.compile(r"<!-- galaxy-nightly-transition:v1 release=(new|[1-9][0-9]{0,18}) previous=([0-9a-f]{40}|none) build=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}|none) ref=([0-9a-f]{40}|none) candidate=([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}) -->")
OWNED_ASSET = re.compile(r"(?:Galaxy_nightly_([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8})_(?:aarch64\.dmg|x86_64-setup\.exe|amd64\.deb|linux_x86_64\.tar\.gz)|SHA256SUMS_nightly_([0-9a-f]{40}_[1-9][0-9]{0,18}-[1-9][0-9]{0,8}))\Z")
MAX_ASSET = 2 * 1024 * 1024 * 1024
MAX_MANIFEST = 8192


class PublishError(Exception):
    pass


class AssetMismatch(PublishError):
    pass


@dataclass(frozen=True)
class Config:
    channel: str
    version: str
    commit: str
    directory: Path
    token: str = field(repr=False)
    nightly_id: str = ""
    previous_sha: str = ""
    previous_build: str = ""
    previous_id: int | None = None
    previous_ref: str | None = None
    notes: str = ""
    repository: str = "rkw6086/galaxy"

    @property
    def tag(self):
        return "nightly" if self.channel == "nightly" else self.version

    @property
    def identity(self):
        return "nightly_" + self.nightly_id if self.channel == "nightly" else self.version

    @property
    def manifest(self):
        return "SHA256SUMS_nightly_" + self.nightly_id if self.channel == "nightly" else "SHA256SUMS"

    @classmethod
    def from_env(cls, env):
        channel, version, commit = (env.get(key, "") for key in ("GALAXY_BUILD_CHANNEL", "GALAXY_RELEASE_VERSION", "GALAXY_RELEASE_SHA"))
        if channel not in ("nightly", "release") or not VERSION.fullmatch(version) or not COMMIT.fullmatch(commit):
            raise PublishError("Publication requires a Nightly or stable channel, version and exact source.")
        if env.get("GITHUB_SERVER_URL") != "https://github.com" or env.get("GITHUB_REPOSITORY") != "rkw6086/galaxy" or env.get("GITHUB_API_URL", "https://api.github.com") != "https://api.github.com":
            raise PublishError("Publication is restricted to Galaxy on GitHub.")
        if channel == "release" and (env.get("GITHUB_EVENT_NAME") != "push" or env.get("GITHUB_REF") != "refs/tags/" + version):
            raise PublishError("Stable publication requires its pushed version tag.")
        if channel == "nightly" and (env.get("GITHUB_EVENT_NAME") not in ("schedule", "workflow_dispatch") or env.get("GITHUB_REF") != "refs/heads/master"):
            raise PublishError("Nightly publication requires a scheduled or manual master run.")
        token = env.get("GITHUB_TOKEN", "")
        if not token or any(c.isspace() for c in token):
            raise PublishError("The GitHub workflow token is missing or malformed.")
        directory = Path(env.get("GALAXY_RELEASE_DIR", ""))
        if not directory.is_absolute() or not directory.is_dir() or directory.is_symlink():
            raise PublishError("Release artifacts need an absolute regular job directory.")
        nightly_id = env.get("GALAXY_NIGHTLY_ID", "") if channel == "nightly" else ""
        if channel == "nightly":
            match = BUILD.fullmatch(nightly_id)
            attempt = env.get("GITHUB_RUN_ATTEMPT", "")
            if (not match or match[1] != commit or match[2] != env.get("GITHUB_RUN_ID")
                    or not re.fullmatch(r"[1-9][0-9]{0,8}", attempt) or int(match[3]) > int(attempt)):
                raise PublishError("Nightly identity differs from its prepared source/run/attempt.")
        previous_sha = env.get("GALAXY_PREVIOUS_NIGHTLY_SHA", "")
        previous_build = env.get("GALAXY_PREVIOUS_NIGHTLY_BUILD", "")
        previous_id = env.get("GALAXY_PREVIOUS_NIGHTLY_RELEASE_ID", "")
        previous_ref = env.get("GALAXY_PREVIOUS_NIGHTLY_REF", "")
        if any((previous_sha, previous_build, previous_id)):
            old = BUILD.fullmatch(previous_build)
            if not COMMIT.fullmatch(previous_sha) or not old or old[1] != previous_sha or not re.fullmatch(r"[1-9][0-9]{0,18}", previous_id):
                raise PublishError("Prepared Nightly snapshot is invalid.")
        if previous_ref and not COMMIT.fullmatch(previous_ref):
            raise PublishError("Prepared Nightly ref binding is invalid.")
        if previous_ref and not previous_id:
            raise PublishError("A prepared Nightly ref requires its owned release record.")
        notes = ""
        if env.get("GALAXY_RELEASE_NOTES_FILE"):
            path = Path(env["GALAXY_RELEASE_NOTES_FILE"])
            if not path.is_file() or path.is_symlink() or not 0 < path.stat().st_size <= 128 * 1024:
                raise PublishError("Release notes must be a bounded regular file.")
            notes = path.read_text(encoding="utf-8").strip()
        return cls(channel, version, commit, directory, token, nightly_id, previous_sha, previous_build,
                   int(previous_id) if previous_id else None, previous_ref or None, notes)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class GitHub:
    def __init__(self, config):
        self.config = config
        self.api = "https://api.github.com/repos/" + config.repository
        self.opener = urllib.request.build_opener(NoRedirect())
        self.digests = {}
        self.nightly_delete_ids = set()

    def request(self, method, path, body=None, upload=None, missing=False):
        if method == "DELETE":
            match = re.fullmatch(r"/releases/assets/([1-9][0-9]*)", path)
            if self.config.channel != "nightly" or not match or int(match[1]) not in self.nightly_delete_ids:
                raise PublishError("Release deletion is prohibited; only verified owned Nightly assets can be retired.")
        if method != "GET" and path.startswith("/git/"):
            raise PublishError("Ref mutation is restricted to the dedicated Nightly lease operation.")
        headers = {"Authorization": "Bearer " + self.config.token,
                   "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2026-03-10"}
        url = self.api + path
        data = None
        stream = None
        if upload is not None:
            if method != "POST" or not re.fullmatch(r"/releases/[1-9][0-9]*/assets\?name=[A-Za-z0-9_.%-]+", path):
                raise PublishError("Invalid release asset upload identity.")
            url = "https://uploads.github.com/repos/" + self.config.repository + path
            stream = upload.open("rb")
            data = stream
            headers.update({"Content-Type": "application/octet-stream", "Content-Length": str(upload.stat().st_size)})
        elif body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        try:
            request = urllib.request.Request(url, data=data, method=method, headers=headers)
            with self.opener.open(request, timeout=600 if stream else 60) as response:
                raw = response.read(8 * 1024 * 1024 + 1)
                if len(raw) > 8 * 1024 * 1024:
                    raise PublishError("GitHub API response exceeded the safe bound.")
                return json.loads(raw) if raw else None
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            if status == 404 and missing:
                return None
            raise PublishError(f"GitHub API returned HTTP {status}; reconcile release state before retrying a write.") from None
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
            raise PublishError("GitHub API operation failed; a write may have completed. Reconcile on the next run.") from None
        finally:
            if stream is not None:
                stream.close()

    def download(self, asset, limit, digest=False):
        asset_id, expected_size = asset.get("id"), asset.get("size")
        if not isinstance(asset_id, int) or asset_id <= 0 or not isinstance(expected_size, int) or not 0 < expected_size <= limit:
            raise PublishError("Release asset metadata is invalid.")
        key = (asset_id, expected_size)
        if digest and key in self.digests:
            return self.digests[key]
        # Draft browser URLs require browser authorization and may return 404.
        # The authenticated asset API is the documented binary download route.
        url = self.api + f"/releases/assets/{asset_id}"
        for _ in range(6):
            parsed = urllib.parse.urlsplit(url)
            if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
                raise PublishError("Asset download is not a safe HTTPS URL.")
            headers = {"Accept": "application/octet-stream"}
            if parsed.netloc == "api.github.com":
                headers.update({"Authorization": "Bearer " + self.config.token, "X-GitHub-Api-Version": "2026-03-10"})
            try:
                with self.opener.open(urllib.request.Request(url, headers=headers), timeout=180) as response:
                    total = 0
                    value = hashlib.sha256() if digest else bytearray()
                    while chunk := response.read(1024 * 1024):
                        total += len(chunk)
                        if total > expected_size or total > limit:
                            raise PublishError("Downloaded asset exceeded its declared size.")
                        value.update(chunk) if digest else value.extend(chunk)
                    if total != expected_size:
                        raise PublishError("Downloaded asset size differs from its metadata.")
                    if digest:
                        result = value.hexdigest()
                        self.digests[key] = result
                        return result
                    return bytes(value)
            except urllib.error.HTTPError as error:
                status, location = error.code, error.headers.get("Location")
                error.close()
                if status in (301, 302, 303, 307, 308) and location:
                    url = urllib.parse.urljoin(url, location)
                    continue
                raise PublishError(f"Asset download returned HTTP {status}; URLs and credentials withheld.") from None
            except (urllib.error.URLError, TimeoutError):
                raise PublishError("Asset verification download failed; URLs and credentials withheld.") from None
        raise PublishError("Asset download exceeded the redirect limit.")


def listing(client, path, key=None):
    result = []
    for page in range(1, 101):
        value = client.request("GET", path + ("&" if "?" in path else "?") + f"per_page=100&page={page}")
        values = value.get(key) if key and isinstance(value, dict) else value
        if not isinstance(values, list):
            raise PublishError("GitHub returned an invalid paginated list.")
        result.extend(values)
        if len(values) < 100:
            return result
    raise PublishError("GitHub listing exceeded its safe bound.")


def payload_names(identity):
    return {f"Galaxy_{identity}_aarch64.dmg", f"Galaxy_{identity}_x86_64-setup.exe",
            f"Galaxy_{identity}_amd64.deb", f"Galaxy_{identity}_linux_x86_64.tar.gz"}


def digest_file(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def release_files(config):
    expected = payload_names(config.identity)
    files, manifests = {}, []
    entries = list(config.directory.rglob("*"))
    if len(entries) > 1000:
        raise PublishError("Too many artifact inputs.")
    for path in entries:
        if path.is_symlink():
            raise PublishError("Artifact inputs cannot contain symlinks.")
        if not path.is_file():
            continue
        if path.name in expected:
            if path.name in files or not 0 < path.stat().st_size <= MAX_ASSET:
                raise PublishError("Payloads are duplicated, empty, or too large.")
            files[path.name] = (path, digest_file(path))
        elif path.name == "SHA256SUMS" and path.parent != config.directory:
            manifests.append(path)
        elif path == config.directory / config.manifest:
            pass  # This job's previously generated combined manifest.
        else:
            raise PublishError("Artifact directory contains an unexpected file.")
    if set(files) != expected or not manifests:
        raise PublishError("All four platform payloads and adjacent checksums are required.")
    checked = set()
    for manifest in manifests:
        if not 0 < manifest.stat().st_size <= MAX_MANIFEST:
            raise PublishError("Platform checksum size is invalid.")
        for line in manifest.read_text(encoding="ascii").splitlines():
            match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_.-]+)", line)
            if not match or match[2] not in files or match[2] in checked:
                raise PublishError("Platform checksum identity is invalid or duplicated.")
            path, digest = files[match[2]]
            if path.parent != manifest.parent or not hmac.compare_digest(digest, match[1]):
                raise PublishError("Payload differs from its adjacent checksum.")
            checked.add(match[2])
    if checked != expected:
        raise PublishError("Checksums do not cover every platform.")
    checksum = config.directory / config.manifest
    checksum.write_text("".join(f"{files[name][1]}  {name}\n" for name in sorted(files)), encoding="ascii")
    files[checksum.name] = (checksum, digest_file(checksum))
    return files


def release_path(tag):
    return "/releases/tags/" + urllib.parse.quote(tag, safe="")


def raw_tag(client, tag, missing=False):
    value = client.request("GET", "/git/ref/tags/" + urllib.parse.quote(tag, safe=""), missing=missing)
    if value is None:
        return None
    if value.get("ref") != "refs/tags/" + tag:
        raise PublishError("Unexpected remote tag identity.")
    return value.get("object", {})


def tag_commit(client, tag):
    value = raw_tag(client, tag)
    for _ in range(8):
        if value.get("type") == "commit" and COMMIT.fullmatch(value.get("sha", "")):
            return value["sha"]
        if value.get("type") != "tag" or not COMMIT.fullmatch(value.get("sha", "")):
            break
        value = client.request("GET", "/git/tags/" + value["sha"]).get("object", {})
    raise PublishError("Tag does not identify a bounded commit object.")


def nightly_ref(client):
    value = raw_tag(client, "nightly", missing=True)
    if value is None:
        return None
    if value.get("type") != "commit" or not COMMIT.fullmatch(value.get("sha", "")):
        raise PublishError("Nightly must use its owned lightweight source ref.")
    return value["sha"]


def primary_source(config):
    if config.channel == "nightly":
        verify_nightly_ancestry(config)
        return
    ref = "refs/tags/" + config.version
    try:
        rows = subprocess.check_output(["git", "-c", "credential.helper=", "ls-remote",
            "https://git.ryserve.net/public/galaxy.git", ref, ref + "^{}"], cwd=ROOT,
            stdin=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=60).splitlines()
    except (OSError, subprocess.SubprocessError):
        raise PublishError("Cannot verify the Gitea primary source.") from None
    refs = {name: oid for oid, name in (row.split("\t") for row in rows)}
    if refs.get(ref + "^{}", refs.get(ref)) != config.commit:
        raise PublishError("Gitea primary changed; candidate was not committed.")


def verify_nightly_ancestry(config):
    # Prepare already bound this exact snapshot to both trusted masters. Normal
    # descendant pushes during compilation must not invalidate that day's build.
    # Fetch only commit graphs into a disposable bare repository: no checkout,
    # source hooks, submodules, tags, LFS smudging, or inherited Git configuration.
    env = {key: value for key, value in os.environ.items()
           if not key.startswith("GIT_") and key not in ("SSH_ASKPASS", "SSH_ASKPASS_REQUIRE")}
    env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
               GIT_TERMINAL_PROMPT="0", GIT_NO_LAZY_FETCH="1")
    command = ["git", "-c", "core.hooksPath=" + os.devnull,
               "-c", "credential.helper=", "-c", "protocol.allow=never",
               "-c", "protocol.https.allow=always", "-c", "http.followRedirects=false",
               "-c", "maintenance.auto=false", "-c", "gc.auto=0"]
    with tempfile.TemporaryDirectory(prefix="galaxy-nightly-graph-") as directory:
        def graph(*args, allowed=(0,), timeout=30):
            with tempfile.TemporaryFile() as output, tempfile.TemporaryFile() as errors:
                try:
                    result = subprocess.run(command + list(args), cwd=directory, env=env,
                        stdin=subprocess.DEVNULL, stdout=output, stderr=errors,
                        timeout=timeout, check=False)
                except (OSError, subprocess.SubprocessError):
                    raise PublishError("Cannot verify fresh Nightly master ancestry; candidate was not committed.") from None
                if result.returncode not in allowed:
                    raise PublishError("Cannot verify fresh Nightly master ancestry; candidate was not committed.")
                output.seek(0)
                value = output.read(256)
                if len(value) == 256:
                    raise PublishError("Unexpected Nightly graph verification output.")
                return result.returncode, value.decode("ascii").strip()

        graph("init", "--bare", "--quiet", ".")
        for owner, url in (("Gitea", "https://git.ryserve.net/public/galaxy.git"),
                           ("GitHub", "https://github.com/rkw6086/galaxy.git")):
            ref = "refs/galaxy-nightly-verification/" + owner.lower()
            graph("fetch", "--quiet", "--no-tags", "--no-recurse-submodules",
                  "--no-write-fetch-head", "--filter=tree:0", url,
                  "refs/heads/master:" + ref, timeout=120)
            _, head = graph("rev-parse", "--verify", ref + "^{commit}")
            if not COMMIT.fullmatch(head):
                raise PublishError("Fresh Nightly master identity is invalid.")
            status, _ = graph("merge-base", "--is-ancestor", config.commit, head, allowed=(0, 1))
            if status:
                raise PublishError(f"Nightly snapshot is no longer an ancestor of {owner} master; candidate was not committed.")


def assets(client, release):
    values = listing(client, f"/releases/{release['id']}/assets")
    names = [value.get("name") for value in values]
    if any(not isinstance(name, str) for name in names) or len(set(names)) != len(names):
        raise PublishError("Asset identities are invalid or duplicated.")
    return {value["name"]: value for value in values}


def verify_local(client, release, files, complete=False):
    found = assets(client, release)
    if complete and not set(files) <= set(found):
        raise AssetMismatch("Candidate downloads are incomplete; previous assets remain intact.")
    for name in set(files) & set(found):
        path, digest = files[name]
        if found[name].get("size") != path.stat().st_size or not hmac.compare_digest(client.download(found[name], MAX_ASSET, digest=True), digest):
            raise AssetMismatch("An existing asset name has different content; it will not be replaced.")
    return found


def upload_files(client, release, files):
    found = verify_local(client, release, files)
    for name, (path, _) in files.items():
        if name not in found:
            client.request("POST", f"/releases/{release['id']}/assets?name=" + urllib.parse.quote(name, safe=""), upload=path)
    return verify_local(client, release, files, complete=True)


def verify_subset(client, release, identity, manifest):
    found = assets(client, release)
    expected = payload_names(identity)
    if not expected | {manifest} <= set(found):
        raise PublishError("Nightly candidate is missing downloads or its unique checksums.")
    raw = client.download(found[manifest], MAX_MANIFEST)
    checksums = {}
    for line in raw.decode("ascii").splitlines():
        match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_.-]+)", line)
        if not match or match[2] not in expected or match[2] in checksums:
            raise PublishError("Combined checksum manifest is invalid.")
        checksums[match[2]] = match[1]
    if set(checksums) != expected:
        raise PublishError("Combined checksums do not cover all platforms.")
    for name in expected:
        if not hmac.compare_digest(client.download(found[name], MAX_ASSET, digest=True), checksums[name]):
            raise PublishError("Candidate download failed checksum verification.")
    return {name: found[name] for name in expected | {manifest}}


def nightly_state(release, ref):
    if release.get("tag_name") != "nightly" or release.get("name") != "Nightly" or not release.get("prerelease") or not isinstance(release.get("id"), int) or release["id"] <= 0:
        raise PublishError("Existing Nightly is not an owned rolling record.")
    markers = NIGHTLY.findall(release.get("body") or "")
    transitions = TRANSITION.findall(release.get("body") or "")
    if len(markers) != 1 or len(transitions) > 1:
        raise PublishError("Nightly identity markers are invalid.")
    commit, build = markers[0]
    if not BUILD.fullmatch(build) or BUILD.fullmatch(build)[1] != commit:
        raise PublishError("Nightly source/build identity is invalid.")
    transition = transitions[0] if transitions else None
    initial = False
    if transition:
        record_id, previous, previous_build, old_ref, candidate = transition
        if candidate != build or (previous == "none") != (previous_build == "none") or (previous_build != "none" and BUILD.fullmatch(previous_build)[1] != previous):
            raise PublishError("Nightly transition source identity is invalid.")
        initial = record_id == "new"
        if initial:
            if not release.get("draft") or previous != "none" or old_ref != "none":
                raise PublishError("Initial Nightly marker is only valid on a new owned draft.")
        elif record_id != str(release["id"]):
            raise PublishError("Nightly transition belongs to another release record.")
        if ref not in (None if old_ref == "none" else old_ref, commit):
            raise PublishError("Nightly alias changed outside its recorded transition.")
    elif release.get("draft") or ref != commit:
        raise PublishError("Nightly record and alias do not agree.")
    return {"id": release["id"], "commit": commit, "build": build, "transition": transition,
            "initial": initial, "draft": bool(release.get("draft"))}


def state_key(state):
    return state["id"], state["commit"], state["build"], state["transition"], state["draft"]


def body_for(config, notes, transition=None):
    names = payload_names(config.identity) | {config.manifest}
    links = "\n".join(f"- [{name}](https://github.com/{config.repository}/releases/download/{config.tag}/{name})" for name in sorted(names))
    marker = (f"<!-- galaxy-nightly:v1 commit={config.commit} build={config.nightly_id} -->" if config.channel == "nightly"
              else f"<!-- galaxy-release:v1 tag={config.version} commit={config.commit} -->")
    body = (notes.strip() + "\n\nDownloads: Apple Silicon macOS DMG (signed and notarized), Windows installer, "
            "and Linux packages. Verify each download with the linked checksum manifest.\n\n" + links
            + f"\n\nSource commit: `{config.commit}`\n\n{marker}")
    if transition:
        body += "\n\n" + transition
    return body


def transition_marker(record_id, previous, previous_build, old_ref, candidate):
    return (f"<!-- galaxy-nightly-transition:v1 release={record_id} previous={previous or 'none'} "
            f"build={previous_build or 'none'} ref={old_ref or 'none'} candidate={candidate} -->")


def move_nightly_ref(client, config, expected):
    current = nightly_ref(client)
    if current == config.commit:
        return
    if current != expected:
        raise PublishError("Nightly alias changed before its guarded update.")
    # Force is restricted to this explicit rolling alias. The lease is an
    # atomic server-side comparison; a racing human update is never overwritten.
    with tempfile.TemporaryDirectory(prefix="galaxy-nightly-auth-") as directory:
        askpass = Path(directory) / "askpass.py"
        askpass.write_text("#!/usr/bin/env python3\nimport os,sys\np=sys.argv[1].lower() if len(sys.argv)>1 else ''\n"
            "if 'github.com' not in p: sys.exit(1)\n"
            "if 'username' in p: print('x-access-token')\n"
            "elif 'password' in p: print(os.environ['GITHUB_TOKEN'])\n"
            "else: sys.exit(1)\n")
        askpass.chmod(0o700)
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(GIT_ASKPASS=str(askpass), GIT_TERMINAL_PROMPT="0", GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        try:
            result = subprocess.run(["git", "-c", "credential.helper=", "-c", "core.hooksPath=" + os.devnull,
                "-c", "http.followRedirects=false", "push", "--no-follow-tags",
                "--force-with-lease=refs/tags/nightly:" + (expected or ""),
                "https://github.com/rkw6086/galaxy.git", config.commit + ":refs/tags/nightly"],
                cwd=ROOT, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                timeout=120, check=False)
        except (OSError, subprocess.SubprocessError):
            raise PublishError("Nightly alias update outcome is unknown; reconcile its recorded transition.") from None
        if result.returncode:
            raise PublishError("Nightly alias lease was rejected or transport failed; previous assets remain intact.")
    if nightly_ref(client) != config.commit:
        raise PublishError("Nightly alias update was not confirmed.")


def finish_nightly(client, config, release, files):
    ref = nightly_ref(client)
    state = nightly_state(release, ref)
    if (state["commit"], state["build"]) != (config.commit, config.nightly_id) or state["initial"]:
        raise PublishError("Cannot finalize another or unprepared Nightly candidate.")
    verify_local(client, release, files, complete=True)
    verify_subset(client, release, config.identity, config.manifest)
    if state["transition"]:
        expected = state["transition"][3]
        current = client.request("GET", f"/releases/{release['id']}")
        if state_key(nightly_state(current, nightly_ref(client))) != state_key(state):
            raise PublishError("Nightly intent changed before alias finalization.")
        move_nightly_ref(client, config, None if expected == "none" else expected)
        verify_subset(client, release, config.identity, config.manifest)
        current = client.request("GET", f"/releases/{release['id']}")
        current_state = nightly_state(current, nightly_ref(client))
        if state_key(current_state) != state_key(state) or nightly_ref(client) != config.commit:
            raise PublishError("Nightly intent changed before completion.")
        body = TRANSITION.sub("", current["body"]).strip()
        release = client.request("PATCH", f"/releases/{release['id']}", body={"body": body, "draft": False,
            "name": "Nightly", "prerelease": True, "make_latest": "false", "target_commitish": config.commit})
    state = nightly_state(release, nightly_ref(client))
    if state["draft"] or state["transition"] or (state["commit"], state["build"]) != (config.commit, config.nightly_id):
        raise PublishError("Nightly completion was not confirmed.")
    verify_subset(client, release, config.identity, config.manifest)
    retire_nightly_assets(client, config, release)


def retire_nightly_assets(client, config, release):
    if config.channel != "nightly":
        raise PublishError("Tagged release assets can never be retired by this publisher.")
    current_names = payload_names(config.identity) | {config.manifest}
    for name, value in assets(client, release).items():
        if name in current_names or not OWNED_ASSET.fullmatch(name):
            continue
        # This namespace and uploader identify files owned by this workflow.
        if value.get("uploader", {}).get("login") != "github-actions[bot]":
            continue
        current = client.request("GET", f"/releases/{release['id']}")
        state = nightly_state(current, nightly_ref(client))
        if state["draft"] or state["transition"] or (state["commit"], state["build"]) != (config.commit, config.nightly_id):
            raise PublishError("Nightly changed before old-asset cleanup; stopped.")
        fresh = client.request("GET", f"/releases/assets/{value['id']}", missing=True)
        if fresh is None:
            continue
        if any(fresh.get(key) != value.get(key) for key in ("id", "name", "size", "updated_at")):
            raise PublishError("An old Nightly asset changed before cleanup; left unchanged.")
        client.nightly_delete_ids.add(value["id"])
        client.request("DELETE", f"/releases/assets/{value['id']}")
        if client.request("GET", f"/releases/assets/{value['id']}", missing=True) is not None:
            raise PublishError("Old Nightly asset removal was not confirmed.")


def publish_nightly(config, client, files, notes):
    release = client.request("GET", release_path("nightly"), missing=True)
    ref = nightly_ref(client)
    if release is None:
        if config.previous_id is not None or ref is not None:
            raise PublishError("Prepared Nightly record disappeared or an unowned alias exists; it will not be recreated.")
        primary_source(config)
        initial = transition_marker("new", "", "", None, config.nightly_id)
        release = client.request("POST", "/releases", body={"tag_name": "nightly", "target_commitish": config.commit,
            "name": "Nightly", "body": body_for(config, notes, initial), "draft": True,
            "prerelease": True, "make_latest": "false"})
        ref = nightly_ref(client)
    state = nightly_state(release, ref)
    if config.previous_id is not None and state["id"] != config.previous_id:
        raise PublishError("Nightly release record changed since preparation.")
    same_candidate = (state["commit"], state["build"]) == (config.commit, config.nightly_id)
    if same_candidate and not state["initial"]:
        # Retry after a committed body/alias operation: exact bytes establish
        # idempotence, not merely an unchanged source or reusable record ID.
        finish_nightly(client, config, release, files)
        return
    prepared = (config.previous_id, config.previous_sha, config.previous_build, config.previous_ref)
    observed = (state["id"], state["commit"], state["build"], ref)
    initial_retry = same_candidate and state["initial"] and config.previous_id is None
    if not initial_retry and prepared != observed:
        raise PublishError("Nightly changed since preparation, including another build of the same source.")
    if state["transition"] and not state["initial"]:
        # A previous body's complete downloads establish the recorded ownership
        # of an older alias even if that previous alias update was interrupted.
        verify_subset(client, release, "nightly_" + state["build"], "SHA256SUMS_nightly_" + state["build"])
    upload_files(client, release, files)
    primary_source(config)
    current = client.request("GET", f"/releases/{release['id']}")
    if state_key(nightly_state(current, nightly_ref(client))) != state_key(state) or nightly_ref(client) != ref:
        raise PublishError("Nightly changed while candidate downloads were uploaded; existing assets remain intact.")
    previous = state["commit"] if not state["draft"] else ""
    previous_build = state["build"] if not state["draft"] else ""
    if state["draft"] and state["transition"] and not state["initial"]:
        previous = "" if state["transition"][1] == "none" else state["transition"][1]
        previous_build = "" if state["transition"][2] == "none" else state["transition"][2]
    intent = transition_marker(state["id"], previous, previous_build, ref, config.nightly_id)
    release = client.request("PATCH", f"/releases/{release['id']}", body={"name": "Nightly",
        "body": body_for(config, notes, intent), "target_commitish": config.commit,
        "draft": state["draft"], "prerelease": True, "make_latest": "false"})
    finish_nightly(client, config, release, files)


def stable_identity(release, version, commit):
    markers = re.findall(r"<!-- galaxy-release:v1 tag=" + re.escape(version) + r" commit=([0-9a-f]{40}) -->", release.get("body") or "")
    if release.get("tag_name") != version or release.get("prerelease") or markers != [commit]:
        raise PublishError("Existing tagged release has another identity; left unchanged.")


def publish_stable(config, client, files, notes):
    primary_source(config)
    if tag_commit(client, config.tag) != config.commit:
        raise PublishError("Stable tag changed before publication.")
    release = client.request("GET", release_path(config.tag), missing=True)
    if release is None:
        release = client.request("POST", "/releases", body={"tag_name": config.tag, "target_commitish": config.commit,
            "name": "Galaxy " + config.version, "body": body_for(config, notes), "draft": True, "prerelease": False})
    stable_identity(release, config.version, config.commit)
    if release.get("draft"):
        upload_files(client, release, files)
        primary_source(config)
        if tag_commit(client, config.tag) != config.commit:
            raise PublishError("Stable tag changed while uploading; publication stopped.")
        # Queued tags can finish out of version order. Preserve the newer
        # release's Latest designation while still publishing this version.
        version_key = tuple(map(int, config.version.split(".")))
        newer = any(not value.get("draft") and not value.get("prerelease")
                    and VERSION.fullmatch(value.get("tag_name", ""))
                    and tuple(map(int, value["tag_name"].split("."))) > version_key
                    for value in listing(client, "/releases"))
        release = client.request("PATCH", f"/releases/{release['id']}", body={"draft": False, "make_latest": "false" if newer else "true"})
    else:
        verify_local(client, release, files, complete=True)
    stable_identity(release, config.version, config.commit)
    if release.get("draft") or tag_commit(client, config.tag) != config.commit:
        raise PublishError("Tagged publication was not confirmed.")
    verify_subset(client, release, config.identity, config.manifest)
    # Tagged releases, assets and numeric tags are retained indefinitely.


def notes_for(config):
    if config.notes:
        return config.notes
    baseline = config.previous_sha
    try:
        rows = subprocess.check_output(["git", "log", "--format=%h %s", "--max-count=201",
            f"{baseline}..{config.commit}" if baseline else config.commit, "--"], cwd=ROOT,
            stderr=subprocess.PIPE, text=True, timeout=30).splitlines()
    except (OSError, subprocess.SubprocessError):
        raise PublishError("Could not generate bounded publication notes.") from None
    lines, size = [], 0
    for row in rows[:200]:
        line = "- " + re.sub(r"[\x00-\x1f\x7f]", " ", row)[:500]
        size += len(line.encode("utf-8")) + 1
        if size > 96 * 1024:
            break
        lines.append(line)
    notes = "\n".join(lines) or "Rebuilt the same source; no source changes."
    if len(rows) > len(lines):
        notes += f"\n\nShowing the latest {len(lines)} commits within the release-note limit."
    return notes


def main():
    try:
        config = Config.from_env(os.environ)
        files = release_files(config)
        client = GitHub(config)
        notes = notes_for(config)
        if not notes.strip() or len(notes.encode("utf-8")) > 128 * 1024:
            raise PublishError("Publication notes must be nonempty and bounded.")
        (publish_nightly if config.channel == "nightly" else publish_stable)(config, client, files, notes)
        url = "https://github.com/" + config.repository + "/releases/tag/" + config.tag
        print("Verified GitHub downloads published: " + url)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as stream:
                stream.write("release_url=" + url + "\n")
        return 0
    except PublishError as error:
        print("Galaxy GitHub publication failed: " + str(error), file=sys.stderr)
    except (OSError, ValueError, TypeError, KeyError, UnicodeError):
        print("Galaxy GitHub publication failed; existing releases were retained.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
