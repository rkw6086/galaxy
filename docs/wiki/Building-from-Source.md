# Building from source

For ordinary use, start with [Installation and Updates](https://github.com/rkw6086/galaxy/wiki/Installation-and-Updates). Building Galaxy requires a native Rust toolchain and platform libraries; a successful compile does not by itself install an application bundle or create an official signed release.

## Get the source

```bash
git clone https://github.com/rkw6086/galaxy.git
cd galaxy
```

Install Git LFS before bootstrapping. The bootstrap script initializes LFS for the checkout and downloads the retained assets. The repository pins **Rust 1.97.1**, including `rustfmt` and `clippy`, in `rust-toolchain.toml`.

After installing Git LFS, run this in the checkout on every platform, including when using the Windows bootstrap directly:

```bash
git lfs install --local
git lfs pull
```

Public contributors can work through the GitHub repository. Follow the current contribution and release instructions for the task you are performing.

## Review platform setup first

The repository has a broad development bootstrap:

```bash
./script/bootstrap --help
```

Read the script and its platform helper before executing it. It can install system packages and development tools, request administrator permission, initialize Git LFS, and install common agent skills. Common skills are optional:

```bash
./script/bootstrap --skip-common-skills
```

Some bootstrap paths retain inherited test-tool setup, including Google Cloud CLI installation or authentication prompts. Google Cloud authentication is not a Galaxy runtime or ordinary model-provider requirement. Do not create or change a cloud account just to use Galaxy; review whether a particular inherited integration-test step is relevant to your development work.

### Ubuntu 24.04 x86-64: focused build prerequisites

This recipe adapts the native libraries used by Galaxy's Ubuntu 24.04 CI job. It prepares a normal `cargo build` without the broad bootstrap's inherited cloud authentication, integration-test tools, or common-skills installation. Run it on Ubuntu 24.04 x86-64; other distributions and older Ubuntu releases may need different package names or versions.

Install the system tools and development libraries:

```bash
sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  git git-lfs curl ca-certificates \
  build-essential cmake pkg-config libssl-dev libfreetype-dev libexpat1-dev \
  libgit2-dev libfontconfig1-dev libasound2-dev libclang-dev clang \
  libx11-dev libxi-dev libxcursor-dev libxrandr-dev libxinerama-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libegl1-mesa-dev \
  libgl1-mesa-dev libdbus-1-dev protobuf-compiler libprotobuf-dev
```

If `rustup` is not installed, install it using the [official Rust installer](https://rustup.rs/); the repository's [Rust installation helper](https://github.com/rkw6086/galaxy/blob/master/script/install_rust) uses that same source. Then install the pinned toolchain:

```bash
rustup toolchain install 1.97.1 --profile minimal \
  --component rustfmt --component clippy --target x86_64-unknown-linux-gnu
```

Open a new shell if `cargo` or `rustup` is not yet on `PATH`. From the Galaxy checkout, download the LFS assets and select the Ubuntu system tools for this shell:

```bash
git lfs install --local
git lfs pull
export PKG_CONFIG=/usr/bin/pkg-config
export PROTOC=/usr/bin/protoc
export PROTOC_INCLUDE=/usr/include
protoc --version
CARGO_BUILD_JOBS=2 cargo build --bin galaxy-oss
```

Both Protobuf packages matter: the compiler alone does not provide the complete well-known `.proto` include files that the build uses. `PROTOC_INCLUDE` points at the SDK's include root. Setting `PKG_CONFIG` explicitly avoids accidentally using a different package-manager installation when resolving Ubuntu libraries.

For Debian package creation and bundled third-party license attribution, install these additional tools:

```bash
sudo apt-get install -y --no-install-recommends fakeroot dpkg-dev
cargo +1.97.1 install --locked cargo-about@0.9.3 --features cli
```

`cargo-about` is used by resource/license generation for bundles; it is not an extra provider or cloud service. A plain `cargo build` does not produce a Debian installer. Use the packaging workflow described below when you need one.

The maintained [CI setup](https://github.com/rkw6086/galaxy/blob/master/script/ci/setup-linux.sh) remains the authority for current build dependencies. That helper requires GitHub Actions job variables, so the commands above adapt its prerequisites rather than execute it directly. A minimal Linux machine may also need graphics drivers and runtime packages before interactive use; see [the runtime dependency list](https://github.com/rkw6086/galaxy/blob/master/script/linux/install_runtime_deps). Compiling does not verify a headless machine's desktop or GPU setup.

### macOS

The development bootstrap expects full Xcode, configures the selected developer directory, runs Xcode's first-launch setup, and installs development tools through Homebrew/Cargo. It adds the `aarch64-apple-darwin` Rust target. Review its tool list before running it.

Official Mac artifacts are built natively on Apple Silicon and signed/notarized in GitHub Actions. A local source build does not automatically receive the project's signing credentials.

### Windows

Use Git for Windows and an MSVC toolchain with the Windows SDK. The maintained PowerShell setup path is:

```powershell
.\script\windows\bootstrap.ps1 -Help
```

The Windows bootstrap can install Rust, Visual Studio Build Tools, CMake, Inno Setup, and other development/test tools. Review [the bootstrap](https://github.com/rkw6086/galaxy/blob/master/script/windows/bootstrap.ps1) and [the release CI setup](https://github.com/rkw6086/galaxy/blob/master/script/ci/setup-windows.ps1) for exact current prerequisites. The CI helper expects GitHub Actions job state.

## Compile the app

From the repository root:

```bash
cargo build --bin galaxy-oss
```

The desktop binary is `galaxy-oss`; `galaxy` is the Cargo package name. The workspace's default app run target is also `galaxy-oss`. This distinction matters when selecting `--bin` or bundling an artifact.

For resource-constrained Linux development machines, the repository commonly limits parallel compiler jobs:

```bash
CARGO_BUILD_JOBS=2 cargo build --bin galaxy-oss
```

The local agent workflow in `AGENTS.md` verifies feature and bug-fix changes with `cargo build` only. Do not use app launch as a substitute for that required compile check. Native UI behavior still needs manual verification on a suitable target desktop; document separately what was built and what was exercised interactively.

The README also lists formatting, Clippy, and test commands for contributor checks. Follow the current `AGENTS.md` and contribution instructions for your task rather than assuming that a full workspace test run is required for every edit.

## Packages and local installation

The native release scripts are the authority for release features, resources, and version identity:

- [Linux build/package script](https://github.com/rkw6086/galaxy/blob/master/script/ci/build-linux.sh)
- [Mac build script](https://github.com/rkw6086/galaxy/blob/master/script/ci/build-macos.sh)
- [Mac signing/package script](https://github.com/rkw6086/galaxy/blob/master/script/ci/package-macos.sh)
- [Windows build/installer script](https://github.com/rkw6086/galaxy/blob/master/script/ci/build-windows.ps1)

These helpers expect validated CI inputs and job-specific directories. They are not generic copy-and-paste commands for an arbitrary shell.

A plain `cargo build --release` creates compiler output, not a `.deb`, DMG, or Windows installer. Use the platform bundling workflow and its documented prerequisites when you need a distributable application.

## Releases and CI

GitHub Actions builds Windows x64, Linux x64, and Mac ARM concurrently. Pull requests build without official signing or publication. Trusted Nightly and tagged publication require validated source identity and all platform builds to succeed.

Maintainers publish stable builds by creating an immutable **bare Semantic Version tag**, such as `4.0.1`, from appropriate source in `master`. Consult [the current release procedure](https://github.com/rkw6086/galaxy/blob/master/docs/release-macos.md) before tagging; do not move or reuse an existing version tag. A manually triggered `Build Galaxy` run on `master` creates a rolling Nightly without creating a stable version tag.

Native CI caches compiler results, public Cargo downloads, and version-checked packaging tools. Cache misses remain valid cold builds; signing, notarization, resources, and installer verification still run for the current source. Actual cache statistics are in each native job's logs.

## Implementation references

- [README](https://github.com/rkw6086/galaxy/blob/master/README.md)
- [Current repository instructions](https://github.com/rkw6086/galaxy/blob/master/AGENTS.md)
- [Rust toolchain](https://github.com/rkw6086/galaxy/blob/master/rust-toolchain.toml)
- [App manifest and binary target](https://github.com/rkw6086/galaxy/blob/master/app/Cargo.toml)
- [Bootstrap dispatcher](https://github.com/rkw6086/galaxy/blob/master/script/bootstrap)
- [Native release policy](https://github.com/rkw6086/galaxy/blob/master/docs/release-macos.md)
