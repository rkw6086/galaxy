# Installation and updates

Use the official installers attached to [Galaxy GitHub Releases](https://github.com/rkw6086/galaxy/releases).

## Choose a release

| Track | Download page | About shows | Updates follow |
| --- | --- | --- | --- |
| Stable | [Latest stable release](https://github.com/rkw6086/galaxy/releases/latest) | Release version | Published stable tags |
| Nightly | [Rolling Nightly](https://github.com/rkw6086/galaxy/releases/tag/nightly) | Source commit | Rolling Nightly |

A Nightly is built from `master` at 4:00 a.m. America/Chicago each day, including when the source is unchanged. It can also be triggered manually. GitHub may delay scheduled starts. One rolling Nightly keeps the newest complete download set; tagged releases retain their history.

Nightly filenames include the Chicago date, time, and workflow sequence. For example, `Galaxy_Nightly_11202026_040000_CT_r8-1_aarch64.dmg` identifies a particular build rather than only a version number.

## Choose your package

| Computer | File | Notes |
| --- | --- | --- |
| Apple Silicon Mac | `*_aarch64.dmg` | Official app and DMG are signed, notarized, and stapled. |
| Linux x86-64, Debian/Ubuntu package install | `*_amd64.deb` | Installs the application, desktop launcher, icons, resources, and runtime dependency metadata. |
| Linux x86-64, manual portable install | `*_linux_x86_64.tar.gz` | Includes the executable and its `resources` directory; system libraries are still required. |
| Windows x64 | `*_x86_64-setup.exe` | Inno Setup installer. Windows code signing is not currently configured. |

The current release matrix builds Linux packages on Ubuntu 24.04, Mac packages on the `macos-15` ARM64 runner, and Windows packages on `windows-2025`. These runner names describe the build environment, not a promise that every other OS version is compatible. Current official downloads do not include Intel Mac, Linux ARM, or Windows ARM packages.

## Install on macOS

1. Download the Apple Silicon DMG from the chosen release.
2. Open the DMG and drag **Galaxy.app** to **Applications**.
3. Eject the disk image and open Galaxy from Applications.

Use the signed GitHub DMG for a normal installation.

## Install on Ubuntu or Debian

Download the `.deb`, then install that exact local file with APT. For example, from the directory containing a stable 4.0.0 package:

```bash
sudo apt install ./Galaxy_4.0.0_amd64.deb
```

Replace the example filename with the file you actually downloaded. Nightly filenames contain a longer date/time identity. APT can resolve the package's declared dependencies and may request administrator authentication.

The package is named `galaxy-oss`. It installs the executable under `/opt/galaxy/galaxy-oss/`, exposes a command through `/usr/bin/galaxy-oss`, and installs a desktop entry in `/usr/share/applications`. Look for Galaxy in the application launcher after installation. The Galaxy package does not add a Warp APT repository or signing key.

### Portable Linux archive

Extract the `.tar.gz` and keep the resulting `Galaxy` directory together. Start `galaxy-oss` from inside that directory. Keep its `resources` directory beside the executable.

The portable archive does not register a system package or install a desktop launcher. Install the `.deb` if you want the packaged application-list integration. Portable installs still need the normal system runtime libraries; consult [the runtime dependency script](https://github.com/rkw6086/galaxy/blob/master/script/linux/install_runtime_deps) and [current CI setup](https://github.com/rkw6086/galaxy/blob/master/script/ci/setup-linux.sh) for the maintained dependency lists.

## Install on Windows

1. Download the x64 `setup.exe` from the chosen release.
2. Run the installer and choose the installation options.
3. Launch the installed application from the Start menu. The installer also offers an optional desktop icon.

The current Windows installer is unsigned. Apple signing credentials only sign the Mac release; they do not provide a Windows publisher certificate.

## Verify a manual download

Each release includes a checksum manifest. Stable releases use `SHA256SUMS`; the Nightly manifest has its full build identity in the name. Find the entry for your exact downloaded filename and compare its SHA-256 with the file on your computer.

Examples, with the filename replaced as appropriate:

```bash
# Linux
sha256sum Galaxy_4.0.0_amd64.deb

# macOS
shasum -a 256 Galaxy_4.0.0_aarch64.dmg
```

```powershell
# Windows PowerShell
Get-FileHash .\Galaxy_4.0.0_x86_64-setup.exe -Algorithm SHA256
```

The complete checksum manifest covers multiple platforms and the update metadata. If you download only one installer, compare only its matching line rather than expecting a check of every manifest entry to pass.

## Update Galaxy

In an official updater-enabled build, open **Settings → About → Check for updates**. When an update is ready, use the displayed download/install/restart controls.

The app authenticates a signed Ed25519 update manifest, then checks the selected payload's name, size, and SHA-256 before installation. Stable and Nightly are fixed by the installed binary: a Nightly or older Beta follows Nightly, while a tagged build follows stable. Install the desired track manually to change tracks.

Platform behavior differs:

- **macOS:** downloads and verifies the selected DMG, then prepares the app update and relaunch.
- **Windows:** downloads and verifies the native installer for the update handoff.
- **Linux `.deb`:** automatic installation requires the canonical packaged executable, an installed `galaxy-oss` Debian package, `apt-get`, and `pkexec` with a working desktop authorization agent. The operating system handles the administrator prompt; Galaxy does not read your password.
- **Portable Linux, custom paths, or unsupported package authorization:** use the public download/manual installation path offered by the updater.

Older versions without the updater need one manual installation of an official updater-enabled build. Local source builds, unsigned pull-request builds, and the independent Windows validation installer are not official automatic-update builds.

See [Troubleshooting](https://github.com/rkw6086/galaxy/wiki/Troubleshooting) if update checks, authorization, or installation fail.

## Implementation references

- [Native release policy and package matrix](https://github.com/rkw6086/galaxy/blob/master/docs/release-macos.md)
- [GitHub update discovery and channel selection](https://github.com/rkw6086/galaxy/blob/master/app/src/autoupdate/github.rs)
- [Linux updater and package detection](https://github.com/rkw6086/galaxy/blob/master/app/src/autoupdate/linux.rs)
- [Linux desktop/package staging](https://github.com/rkw6086/galaxy/blob/master/script/linux/bundle_install)
- [macOS signing and packaging](https://github.com/rkw6086/galaxy/blob/master/script/ci/package-macos.sh)
- [Windows installer definition](https://github.com/rkw6086/galaxy/blob/master/script/windows/windows-installer.iss)
