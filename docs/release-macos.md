# Galaxy macOS release channels

Galaxy keeps two public download releases: **Galaxy Beta** and the latest numbered stable release. Both contain a Developer ID signed, notarized, stapled Apple Silicon DMG and `SHA256SUMS`.

## Nightly and manual beta

[macOS beta](../.gitea/workflows/macos-beta.yml) runs at **3:00 a.m. America/Chicago**, including daylight saving time, using Gitea's `CRON_TZ=America/Chicago 0 3 * * *` schedule. It builds only when current `master` differs from the published beta. Missing beta assets also permit rebuilding. Start the same workflow manually on `master` to build on demand, including an unchanged commit.

The workflow explicitly checks out current `master` and validates its SHA against the live repository API. This avoids Gitea's scheduled context sometimes retaining an older workflow SHA. Branch pushes and pull requests do not start a signed beta build.

Download [Galaxy Beta](https://git.ryserve.net/public/galaxy/releases/tag/beta). Its notes list commits since the previous successfully published beta, with a comparison link. The first rolling release uses the last successful per-push beta artifact as its notes baseline when available. Long lists show the latest 200 commits explicitly. Failed builds do not advance that baseline. A manual rebuild without source changes says so.

About Galaxy shows `Beta · COMMIT`; the DMG is `Galaxy_beta_COMMIT_aarch64.dmg`, using the full source commit. A successful replacement updates the same `beta` download channel and removes its previous assets. The rolling beta tag is mutable; numbered version tags remain permanent.

## Tagged stable releases

[macOS release](../.gitea/workflows/macos-release.yml) starts automatically when a new bare `MAJOR.MINOR.PATCH` tag is pushed, such as `3.1.4`. Use a version newer than all existing version tags. It must identify a commit already in `master`.

After the required local build, commit and push source to both remotes, then create the release tag from `master`:

```sh
node script/release.mjs tag 3.1.4 --push
```

The tag selects the packaged app version automatically. CI stages only the Galaxy package version in `app/Cargo.toml` and its matching entry in `Cargo.lock`; it verifies those exact edits against the immutable tagged source before building and again before signing. It does not rewrite the tag or commit those build-time edits to `master`. About shows `Version 3.1.4`, and the DMG is `Galaxy_3.1.4_aarch64.dmg`.

A matching version section in `CHANGELOG.md` supplies release notes when present. Otherwise the publisher generates bounded notes from commits since the previous stable release. `node script/release.mjs prepare VERSION` remains available when you want to update the source manifests before tagging, but is optional.

The replacement is published and its remote downloads verified before older numbered release records and their assets are deleted. Their Git tags remain available for source history. Reused, moved, lower, or stale version tags cannot replace a newer release.

## Publication, retention, and recovery

Both workflows build without Apple credentials, then sign nested native code, notarize and staple the app, create the DMG, and sign, notarize, staple, and verify the DMG with Gatekeeper. Bundled resources include the prebuilt Wormhole archives. Apple submissions each wait up to 90 minutes; failure never publishes an unsigned fallback.

Publishing runs directly on the native runner after signing, so large DMGs are not duplicated in Actions artifact storage. Only the DMG and checksum become public release assets. Job-owned build files, tooling, notes, and signing material are removed when the job exits. Build and signing credentials remain in their respective steps.

The publisher uploads a draft and downloads its assets to check their hashes before publication. A beta replacement uses an owned staging prerelease so its new download is healthy before retiring the old beta. Gitea cannot move the release and tag atomically, so the stable `beta` URL can briefly be absent during cutover. The verified staging download remains available during that interval. After success, staging release records and old beta assets are removed; tiny internal staging Git refs may remain when Gitea cannot delete an unindexed ref through its tag API. They retain no download assets.

After both current channels have verified downloads, retention also removes superseded Galaxy macOS Actions artifacts from completed native release/beta/recovery runs. Gitea marks these archives for deletion and reclaims their stored bytes through its background cleanup; expired archives are already unavailable and stay under that cleanup worker. Retention skips active runs and unrelated artifacts or releases. The precisely identified empty failed `3.1.0` draft is retired during this migration; its version tag remains. Normal completed state is one beta release and one stable release, with one DMG/checksum pair each. A failed or uncertain API write retains recoverable state and stops; retries reconcile exact owned release identities and assets before continuing.

Keep the existing native `macos-arm64:host` runner at capacity **1**. Building, publication, and retention stay in the same job, serializing both channels and preventing another native publication from interleaving with a replacement. No global runner cache or unrelated app data is removed.

## Runner and credentials

The runner needs full Xcode with its license accepted, selected command-line tools, Metal tools, Git, Python 3, Node, rustup, and Homebrew. The shared scripts select Node `24.17.0`, Rust `1.97.1`, target `aarch64-apple-darwin`, cargo-bundle `0.11.0` at its pinned commit, and cargo-about `0.8.4`. Missing Homebrew `cmake`, `pkgconf`, and `llvm` and a missing Metal toolchain are installed during unsigned setup. Build directories and tools are job-owned.

The existing organization secrets remain unchanged:

| Secret | Purpose |
| --- | --- |
| `APPLE_CERTIFICATE` | Base64 Developer ID Application P12 with private key |
| `APPLE_CERTIFICATE_PASSWORD` | P12 password |
| `APPLE_SIGNING_IDENTITY` | Full Developer ID Application identity |
| `APPLE_TEAM_ID` | Matching Apple team ID |
| `APPLE_API_KEY` | Notarization API key ID |
| `APPLE_API_ISSUER` | Notarization issuer ID |
| `APPLE_API_KEY_P8` | Base64 notarization private key |

Signing temporarily uses a private keychain and restores the runner's prior keychain lists on exit. The short-lived workflow token is supplied only to preflight and publication steps. Publication needs release writes and Actions artifact deletion; beta also needs permission to replace its rolling/staging tags. Native hosts remain restricted to trusted repositories, protected `master`, and version tags.

[Release checks](../.gitea/workflows/release-checks.yml) validate metadata and script syntax on master pushes and pull requests. A manual run on `master` additionally builds and verifies the complete unsigned macOS bundle without signing or publication. Local verification remains `cargo build` only; the development machine does not launch the app.

Install either DMG by dragging `Galaxy.app` to Applications. Beta and stable share the same app identifier, local settings, and conversations. Install a stable DMG again to return from beta. The legacy `script/install-galaxy.sh` uses ad-hoc signing for unsigned local builds and must not replace the signature of a Developer ID signed download.
