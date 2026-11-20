#!/usr/bin/env python3
"""Publish verified Windows, Linux and signed macOS downloads on GitHub.

Only Galaxy-owned releases are replaced. Numeric tags are never changed or
deleted. A beta is promoted only after its staged public downloads are healthy.
Unknown write outcomes stop for read-based reconciliation on the next run.
"""
from __future__ import annotations

from dataclasses import dataclass, field, replace
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\Z")
COMMIT = re.compile(r"[0-9a-f]{40}\Z")
STAGE = re.compile(r"beta-stage-([0-9a-f]{40})-[0-9a-f]{16}\Z")
CUTOVER = re.compile(r"<!-- galaxy-beta-cutover:v1 previous=([0-9a-f]{40}|none) release=([1-9][0-9]{0,18}|none) orphan=([0-9a-f]{40}|none) -->")
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
    previous: str = ""
    notes: str = ""
    repository: str = "rkw6086/galaxy"
    previous_release_id: int | None = None
    # Set only after verifying an owned published staging proof, never from env.
    recovery_orphan: str = ""

    @property
    def tag(self):
        return "beta" if self.channel == "beta" else self.version

    @classmethod
    def from_env(cls, env):
        channel, version, commit = (env.get(key, "") for key in
            ("GALAXY_BUILD_CHANNEL", "GALAXY_RELEASE_VERSION", "GALAXY_RELEASE_SHA"))
        if channel not in ("beta", "release") or not VERSION.fullmatch(version) or not COMMIT.fullmatch(commit):
            raise PublishError("Publication needs an exact beta/release channel, stable version, and source commit.")
        if env.get("GITHUB_SERVER_URL") != "https://github.com" or env.get("GITHUB_REPOSITORY") != "rkw6086/galaxy":
            raise PublishError("Publication is restricted to the Galaxy GitHub repository.")
        if env.get("GITHUB_API_URL", "https://api.github.com") != "https://api.github.com":
            raise PublishError("Publication requires the official GitHub API.")
        if channel == "release" and (env.get("GITHUB_EVENT_NAME") != "push" or env.get("GITHUB_REF") != "refs/tags/" + version):
            raise PublishError("Stable publication requires its pushed immutable version tag.")
        if channel == "beta" and (env.get("GITHUB_EVENT_NAME") not in ("schedule", "workflow_dispatch") or env.get("GITHUB_REF") != "refs/heads/master"):
            raise PublishError("Beta publication requires a scheduled or manual master run.")
        token = env.get("GITHUB_TOKEN", "")
        if not token or any(c.isspace() for c in token):
            raise PublishError("The GitHub workflow token is missing or malformed.")
        directory = Path(env.get("GALAXY_RELEASE_DIR", ""))
        if not directory.is_absolute() or not directory.is_dir() or directory.is_symlink():
            raise PublishError("Release inputs must use an absolute regular job artifact directory.")
        previous = env.get("GALAXY_PREVIOUS_BETA_SHA", "")
        if previous and not COMMIT.fullmatch(previous):
            raise PublishError("Previous beta must be an exact source commit.")
        previous_id = env.get("GALAXY_PREVIOUS_BETA_RELEASE_ID", "")
        if previous_id and not re.fullmatch(r"[1-9][0-9]{0,18}", previous_id):
            raise PublishError("Previous beta release identity is invalid.")
        notes = ""
        if env.get("GALAXY_RELEASE_NOTES_FILE"):
            path = Path(env["GALAXY_RELEASE_NOTES_FILE"])
            if not path.is_file() or path.is_symlink() or not 0 < path.stat().st_size <= 128 * 1024:
                raise PublishError("Release notes must be a bounded regular file.")
            notes = path.read_text(encoding="utf-8").strip()
        return cls(channel, version, commit, directory, token, previous, notes,
                   previous_release_id=int(previous_id) if previous_id else None)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


class GitHub:
    def __init__(self, config):
        self.config = config
        self.api = "https://api.github.com/repos/" + config.repository
        self.opener = urllib.request.build_opener(NoRedirect())
        self.digests = {}

    def request(self, method, path, body=None, upload=None, missing=False):
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


def expected_names(channel, version, commit):
    identity = "beta_" + commit if channel == "beta" else version
    return {f"Galaxy_{identity}_aarch64.dmg", f"Galaxy_{identity}_x86_64-setup.exe",
            f"Galaxy_{identity}_amd64.deb", f"Galaxy_{identity}_linux_x86_64.tar.gz"}


def digest_file(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def release_files(config):
    expected = expected_names(config.channel, config.version, config.commit)
    files, manifests = {}, []
    entries = list(config.directory.rglob("*"))
    if len(entries) > 1000:
        raise PublishError("Too many release input entries.")
    for path in entries:
        if path.is_symlink():
            raise PublishError("Release inputs cannot contain symlinks.")
        if not path.is_file():
            continue
        if path.name in expected:
            if path.name in files or not 0 < path.stat().st_size <= MAX_ASSET:
                raise PublishError("Release payloads are duplicated, empty, or too large.")
            files[path.name] = (path, digest_file(path))
        elif path.name == "SHA256SUMS":
            if path != config.directory / "SHA256SUMS":
                manifests.append(path)
        else:
            raise PublishError("Release artifact directory contains an unexpected file.")
    if set(files) != expected or not manifests:
        raise PublishError("All Windows, Linux and macOS payloads and platform checksums are required.")
    checked = set()
    for manifest in manifests:
        if not 0 < manifest.stat().st_size <= MAX_MANIFEST:
            raise PublishError("Platform checksum manifest size is invalid.")
        for line in manifest.read_text(encoding="ascii").splitlines():
            match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_.-]+)", line)
            if not match or match[2] not in files or match[2] in checked:
                raise PublishError("Platform checksum manifest has an invalid or duplicate filename.")
            path, digest = files[match[2]]
            if path.parent != manifest.parent or not hmac.compare_digest(digest, match[1]):
                raise PublishError("Platform payload does not match its adjacent checksum manifest.")
            checked.add(match[2])
    if checked != expected:
        raise PublishError("Platform checksum manifests do not cover every payload.")
    checksum = config.directory / "SHA256SUMS"
    checksum.write_text("".join(f"{files[name][1]}  {name}\n" for name in sorted(files)), encoding="ascii")
    files[checksum.name] = (checksum, digest_file(checksum))
    return files


def release_path(tag):
    return "/releases/tags/" + urllib.parse.quote(tag, safe="")


def tag_commit(client, tag, missing=False):
    ref = client.request("GET", "/git/ref/tags/" + urllib.parse.quote(tag, safe=""), missing=missing)
    if ref is None:
        return None
    if ref.get("ref") != "refs/tags/" + tag:
        raise PublishError("GitHub returned an unexpected tag identity.")
    value = ref.get("object", {})
    for _ in range(8):
        if value.get("type") == "commit" and COMMIT.fullmatch(value.get("sha", "")):
            return value["sha"]
        if value.get("type") != "tag" or not COMMIT.fullmatch(value.get("sha", "")):
            break
        value = client.request("GET", "/git/tags/" + value["sha"]).get("object", {})
    raise PublishError("GitHub tag does not identify a bounded commit object.")


def check_tag(client, tag, commit):
    if tag_commit(client, tag) != commit:
        raise PublishError("Release tag differs from the validated source.")


def primary_source(config):
    ref = "refs/heads/master" if config.channel == "beta" else "refs/tags/" + config.version
    try:
        rows = subprocess.check_output(["git", "-c", "credential.helper=", "ls-remote",
            "https://git.ryserve.net/public/galaxy.git", ref, ref + "^{}"], cwd=ROOT,
            stdin=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=60).splitlines()
    except (subprocess.SubprocessError, OSError):
        raise PublishError("Cannot verify the Gitea primary source before publication.") from None
    refs = {name: oid for oid, name in (row.split("\t") for row in rows)}
    if refs.get(ref + "^{}", refs.get(ref)) != config.commit:
        raise PublishError("Gitea primary advanced or its tag differs; stale publication stopped.")


def owned(release):
    tag, body = release.get("tag_name", ""), release.get("body") or ""
    if VERSION.fullmatch(tag) and not release.get("prerelease"):
        match = re.findall(r"<!-- galaxy-release:v1 tag=" + re.escape(tag) + r" commit=([0-9a-f]{40}) -->", body)
        channel = "release"
    elif (tag == "beta" or STAGE.fullmatch(tag)) and release.get("prerelease"):
        match = re.findall(r"<!-- galaxy-beta:v1 commit=([0-9a-f]{40}) -->", body)
        channel = "beta"
    else:
        return None
    if len(match) != 1 or release.get("target_commitish") != match[0]:
        return None
    if STAGE.fullmatch(tag) and STAGE.fullmatch(tag)[1] != match[0]:
        return None
    return channel, match[0]


def require_owned(release, channel, commit, tag):
    if release.get("tag_name") != tag or owned(release) != (channel, commit):
        raise PublishError("Existing release does not match this owned channel/source; left unchanged.")


def cutover_identity(release):
    matches = CUTOVER.findall(release.get("body") or "")
    if len(matches) != 1:
        return None
    previous, release_id, orphan = matches[0]
    if (previous == "none") != (release_id == "none"):
        return None
    return ("" if previous == "none" else previous,
            None if release_id == "none" else int(release_id),
            "" if orphan == "none" else orphan)


def verify_recovery_snapshot(client, stage, found):
    current = client.request("GET", f"/releases/{stage['id']}")
    if (current.get("tag_name") != stage.get("tag_name") or current.get("draft")
            or owned(current) != owned(stage) or cutover_identity(current) != cutover_identity(stage)):
        raise PublishError("Staging cutover proof changed during verification.")
    check_tag(client, current["tag_name"], owned(current)[1])
    refreshed = assets(client, current)
    if ({name: (value.get("id"), value.get("size")) for name, value in refreshed.items()}
            != {name: (value.get("id"), value.get("size")) for name, value in found.items()}):
        raise PublishError("Staging cutover proof assets changed during verification.")


def recover_beta(config, client, files):
    # Deleting a release retains its Git ref. A fresh workflow may therefore
    # have no previous-release metadata even though an old beta ref remains.
    # The immutable staging record must have persisted this exact cutover
    # before removal; an arbitrary unattached ref never establishes ownership.
    if client.request("GET", release_path("beta"), missing=True) is not None:
        return config
    orphan = tag_commit(client, "beta", missing=True)
    if orphan is None:
        # An unknown DELETE outcome can leave neither the old record nor ref.
        # Reuse only the exact current candidate/hash, never another build's
        # unrelated draft, to recover the persisted preparation metadata.
        stage_tag = f"beta-stage-{config.commit}-{files['SHA256SUMS'][1][:16]}"
        stage = client.request("GET", release_path(stage_tag), missing=True)
        proof = cutover_identity(stage) if stage else None
        if stage is None or stage.get("draft") or owned(stage) != ("beta", config.commit) or proof is None:
            return config
        previous, prior_id, _ = proof
        if ((config.previous or config.previous_release_id is not None)
                and (config.previous, config.previous_release_id) != (previous, prior_id)):
            raise PublishError("Interrupted exact beta candidate has another prepared baseline.")
        verify_local(client, stage, files, complete=True)
        found = healthy(client, stage)
        verify_recovery_snapshot(client, stage, found)
        if client.request("GET", release_path("beta"), missing=True) is not None or tag_commit(client, "beta", missing=True) is not None:
            raise PublishError("Beta changed during exact candidate recovery; restart with fresh state.")
        primary_source(config)
        print("Recovered the exact interrupted beta candidate after ref removal.")
        return replace(config, previous=previous, previous_release_id=prior_id)
    proofs = []
    for stage in listing(client, "/releases"):
        identity, cutover = owned(stage), cutover_identity(stage)
        if not identity or identity[0] != "beta" or not STAGE.fullmatch(stage.get("tag_name", "")) or stage.get("draft") or cutover is None:
            continue
        previous, prior_id, recorded_orphan = cutover
        candidate = identity[1]
        if orphan not in (candidate, recorded_orphan):
            continue
        values = stage.get("assets")
        expected = expected_names("beta", "", candidate) | {"SHA256SUMS"}
        if not isinstance(values, list) or len(values) != len(expected):
            continue
        metadata = {value.get("name"): value for value in values}
        if set(metadata) != expected or any(not isinstance(value.get("size"), int)
                or not 0 < value["size"] <= (MAX_MANIFEST if name == "SHA256SUMS" else MAX_ASSET)
                for name, value in metadata.items()):
            continue
        try:
            result = subprocess.run(["git", "merge-base", "--is-ancestor", candidate, config.commit],
                cwd=ROOT, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, timeout=30, check=False)
        except (OSError, subprocess.SubprocessError):
            raise PublishError("Cannot verify interrupted beta staging ancestry.") from None
        if result.returncode != 0:
            continue
        if not isinstance(stage.get("id"), int) or stage["id"] <= 0:
            continue
        proofs.append((stage, previous, prior_id))
        if len(proofs) > 20:
            raise PublishError("Too many interrupted beta cutover proofs; inspect retained state.")
    if not proofs or len({(previous, prior_id) for _, previous, prior_id in proofs}) != 1:
        raise PublishError("Unattached beta ref has no unambiguous owned cutover proof; left unchanged.")
    stage, previous, prior_id = max(proofs, key=lambda proof: proof[0]["id"])
    if ((config.previous or config.previous_release_id is not None)
            and (config.previous, config.previous_release_id) != (previous, prior_id)):
        raise PublishError("Interrupted beta proof differs from the prepared baseline.")
    found = healthy(client, stage)
    manifest_digest = client.download(found["SHA256SUMS"], MAX_MANIFEST, digest=True)
    if not stage["tag_name"].endswith("-" + manifest_digest[:16]):
        raise PublishError("Interrupted beta staging checksum identity differs from its files.")
    verify_recovery_snapshot(client, stage, found)
    # Re-read after potentially large verification downloads. Recovery must not
    # consume a ref or release that another publisher restored in the meantime.
    if client.request("GET", release_path("beta"), missing=True) is not None or tag_commit(client, "beta", missing=True) != orphan:
        raise PublishError("Beta changed during recovery verification; restart with fresh state.")
    primary_source(config)
    print("Recovered an interrupted beta cutover from verified staging downloads.")
    return replace(config, previous=previous, previous_release_id=prior_id, recovery_orphan=orphan)


def assets(client, release):
    values = listing(client, f"/releases/{release['id']}/assets")
    names = [value.get("name") for value in values]
    if any(not isinstance(name, str) for name in names) or len(set(names)) != len(names):
        raise PublishError("Release asset identities are invalid or duplicated.")
    return {value["name"]: value for value in values}


def verify_local(client, release, files, complete=False):
    found = assets(client, release)
    if not set(found) <= set(files) or (complete and set(found) != set(files)):
        raise AssetMismatch("Release contains unexpected or missing assets; left unchanged.")
    for name, value in found.items():
        path, digest = files[name]
        if value.get("size") != path.stat().st_size or not hmac.compare_digest(client.download(value, MAX_ASSET, digest=True), digest):
            raise AssetMismatch("Uploaded asset differs from its exact local payload.")
    return found


def healthy(client, release):
    identity = owned(release)
    if not identity or release.get("draft"):
        raise PublishError("Retention requires a published Galaxy-owned replacement.")
    channel, commit = identity
    tag = release["tag_name"]
    check_tag(client, tag, commit)
    expected = expected_names(channel, tag, commit)
    found = assets(client, release)
    if set(found) != expected | {"SHA256SUMS"}:
        raise PublishError("Replacement release is missing a platform payload or its combined checksums.")
    raw = client.download(found["SHA256SUMS"], MAX_MANIFEST)
    lines = raw.decode("ascii").splitlines()
    checksums = {}
    for line in lines:
        match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9_.-]+)", line)
        if not match or match[2] not in expected or match[2] in checksums:
            raise PublishError("Replacement checksum manifest is invalid.")
        checksums[match[2]] = match[1]
    if set(checksums) != expected:
        raise PublishError("Replacement checksums do not cover all platforms.")
    for name in expected:
        if not hmac.compare_digest(client.download(found[name], MAX_ASSET, digest=True), checksums[name]):
            raise PublishError("Replacement public payload failed checksum verification.")
    return found


def body_for(config, notes):
    marker = (f"<!-- galaxy-beta:v1 commit={config.commit} -->" if config.channel == "beta"
              else f"<!-- galaxy-release:v1 tag={config.version} commit={config.commit} -->")
    return (notes.strip() + "\n\nDownloads: signed/notarized Apple Silicon macOS DMG, Windows installer, "
            "and Linux Debian package/portable archive. Verify files with SHA256SUMS.\n\n"
            + f"Source commit: `{config.commit}`\n\n{marker}")


def draft(client, config, tag, files, body):
    release = client.request("GET", release_path(tag), missing=True)
    if release is None:
        release = client.request("POST", "/releases", body={"tag_name": tag,
            "target_commitish": config.commit, "name": "Galaxy Beta" if config.channel == "beta" else "Galaxy " + config.version,
            "body": body, "draft": True, "prerelease": config.channel == "beta", "make_latest": "false"})
    require_owned(release, config.channel, config.commit, tag)
    if config.channel == "beta":
        planned, recorded = cutover_identity({"body": body}), cutover_identity(release)
        if planned is None or recorded is None or planned[:2] != recorded[:2]:
            raise PublishError("Existing beta staging record has a different cutover baseline; left unchanged.")
    found = verify_local(client, release, files)
    if not release.get("draft") and set(found) != set(files):
        raise PublishError("An already-published release is incomplete; it will not be overwritten.")
    for name, (path, _) in files.items():
        if name not in found:
            client.request("POST", f"/releases/{release['id']}/assets?name=" + urllib.parse.quote(name, safe=""), upload=path)
    verify_local(client, release, files, complete=True)
    return client.request("GET", f"/releases/{release['id']}")


def delete_owned(client, release):
    identity = owned(release)
    if not identity:
        raise PublishError("Cannot remove an unrecognized release.")
    current = client.request("GET", f"/releases/{release['id']}", missing=True)
    if current is None:
        return
    if owned(current) != identity or current.get("tag_name") != release["tag_name"] or current.get("draft") != release.get("draft"):
        raise PublishError("Release changed before retention; left unchanged.")
    channel, commit = identity
    if not set(assets(client, current)) <= expected_names(channel, release["tag_name"], commit) | {"SHA256SUMS"}:
        raise PublishError("Release has unrecognized assets; retention stopped.")
    before = tag_commit(client, release["tag_name"], missing=True)
    if before is not None and before != commit:
        raise PublishError("Old release tag changed before retention.")
    client.request("DELETE", f"/releases/{release['id']}")
    if client.request("GET", f"/releases/{release['id']}", missing=True) is not None:
        raise PublishError("Release removal was not confirmed.")
    if before is not None:
        check_tag(client, release["tag_name"], commit)


def delete_beta_tag(client, tag, commit):
    if tag != "beta" and not STAGE.fullmatch(tag):
        raise PublishError("Only rolling beta or staging refs may be removed.")
    value = tag_commit(client, tag, missing=True)
    if value is None:
        return
    if value != commit or client.request("GET", release_path(tag), missing=True) is not None:
        raise PublishError("Beta ref is changed or still owns a release; left unchanged.")
    client.request("DELETE", "/git/refs/tags/" + urllib.parse.quote(tag, safe=""))
    if tag_commit(client, tag, missing=True) is not None:
        raise PublishError("Beta ref deletion was not confirmed.")


def version_key(value):
    return tuple(map(int, value.split(".")))


def retain(client):
    releases = listing(client, "/releases")
    stable = [release for release in releases if owned(release) and owned(release)[0] == "release" and not release.get("draft")]
    latest = max(stable, key=lambda value: version_key(value["tag_name"])) if stable else None
    beta = client.request("GET", release_path("beta"), missing=True)
    if latest:
        healthy(client, latest)
    if beta:
        healthy(client, beta)
    for release in releases:
        identity = owned(release)
        if not identity:
            continue
        if latest and identity[0] == "release" and version_key(release["tag_name"]) < version_key(latest["tag_name"]):
            delete_owned(client, release)
        elif beta and identity[0] == "beta" and STAGE.fullmatch(release["tag_name"]):
            delete_owned(client, release)
            delete_beta_tag(client, release["tag_name"], identity[1])


def notes_for(config, client):
    if config.notes:
        return config.notes
    baseline = config.previous
    if config.channel == "release":
        content = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8")
        match = re.search(r"(?ms)^## \[" + re.escape(config.version) + r"\][^\n]*\n(.*?)(?=^## |\Z)", content)
        if match and match[1].strip():
            return match[1].strip()
        stable = [r for r in listing(client, "/releases") if owned(r) and owned(r)[0] == "release"
                  and not r.get("draft") and version_key(r["tag_name"]) < version_key(config.version)]
        if stable:
            baseline = owned(max(stable, key=lambda value: version_key(value["tag_name"])))[1]
    command = ["git", "log", "--format=%h %s", "--max-count=201", f"{baseline}..{config.commit}" if baseline else config.commit, "--"]
    try:
        rows = subprocess.check_output(command, cwd=ROOT, stderr=subprocess.PIPE, text=True, timeout=30).splitlines()
    except (OSError, subprocess.SubprocessError):
        raise PublishError("Could not generate bounded release notes.") from None
    lines, total = [], 0
    for row in rows[:200]:
        line = "- " + re.sub(r"[\x00-\x1f\x7f]", " ", row)[:500]
        total += len(line.encode("utf-8")) + 1
        if total > 96 * 1024:
            break
        lines.append(line)
    notes = "\n".join(lines) or "Manually rebuilt with no source changes."
    if len(rows) > len(lines):
        notes += f"\n\nShowing the latest {len(lines)} commits within the release-note limit."
    if baseline:
        notes += f"\n\n[Compare changes](https://github.com/{config.repository}/compare/{baseline}...{config.commit})"
    return notes


def publish_stable(config, client, files, notes):
    def current():
        if any(owned(r) and owned(r)[0] == "release" and not r.get("draft")
               and version_key(r["tag_name"]) > version_key(config.version) for r in listing(client, "/releases")):
            raise PublishError("A newer stable release is published; stale publication stopped.")
        primary_source(config)
        check_tag(client, config.tag, config.commit)
    current()
    release = draft(client, config, config.tag, files, body_for(config, notes))
    current()
    if release.get("draft"):
        release = client.request("PATCH", f"/releases/{release['id']}", body={"draft": False, "make_latest": "true"})
    require_owned(release, "release", config.commit, config.tag)
    verify_local(client, release, files, complete=True)
    healthy(client, release)
    current()
    retain(client)


def beta_baseline(config, client):
    primary_source(config)
    if client.request("GET", "/branches/master").get("commit", {}).get("sha") != config.commit:
        raise PublishError("GitHub master advanced during this beta build; current beta left intact.")
    release = client.request("GET", release_path("beta"), missing=True)
    if release and (owned(release) != ("beta", config.previous) or release.get("draft")):
        raise PublishError("Published beta changed since this build began.")
    if release and config.previous_release_id is not None and release.get("id") != config.previous_release_id:
        raise PublishError("Beta was replaced after preparation, including a rebuild of the same source.")
    return release


def publish_beta(config, client, files, notes):
    stage_tag = f"beta-stage-{config.commit}-{files['SHA256SUMS'][1][:16]}"
    existing = client.request("GET", release_path("beta"), missing=True)
    if existing and owned(existing) == ("beta", config.commit) and not existing.get("draft"):
        try:
            verify_local(client, existing, files, complete=True)
        except AssetMismatch:
            if config.previous != config.commit:
                raise PublishError("Another beta for this source is already published.") from None
        else:
            healthy(client, existing)
            delete_beta_tag(client, stage_tag, config.commit)
            retain(client)
            return
    baseline = beta_baseline(config, client)
    orphan_before = tag_commit(client, "beta", missing=True)
    if orphan_before not in (None, config.previous or None, config.commit, config.recovery_orphan or None):
        raise PublishError("Rolling beta ref has no prepared or verified ownership; left unchanged.")
    if baseline is not None and orphan_before != config.previous:
        raise PublishError("Published beta ref differs from its prepared source; left unchanged.")
    body = body_for(config, notes)
    prior_id = baseline.get("id") if baseline else config.previous_release_id
    if bool(config.previous) != (prior_id is not None):
        raise PublishError("Cannot persist the exact previous beta identity before cutover.")
    cutover = (f"<!-- galaxy-beta-cutover:v1 previous={config.previous or 'none'} "
               f"release={prior_id if prior_id is not None else 'none'} orphan={orphan_before or 'none'} -->")
    body += "\n\n" + cutover
    stage = draft(client, config, stage_tag, files, body)
    recorded = cutover_identity(stage)
    if recorded is None or recorded[:2] != (config.previous, prior_id):
        raise PublishError("Staging release does not retain this exact beta cutover; left unchanged.")
    # Keep an existing proof intact across retries after the old ref was
    # removed or the exact new alias was created but promotion was interrupted.
    # Current ref ownership was independently checked before staging above.
    body = stage["body"]
    if stage.get("draft"):
        stage = client.request("PATCH", f"/releases/{stage['id']}", body={"draft": False, "make_latest": "false"})
    require_owned(stage, "beta", config.commit, stage_tag)
    verify_local(client, stage, files, complete=True)
    healthy(client, stage)
    latest = beta_baseline(config, client)
    if (baseline or {}).get("id") != (latest or {}).get("id"):
        raise PublishError("Beta changed while staging; replacement stopped.")
    if tag_commit(client, "beta", missing=True) != orphan_before:
        raise PublishError("Beta ref changed while staging; existing release left intact.")
    if latest:
        delete_owned(client, latest)
    old_tag = tag_commit(client, "beta", missing=True)
    if old_tag not in (None, orphan_before, config.commit):
        raise PublishError("Rolling beta tag changed unexpectedly; cutover stopped.")
    if old_tag and old_tag != config.commit:
        delete_beta_tag(client, "beta", old_tag)
    if tag_commit(client, "beta", missing=True) is None:
        client.request("POST", "/git/refs", body={"ref": "refs/tags/beta", "sha": config.commit})
    check_tag(client, "beta", config.commit)
    stage = client.request("GET", f"/releases/{stage['id']}")
    require_owned(stage, "beta", config.commit, stage_tag)
    release = client.request("PATCH", f"/releases/{stage['id']}", body={"tag_name": "beta",
        "target_commitish": config.commit, "name": "Galaxy Beta", "body": body,
        "draft": False, "prerelease": True, "make_latest": "false"})
    require_owned(release, "beta", config.commit, "beta")
    verify_local(client, release, files, complete=True)
    healthy(client, release)
    delete_beta_tag(client, stage_tag, config.commit)
    retain(client)


def main():
    try:
        config = Config.from_env(os.environ)
        files = release_files(config)
        client = GitHub(config)
        if config.channel == "beta":
            config = recover_beta(config, client, files)
        notes = notes_for(config, client)
        if not notes.strip() or len(notes.encode("utf-8")) > 128 * 1024:
            raise PublishError("Release notes must be nonempty and bounded.")
        (publish_beta if config.channel == "beta" else publish_stable)(config, client, files, notes)
        url = "https://github.com/" + config.repository + "/releases/tag/" + config.tag
        print("Verified GitHub release published: " + url)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
                output.write("release_url=" + url + "\n")
        return 0
    except PublishError as error:
        print("Galaxy GitHub publication failed: " + str(error), file=sys.stderr)
    except (OSError, ValueError, TypeError, KeyError, UnicodeError):
        print("Galaxy GitHub publication failed; retained release state requires inspection.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
