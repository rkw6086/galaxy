# Galaxy changelog

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
