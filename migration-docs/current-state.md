# Current State — as of 2026-07-09

## Git State

- **Branch:** `master`
- **HEAD:** `40bd86f6` — "Adding logging when we crash in bedrock, adding open AI request translator changes and AI page settings cleanup"
- **Remote:** Up to date with `origin/master`
- **Other branches:** `feat/warp-parity` (remote only)
- **Tags:** `v1.6.3` (latest)

## Uncommitted Changes

Two modified files (NOT yet committed):

### `notarize.sh` (diff)
```diff
-    --keychain-profile notarytool-profile \
+    --apple-id "g.brancovici@samsung.com" \
+    --team-id "3JU72Z7Y3J" \
+    --password "cowi-fved-uajk-arsk" \
```

### `package.sh` (diff)
```diff
-    --keychain-profile notarytool-profile \
+    --apple-id "g.brancovici@samsung.com" \
+    --team-id "3JU72Z7Y3J" \
+    --password "cowi-fved-uajk-arsk" \
```

**Why:** The keychain profile `notarytool-profile` was lost (keychain reset). Replaced with inline credentials because Galaxy terminal can't write to keychain (no interactive auth entitlement). These changes have been tested — full build-release.sh completed successfully with them.

## What Was Just Completed

- Pulled latest code (840bc4d → 40bd86f6, massive update with Warp rebase)
- Fixed notarization scripts to use inline credentials
- Successfully built a signed+notarized release DMG (`target/release/bundle/osx/Galaxy.dmg`)

## What's Next (Pending Work)

### Immediate
- **Commit the script changes** — `notarize.sh` and `package.sh` credential fix (George hasn't asked for commit yet)
- **George may re-run build-release.sh** — he asked for clean build instructions

### Backlog (from May code review — still open)
1. **[HIGH]** Gate `dump_error_snapshot` behind `GALAXY_BEDROCK_DIAGNOSTICS=1` env var
2. **[HIGH]** Fix `error_dump_directory` using compile-time `CARGO_MANIFEST_DIR` in release builds
3. **[HIGH]** Install script fetches Homebrew/rustup from HEAD without pinning
4. **[MED]** Silent mutex poisoning in `dump_error_snapshot`
5. **[MED]** Captured lines buffer unbounded by bytes
6. **[MED]** `is_retryable` uses fragile string matching
7. **[MED]** `rotate_if_needed` double-renames
8. **[MED]** `read_file_tail` blocks async runtime
9. **[LOW]** Integer overflow in token sum cast to u32
10. **[LOW]** Race condition in install script's pkill → rm -rf
11. **[LOW]** `git pull --ff-only` fails silently on diverged branches

## Build Output Location

```
target/release/bundle/osx/Galaxy.app   # Signed app bundle
target/release/bundle/osx/Galaxy.dmg   # Signed + notarized DMG (distributable)
```
