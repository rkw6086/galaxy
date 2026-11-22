#!/usr/bin/env python3
"""Create Galaxy's Finder layout without opening Finder or running AppleScript.

The pure-Python dmgbuild tools author .DS_Store and background aliases directly.
See https://dmgbuild.readthedocs.io/en/v1.6.7/settings.html for layout settings.
"""

import argparse
import hashlib
import importlib.metadata
import os
from pathlib import Path
import plistlib
import subprocess
import sys


def file_hash(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.digest()


def bundle_manifest(bundle):
    manifest = {}
    for path in bundle.rglob("*"):
        relative = str(path.relative_to(bundle))
        if path.is_symlink():
            manifest[relative] = ("symlink", os.readlink(path))
        elif path.is_file():
            manifest[relative] = ("file", path.stat().st_size, path.stat().st_mode & 0o111)
        elif path.is_dir():
            manifest[relative] = ("directory",)
        else:
            raise RuntimeError(f"Unsupported item in application bundle: {relative}")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--volume-name", default="Galaxy")
    parser.add_argument("--verify-signature", action="store_true")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("DMG creation requires macOS")
    for package, expected in [("dmgbuild", "1.6.7"), ("ds-store", "1.3.1"), ("mac-alias", "2.2.2")]:
        if importlib.metadata.version(package) != expected:
            parser.error(f"Use the pinned {package} {expected} packaging tool")
    from dmgbuild import build_dmg

    app = args.app.resolve(strict=True)
    output = args.output.absolute()
    if not app.is_dir() or app.suffix != ".app":
        parser.error("--app must be a complete application bundle")
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        parser.error("--output requires a fresh path in an existing directory")
    with (app / "Contents/Info.plist").open("rb") as stream:
        executable = plistlib.load(stream)["CFBundleExecutable"]
    executable_path = Path("Contents/MacOS") / executable
    if Path(executable).name != executable or not (app / executable_path).is_file():
        parser.error("Application bundle has no valid main executable")
    source_hash = file_hash(app / executable_path)
    source_manifest = bundle_manifest(app)
    artwork = Path(__file__).resolve().parent / "dmg/background.png"

    # dmgbuild 1.6.7 uses ditto to preserve resource forks, permissions and seals.
    # Verify the staged app before sealing the image, including a failed/partial
    # ditto copy (that version does not check ditto's return code itself).
    def verify_copy(mount_point, _settings):
        copied = Path(mount_point) / app.name
        if bundle_manifest(copied) != source_manifest or file_hash(copied / executable_path) != source_hash:
            raise RuntimeError("The application bundle changed while copying to the DMG")
        if args.verify_signature:
            subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(copied)], check=True)

    settings = {
        "format": "UDZO",
        "compression_level": 9,
        "filesystem": "APFS",
        "files": [str(app)],
        "symlinks": {"Applications": "/Applications"},
        "background": str(artwork),
        "window_rect": ((180, 160), (700, 500)),
        "default_view": "icon-view",
        "include_icon_view_settings": True,
        "show_status_bar": False,
        "show_tab_view": False,
        "show_toolbar": False,
        "show_pathbar": False,
        "show_sidebar": False,
        "arrange_by": None,
        "grid_spacing": 80,
        "icon_size": 112,
        "text_size": 14,
        "icon_locations": {app.name: (170, 250), "Applications": (530, 250)},
        "create_hook": verify_copy,
    }
    try:
        build_dmg(str(output), args.volume_name, settings=settings)
    except BaseException:
        output.unlink(missing_ok=True)
        raise
    print(f"Created Galaxy installer with Retina artwork and drag-to-Applications layout: {output.name}")


if __name__ == "__main__":
    main()
