# Credentials & Signing Pipeline

## Apple Developer Credentials

| Field | Value |
|-------|-------|
| Apple ID | g.brancovici@samsung.com |
| Team ID | 3JU72Z7Y3J |
| Team Name | SAMSUNG ELECTRONICS AMERICA, INC. |
| Certificate | Developer ID Application: SAMSUNG ELECTRONICS AMERICA, INC. (3JU72Z7Y3J) |
| App-Specific Password | cowi-fved-uajk-arsk |
| Bundle ID | com.samsung.Galaxy |

## Signing Pipeline

The build pipeline is a sequence of standalone scripts:

```
build-release.sh
  ├── cargo bundle --release --bin galaxy-oss --package galaxy
  ├── sign.sh --release
  ├── notarize.sh --release
  └── package.sh --release
```

### sign.sh
- Signs `Galaxy.app` with Samsung Developer ID cert
- Uses hardened runtime (`--options runtime`)
- Signs inside-out (binary first, then bundle)
- Uses `--timestamp` on all codesign calls
- Entitlements from `BuildSupport/Galaxy.entitlements`

### notarize.sh
- Creates zip of Galaxy.app
- Submits to Apple notary service via `xcrun notarytool submit`
- Uses inline credentials (NOT keychain profile — doesn't work from Galaxy terminal)
- Waits for acceptance, then staples ticket

### package.sh
- Creates DMG with app + /Applications symlink via `hdiutil`
- Signs the DMG
- Notarizes the DMG (same inline credentials)
- Staples ticket to DMG

### Entitlements (BuildSupport/Galaxy.entitlements)
- `com.apple.security.network.client` — outbound network
- `com.apple.security.automation.apple-events` — AppleScript
- `com.apple.security.cs.allow-unsigned-executable-memory` — Metal/wgpu shader compilation

## Known Issues

1. **Keychain profile doesn't work from Galaxy terminal** — `xcrun notarytool store-credentials` requires interactive macOS keychain authorization dialog. Galaxy (as a non-standard terminal) can't show it. Workaround: inline credentials in scripts.

2. **hdiutil "Resource busy"** — Sometimes the first `hdiutil create` attempt fails. Scripts retry up to 3 times with 3s delay.

3. **App-specific password expiration** — Apple app-specific passwords can be revoked from appleid.apple.com. If notarization fails with 401, generate a new one at Sign-In & Security → App-Specific Passwords.

## macOS Keychain Notes

If you ever need to use keychain profile approach (e.g., from Terminal.app):
```bash
security unlock-keychain -p 'MACOS_PASSWORD' ~/Library/Keychains/login.keychain-db
xcrun notarytool store-credentials notarytool-profile \
  --apple-id g.brancovici@samsung.com \
  --team-id 3JU72Z7Y3J \
  --password cowi-fved-uajk-arsk
```

George's macOS password uses `!` prefix — must be single-quoted in shell to avoid zsh history expansion.
