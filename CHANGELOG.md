# Galaxy changelog

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
