# Galaxy native releases

Primary source remains <https://git.ryserve.net/public/galaxy>. The Gitea **Sync
GitHub** job copies branches and immutable version tags to
<https://github.com/rkw6086/galaxy>. GitHub Actions builds and publishes current
installers through [Build Galaxy](../.github/workflows/build.yml).

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

## Nightly Beta and tagged releases

Beta runs at **3:00 a.m. America/Chicago**, including daylight saving time, only
when `master` differs from the published Beta or its complete download set is
missing. GitHub schedules can be delayed by runner demand. A manual **Build
Galaxy** run on `master` rebuilds Beta on demand, including an unchanged source.
Beta About displays its source commit. Names include `beta_FULL_COMMIT_SHA`.

A pushed bare Semantic Version tag, such as `3.1.5`, starts a stable build after
Gitea synchronizes the tag to GitHub. Create a version newer than every retained
numeric tag and point it to source already in `master`:

```bash
node script/release.mjs tag 3.1.5 --push
```

The command checks and pushes the immutable tag to both `samsung` and `origin`.
The GitHub workflow stages the selected version in each build checkout without
moving the tag or creating a source version commit. Release About displays that
version. Original source manifests remain internally consistent; `prepare` is
available when a committed source version bump is wanted.

Pull requests targeting `master` build all three platforms without signing or
publication. Fork builds receive no Apple credentials. The source, tag, and
Gitea primary ref are validated before trusted Beta or stable signing.

## Signing, publication and retention

All compilation runs without signing credentials. The macOS signing step alone
receives `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY`, `APPLE_TEAM_ID`, `APPLE_API_KEY`, `APPLE_API_ISSUER`, and
`APPLE_API_KEY_P8` from GitHub repository Actions secrets. It imports a temporary
keychain, signs nested code, notarizes and staples both app and DMG, verifies
Gatekeeper, and removes its private key material. The credentials never enter
artifacts or Git history.

Native platform jobs run concurrently. Publication waits for all three, validates
each platform checksum, creates one combined `SHA256SUMS`, uploads a draft, and
verifies remote bytes before exposing the complete release. A Beta uses a healthy
staged release before replacing the previous Beta. Failed builds preserve the
previous downloads.

Keep one rolling **Beta** and one latest **stable** release on GitHub. Replace
older release records and assets only after a healthy replacement. Numeric Git
tags are retained. GitHub immutable-release enforcement must remain off for the
rolling Beta policy. Old Gitea downloads remain separate historical fallbacks;
retired Gitea macOS workflows live under `.gitea/retired-workflows/` and do not run.

Transfer artifacts expire after one day and are deleted after successful
publication. The workflow uses standard hosted runners, not billed larger
runners. Standard public-repository compute is free; per-job runtime limits and
artifact/cache storage allowances still apply. Build parallelism is capped and
release debug information is omitted to fit hosted runner resources.

Local validation remains `cargo build` only. The development machine does not
launch the application; native CI validates build/package/signing behavior, and
interactive installation and application behavior are checked on target devices.
