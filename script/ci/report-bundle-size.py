#!/usr/bin/env python3
"""Record the unsigned macOS app's shipped bytes, excluding symlink targets."""

import argparse
import json
import os
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bundle", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    bundle = args.bundle.resolve(strict=True)
    if not bundle.is_dir() or bundle.suffix != ".app":
        parser.error("bundle must be an app directory")

    files = []
    for directory, _, names in os.walk(bundle, followlinks=False):
        for name in names:
            item = Path(directory) / name
            if item.is_symlink() or not item.is_file():
                continue
            files.append({"path": item.relative_to(bundle).as_posix(), "bytes": item.stat().st_size})
    files.sort(key=lambda item: (-item["bytes"], item["path"]))
    groups = {}
    for item in files:
        parts = item["path"].split("/")
        group = "/".join(parts[:2])
        groups[group] = groups.get(group, 0) + item["bytes"]
    total = sum(item["bytes"] for item in files)
    report = {
        "stage": "unsigned_app_before_signing",
        "logical_bytes": total,
        "file_count": len(files),
        "groups": dict(sorted(groups.items())),
        "largest_files": files[:20],
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Unsigned Galaxy.app: {total:,} bytes ({total / 1024**2:.1f} MiB), {len(files)} files")
    lines = [
        "### macOS app size before signing",
        "",
        f"**{total / 1024**2:.1f} MiB** of file contents; excludes filesystem overhead and symlink targets.",
        "",
        "| Largest shipped files | MiB |",
        "| --- | ---: |",
    ]
    for item in files[:10]:
        lines.append(f"| `{item['path']}` | {item['bytes'] / 1024**2:.1f} |")
    lines.append("")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as stream:
            stream.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
