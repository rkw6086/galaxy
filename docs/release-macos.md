# Galaxy macOS releases

[The macOS release workflow](../.gitea/workflows/macos-release.yml) adapts Solar's signed release jobs to Galaxy's native Rust bundle. A pushed bare version tag, such as `3.1.0`, builds Galaxy on the existing `macos-arm64` runner and publishes an Apple Silicon DMG at [Gitea Releases](https://git.ryserve.net/public/galaxy/releases).

The build and signing steps are separate. The build receives no Apple secrets. Packaging signs nested native code before the app, notarizes and staples the app, creates a DMG containing `Galaxy.app` and an Applications shortcut, and then signs, notarizes, staples, and verifies the DMG. The verified DMG and `SHA256SUMS` are uploaded as an Actions artifact before a separate Linux job publishes them.

## Runner and credentials

Use the same native Apple Silicon runner setup as Solar, registered as `macos-arm64:host`, with capacity 1 under its dedicated runner account. Signing temporarily adds a private keychain to that account's search list and restores the original lists on exit. Keep the host runner restricted to trusted repositories and protect `master` and version tags.

The runner needs full Xcode with its license accepted, command-line tools selected, the Metal toolchain, Git, Python 3, Node for JavaScript Actions, Rust through rustup, and Homebrew. The workflow selects Node `24.17.0`, Rust `1.97.1`, and target `aarch64-apple-darwin`. It installs missing Homebrew `cmake`, `pkgconf`, and `llvm`, then installs cargo-bundle `0.12.0` at its pinned release commit and `cargo-about` `0.8.4` into a job-owned tools directory. Build and artifact directories are also job-owned and removed after upload. If the selected Xcode is missing its Metal compiler, unsigned setup downloads the Metal toolchain and verifies it before building. The runner must reach Gitea, the pinned Actions sources, dependency registries, bundled-resource downloads, and Apple's notarization services.

These existing organization Actions secrets must be available to `public/galaxy`:

| Secret | Purpose |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64 Developer ID Application `.p12`, including the private key |
| `APPLE_CERTIFICATE_PASSWORD` | Export password for the `.p12` |
| `APPLE_SIGNING_IDENTITY` | Full Developer ID Application identity |
| `APPLE_TEAM_ID` | Apple team ID matching that identity |
| `APPLE_API_KEY` | App Store Connect team API key ID |
| `APPLE_API_ISSUER` | Team API key issuer ID |
| `APPLE_API_KEY_P8` | Base64 notarization API private key |

The names match Solar's secrets. The same Apple identity can sign both apps; their app identifiers remain separate. Repository secrets override organization secrets, so remove obsolete overrides when rotating credentials. Never print credential values or commit them. The workflow uses Gitea's short-lived `GITEA_TOKEN` for release preflight and publication, with release-write permission only in the publishing job.

## Version and publish

The tag is the packaged version: `3.1.0`, with no `v` prefix. Only stable `MAJOR.MINOR.PATCH` versions are supported. About Galaxy displays that packaged version without commit details.

Prepare a version and its release notes, then check the files:

```sh
node script/release.mjs prepare 3.1.0
node script/release.mjs check
```

Keep a nonempty `## [3.1.0]` section in `CHANGELOG.md`. Complete the required local `cargo build`, commit the release changes, and push `master` to both configured remotes. Create and push the annotated tag with `node script/release.mjs tag 3.1.0 --push` after both remote branches contain that commit. The push to Gitea (`origin`) starts the signed release workflow; the Samsung remote retains the same source and tag.

Branch pushes and pull requests only check release metadata and script syntax, without Apple credentials. Before creating a release tag, manually run the **Release checks** workflow on `master` for a native macOS preflight. That manual job builds and verifies the complete unsigned bundle using the same pinned tools and resources as the release, then removes its temporary files. It has no Apple secrets, signing, publication, or app launch. A manual run on another branch cannot start the macOS job. A signing job requires a pushed version tag whose packaged version matches, whose commit is in `master`, and whose local, triggering, and remote tag commits agree. It checks previous Actions history for a reused tag pointing to another commit. Existing published versions skip signing. Treat release tags as permanent and protect them in Gitea; corrections belong in a new version.

## Artifacts and recovery

A successful release contains `Galaxy_VERSION_aarch64.dmg` and `SHA256SUMS`, with release notes from `CHANGELOG.md`. The Actions artifact is named `Galaxy-VERSION-macos-arm64`, retained for 14 days, and may also contain notarization diagnostic files. Only the DMG and checksum file become public release attachments.

The publisher first verifies local checksums, the remote tag commit, and any existing draft or attachments. It creates a draft, uploads the files, verifies their remote hashes, and publishes only after every check passes. Retrying publication resumes the matching draft; it never replaces a differing attachment or edits an already published release. A publish failure can be retried using the retained verified artifact without changing the tag or signing a different build.

Apple submissions each wait up to 90 minutes. A rejected or incomplete notarization fails the job and does not publish a distributable. The package scripts verify signatures, hardened runtime, notarization tickets, and Gatekeeper assessments before upload. Download and exercise the resulting app on macOS to verify terminal behavior, permissions, and bundled resources. This initial job builds Apple Silicon only.

Install a signed release by mounting its DMG and dragging Galaxy to Applications. The legacy `script/install-galaxy.sh` path performs ad-hoc signing and quarantine removal for local unsigned builds; do not run it against the Developer ID signed release because that would replace the release signature.
