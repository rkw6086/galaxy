# Galaxy native releases

GitHub Actions builds and publishes official installers through
[Build Galaxy](../.github/workflows/build.yml).

## Supported downloads

| Platform | Standard hosted runner | Downloads |
| --- | --- | --- |
| Windows x64 | `windows-2025` | Inno Setup `*-setup.exe` |
| Linux x64 | `ubuntu-24.04` | `.deb` and `.tar.gz` |
| Apple Silicon macOS | `macos-15` (ARM64) | Signed, notarized `.dmg` |

Windows installers are unsigned until a separate Windows code-signing identity
is configured. Apple Developer credentials apply to macOS only. Linux tarballs
include Galaxy and its resources and require the normal system libraries; the
Debian package declares its runtime dependencies. Packages include the retained
Wormhole helper archives. Linux installation does not add an upstream Warp
package repository or signing key.

macOS installers include a Galaxy background and an app-to-Applications layout.
The headless packaging tools create the Finder layout without opening Finder.
Release executables are stripped before signing; see the
[build size report](build-size.md) for measurements and remaining opportunities.

## Nightly and tagged releases

Nightly runs every day at **4:00 a.m. America/Chicago**, including daylight saving
time, even when `master` has not changed. GitHub schedules can be delayed by
runner demand. A manual **Build Galaxy** run on `master` also creates a Nightly.
One rolling **Nightly** release contains the newest complete downloads and changes
since the previous Nightly. About displays its source commit. Download names
use a readable Chicago date/time and workflow sequence, such as
`Galaxy_Nightly_11202026_040000_CT_r8-1_aarch64.dmg`. The `r8-1` suffix identifies
the workflow run number and preparation attempt, so separate builds never reuse
the same filenames. The timestamp comes from the trusted workflow creation time
and stays unchanged when failed jobs are retried. The complete source commit,
run ID and preparation attempt remain the internal build identity.
Each Nightly builds a specific source snapshot. Publication validates that
snapshot's provenance; stable releases also require their exact immutable tags.

A new bare Semantic Version tag on GitHub starts a stable build. Authorized
maintainers should choose a version newer than every retained numeric tag and
point it to source already in `master`. Version tags must not be moved or reused.

The GitHub workflow stages the selected version in each build checkout without
moving the tag or creating a source version commit. Release About displays that
version. Original source manifests remain internally consistent; `prepare` is
available when a committed source version bump is wanted.

Pull requests targeting `master` build all three platforms without signing or
publication. Fork builds receive no Apple credentials. The source and tag
identities are validated before trusted Nightly or stable signing.

The manual **Check Windows build** workflow validates Windows packaging
independently while other native builds run. It uses the same unsigned build
scripts and can retain the installer and checksum for one day, without publishing
a release. Full Nightly and tagged publication still wait for all three platforms.

## Signing, publication and retention

All compilation runs without signing credentials. The macOS signing step alone
receives `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY`, `APPLE_TEAM_ID`, `APPLE_API_KEY`, `APPLE_API_ISSUER`, and
`APPLE_API_KEY_P8` from GitHub repository Actions secrets. It imports a temporary
keychain, signs nested code, notarizes and staples both app and DMG, verifies
Gatekeeper, and removes its private key material. The credentials never enter
artifacts or Git history.

Native platform jobs run concurrently. Publication waits for all three, validates
each platform checksum and verifies uploaded bytes before exposing the complete
release. Stable downloads include a combined `SHA256SUMS`; Nightly uses a unique
`SHA256SUMS_Nightly_MMDDYYYY_HHMMSS_CT_rRUN_NUMBER-PREPARE_ATTEMPT`.
Each release also includes `Galaxy_Update_IDENTITY.json` and its raw Ed25519
signature, `Galaxy_Update_IDENTITY.sig`. Their hashes are included in the combined
checksums with the four installers/packages, giving seven public assets.
The signed JSON binds channel, base version, source commit, internal build ID,
readable artifact identity, build time and each platform payload's name, size
and SHA-256. `built_at` is the prepared workflow creation time in UTC, so metadata
and signature bytes remain identical on publication retries.

Only the publication step receives `GALAXY_UPDATE_SIGNING_KEY`, a base64-encoded
Ed25519 PKCS8 PEM key. Its derived public key must match the committed raw public
key in `resources/galaxy-update-ed25519.pub`. Signing uses a private temporary file
removed after use. The app trusts that public key and verifies the metadata
signature and selected payload before installing an update. Beta and Nightly
builds follow the rolling Nightly; stable builds follow published stable tags.
Unsigned pull-request builds and the independent Windows validation installer
are not official automatic-update builds. Existing versions without the updater
need one manual installation of an updater-enabled build.

A Nightly uploads its complete
new set alongside the previous files, verifies it, updates the source tag and
release notes, then removes only older owned Nightly assets. Failed builds
preserve the previous downloads, and interrupted publication can resume safely.
The first replacement also recognizes the original full-commit Nightly filenames
and provenance markers, allowing their verified retirement without changing the
rolling release record. Tagged releases retain their original assets forever.

Keep one rolling **Nightly** and every **tagged release**, including its assets.
The publisher never deletes release records or tagged release assets. Numeric
Git tags are retained. Previously purged releases are not recreated. GitHub
immutable-release enforcement must remain off for the rolling Nightly policy.

Transfer artifacts expire after one day and are deleted after successful
publication. The workflow uses standard hosted runners, not billed larger
runners. Standard public-repository compute is free; per-job runtime limits and
artifact/cache storage allowances still apply. Build parallelism is capped and
release debug information is omitted to fit hosted runner resources.

Native jobs reuse a bounded local sccache compiler cache (2 GiB per platform),
public Cargo downloads, and version-checked packaging tools. Cache identities
separate operating systems, architectures, runner images, toolchains, build
flags, and dependency inputs. Stable compiler paths allow reuse between runs;
application metadata, bundles, installers, signing, and notarization are rebuilt
and checked for the current source and channel. Cache snapshots are saved only
by successful trusted master Nightlies, before Apple credentials are available;
pull requests and tagged builds may restore compatible master caches. Cargo
credentials, signing material, and release payloads are excluded. Cache-service
or compiler-cache setup failures fall back to ordinary compilation. The first
run fills caches; later run statistics show actual reuse. If a wrapped Windows
build fails, it retries the same locked build once with the normal compiler,
retaining completed dependencies and requiring a successful final exit status.

Local validation remains `cargo build` only. The development machine does not
launch the application; native CI validates build/package/signing behavior, and
interactive installation and application behavior are checked on target devices.
