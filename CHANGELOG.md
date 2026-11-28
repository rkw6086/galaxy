# Galaxy changelog

## [Unreleased]

- Reuse bounded compiler caches, Cargo downloads, and pinned packaging tools across native GitHub builds, while rebuilding and verifying each release's application metadata and signed packages.
- Prevent oversized model output limits from consuming the context window and blocking new chats. Keep context accounting, compaction, and the actual reply budget aligned.

## [4.0.2]

- Preserve split-view groups and divider widths when switching tabs and restarting Galaxy.
- Improve split-view resizing, tab drag and drop, and tab/group color consistency.
- Reduce duplicate work when saving window state and keep resize highlighting current.
- Add per-profile model startup timeouts and clearer expired AWS credential errors.
- Normalize Bedrock tool results for reliable multi-tool continuations.
- Use the outline Galaxy mark for loading indicators.
- Correct split-view migration rollback and improve resize cursor visibility on macOS.
- Update installer compatibility and version metadata, including a matching macOS icon fallback for older Xcode versions.

## [4.0.0]

- Check for and download verified updates from GitHub Releases, with an update status and manual check in About Galaxy.
- Keep tagged builds on stable updates and Beta/Nightly builds on the rolling Nightly channel. Authenticate update metadata with an embedded Ed25519 public key and verify package checksums before installation.
- Apply updates through the native macOS and Windows installation flow, and use the system authorization prompt for Linux Debian package updates.
- Give Nightly downloads readable Central-time dates, times, and build numbers while retaining exact source and build provenance.
- Use Gitea as the single local Git origin, with server jobs synchronizing clean merges with GitHub and Samsung.
- Build Windows x64, Linux x64, and signed Apple Silicon macOS releases on GitHub Actions while keeping Gitea as the primary source and synchronizing repository branches.
- Keep one rolling Nightly download while preserving all tagged release history and assets.
- Build Nightly every day at 4 a.m. Central and manually on demand, with notes since the previous Nightly. Set tagged app versions automatically from each new tag.
- Sign and notarize Apple Silicon Nightly DMGs, with the source commit in About Galaxy. Tagged releases continue to display their version.

## [3.1.3]

- Fix a crash when opening Bedrock settings and other provider pages that display saved models in a scrollable table.

## [3.1.2]

- Prevent unavailable or placeholder worker models from falling back to AWS Bedrock and requesting AWS credentials.
- Make unconfigured worker models follow the active chat model, and validate provider restrictions before worker startup and recovery.
- Improve diagnostics for worker model lists and profile provider selections.

## [3.1.1]

- Fix macOS signing order so bundled plug-ins are signed before the main app.
- Publish the signed and notarized macOS release with the automated build workflow.

## [3.1.0]

Unpublished; superseded by 3.1.1 after the initial signing failure.

- Add signed and notarized Apple Silicon macOS releases, published automatically from version tags.
- Show the release version in About Galaxy and remove the commit ID row.
- Enforce orchestration provider choices for child models and their coding, terminal, and computer-use roles.
- Capture application warnings, errors, shell startup failures, and error banners in remote diagnostics when remote logging is enabled.
- Improve split-view colors and preserve other chats when closing an individual split pane.
- Reduce repeated work when changing Bedrock settings and improve provider selection controls.
- Include prebuilt Wormhole helpers in app resources for remote server installation.
