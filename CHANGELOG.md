# Galaxy changelog

## [Unreleased]

- Replace Galaxy Decides permission-review model calls with local checks for known inspections and bounded workspace edits; deny uncertain actions with a reason.
- Run independent observation tools in up to four concurrent slots, reuse provider HTTP connections, and stream output without a UI round trip for every delta.
- Avoid constructing model-output diagnostics when full-session logging is disabled.
- Adapt seven local Warp improvements covering status-bar subscriptions, shared UI task plumbing, Bash startup, Tab settings, CLI authentication detection, Markdown shortcuts, and MCP client identification.
- Refresh compatible Cargo dependencies while retaining the pinned Rust 1.97.1 toolchain and reviewed Git revisions.

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
