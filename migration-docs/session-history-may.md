# Prior Session — 2026-05-13 / 2026-05-14

## Tasks Completed

1. **Codebase learn** — Full read of the Galaxy codebase (56-crate Rust workspace, fork of Warp with Bedrock AI)
2. **Code signing pipeline** — Created complete macOS signing, notarization, and DMG packaging workflow
3. **Review & hardening** — Two rounds of adversarial review + fixes applied
4. **Documentation** — Updated GALAXY.md with signing/diagnostics docs
5. **Code review** — Started adversarial review of last day's committed changes

## Key Decisions

- **Entitlements:** `network.client` + `automation.apple-events` + `cs.allow-unsigned-executable-memory` (for Metal/wgpu shader compilation under hardened runtime)
- **Signing approach:** Simple standalone scripts (sign.sh, notarize.sh, package.sh) rather than adapting Warp's 800-line CI bundle script
- **Signing method:** Inside-out (binary first, then bundle) with `--timestamp` on all codesign calls
- **Architecture:** arm64 only (no universal binary needed)
- **Bundle ID:** `com.samsung.Galaxy`
- **Certificate:** `Developer ID Application: SAMSUNG ELECTRONICS AMERICA, INC. (3JU72Z7Y3J)`

## Files Created

- `BuildSupport/Galaxy.entitlements` — macOS entitlements for code signing
- `sign.sh` — Code signs Galaxy.app with Samsung Developer ID cert + hardened runtime
- `notarize.sh` — Submits to Apple notary service + staples ticket
- `package.sh` — Creates DMG + signs + notarizes + staples
- `build-debug.sh` — Full pipeline orchestration (debug)
- `build-release.sh` — Full pipeline orchestration (release)
- `copy-dmgs.sh` — Copies DMGs to ~/Downloads with date+letter suffix
- `GALAXY.md` — Updated with signing pipeline + diagnostics documentation

## Code Review Findings (STILL OPEN)

### High Severity
1. **[Security] `dump_error_snapshot` writes full conversation context to world-readable files** — Error dumps include user's shell command outputs, secrets potentially in conversation. Recommendation: gate behind `GALAXY_BEDROCK_DIAGNOSTICS=1` or set 0600 permissions.
2. **[Security] `error_dump_directory` uses compile-time `CARGO_MANIFEST_DIR`** — Points to build machine path in release builds. Should be gated behind `cfg!(debug_assertions)`.
3. **[Security] Install script fetches Homebrew/rustup from HEAD without pinning** — Supply chain risk in curl-pipe-bash context.

### Medium Severity
4. **[Robustness] Silent mutex poisoning in `dump_error_snapshot`** — Partial data written without indication.
5. **[Performance] Captured lines buffer unbounded by bytes** — Single "line" can be several MB for large contexts.
6. **[Correctness] `is_retryable` uses fragile string matching** — `"Validation error"` may not appear in stream errors.
7. **[Correctness] `rotate_if_needed` double-renames** — Off-by-one: current file renamed twice (harmless but confused logic).
8. **[Robustness] `read_file_tail` blocks async runtime** — 200KB sync I/O per file on potentially slow filesystem.

### Low Severity
9. Integer overflow in token sum cast to u32
10. Race condition in install script's pkill → rm -rf sequence
11. `git pull --ff-only` fails silently on diverged branches
