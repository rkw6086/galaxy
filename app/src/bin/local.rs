use anyhow::Result;
use galaxy_core::{
    channel::{Channel, ChannelConfig, ChannelState, OzConfig, WarpServerConfig},
    features, AppId,
};

fn main() -> Result<()> {
    let mut state = ChannelState::new(
        Channel::Local,
        ChannelConfig {
            app_id: AppId::new("com", "samsung", "Galaxy-Local"),
            logfile_name: "galaxy.log".into(),
            server_config: WarpServerConfig::production(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            crash_reporting_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    )
    .with_additional_features(features::DEBUG_FLAGS)
    .with_additional_features(features::DOGFOOD_FLAGS)
    .with_additional_features(features::PREVIEW_FLAGS);

    // Enable sandbox telemetry feature flag if the env var is set.
    if std::env::var("WITH_SANDBOX_TELEMETRY").is_ok() {
        state = state.with_additional_features(&[features::FeatureFlag::WithSandboxTelemetry]);
    }

    ChannelState::set(state);

    galaxy::run()
}

// If we're not using an external plist, embed the following as the Info.plist.
#[cfg(all(not(feature = "extern_plist"), target_os = "macos"))]
embed_plist::embed_info_plist_bytes!(r#"
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>English</string>
    <key>CFBundleDisplayName</key>
    <string>Galaxy</string>
    <key>CFBundleExecutable</key>
    <string>galaxy-local</string>
    <key>CFBundleIdentifier</key>
    <string>samsung.galaxy.GalaxyLocal</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Galaxy</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>UIDesignRequiresCompatibility</key>
    <true/>
    <key>CFBundleURLTypes</key>
    <array><dict><key>CFBundleURLName</key><string>Galaxy</string><key>CFBundleURLSchemes</key><array><string>galaxyai</string></array></dict></array>
    <key>NSHumanReadableCopyright</key>
    <string>© 2026, Samsung Electronics Co., Ltd.</string>
    </dict>
    </plist>
"#.as_bytes());
