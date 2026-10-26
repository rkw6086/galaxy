# Shared install/deploy build preparation

`make install` and `make deploy` both call `prepareGalaxyBuild` before their
separate copy/launch or sign/archive/upload steps. Both require a clean checkout
on a branch and pull that branch from `origin` with `--ff-only`.

The shared builder keeps the existing optimized release profile, binary, and
`release_bundle,extern_plist` features. It does not run `cargo clean`.

After pulling, it compares the source commit, build command, toolchain/SDK,
architecture, environment, and Cargo configuration with the last successful
build. An exact match with an intact cached app skips Cargo and bundling.
Otherwise it runs the release bundle command, allowing Cargo to reuse its normal
compilation artifacts. The first invocation builds rather than trusting an
unidentified app already in `target/release`.

The cache lives in `target/galaxy-build-cache`. Bundle contents, modes, and
symlinks are checked before reuse. Each caller receives a private copy, so
signing for deployment cannot modify the shared cached app. Preparation is
serialized; copying, installing, and uploading use independent private copies.
The lock atomically records process identity and waits for live builds without
a fixed cold-build timeout. If a command is killed and leaves an abandoned lock,
the next command reports the lock path for manual removal rather than risk
stealing a live build's lock. Callers must release their copy in a `finally`
block.

Cargo metadata supplies the effective build output directory, including custom
`CARGO_TARGET_DIR` and `build.target-dir` settings. The shared cache remains
checkout-local so both commands use the same lock and cache. Both commands
explicitly request the Rust host target, overriding cross-target configuration;
these commands prepare native macOS bundles.

Unverifiable tool identity, external Cargo configuration, or mutable build
input overrides conservatively disable reuse rather than risk shipping stale
output. Upload credentials and Yarn invocation metadata do not identify the
application build. No credential values are stored in the manifest.

To force a fresh bundle while retaining compiled dependencies, remove
`target/galaxy-build-cache` when no install/deploy command is running.
`make clean` removes both Cargo output and this cache.

Run focused cache tests without installing or deploying anything:

```sh
node --test script/lib/prepare-galaxy-build.test.mjs
```
