// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

use anyhow::Result;
use galaxy_core::channel::{Channel, ChannelConfig, ChannelState, OzConfig, WarpServerConfig};
use galaxy_core::AppId;

fn main() -> Result<()> {
    use galaxy_core::features::FeatureFlag;

    let mut state = ChannelState::new(
        Channel::Oss,
        ChannelConfig {
            app_id: AppId::new("com", "samsung", "Galaxy"),
            logfile_name: "galaxy.log".into(),
            server_config: WarpServerConfig::disabled(),
            oz_config: OzConfig::production(),
            telemetry_config: None,
            autoupdate_config: None,
            mcp_static_config: None,
        },
    );
    state = state.with_additional_features(galaxy_core::features::DOGFOOD_FLAGS);
    if cfg!(debug_assertions) {
        state = state.with_additional_features(galaxy_core::features::DEBUG_FLAGS);
    }
    state = state.with_additional_features(&[
        FeatureFlag::AgentMode,
        FeatureFlag::AgentView,
        FeatureFlag::LspCompletion,
        FeatureFlag::LspCodeActions,
        FeatureFlag::LspRename,
        FeatureFlag::LspSignatureHelp,
    ]);
    ChannelState::set(state);

    galaxy::run()
}

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
    <string>galaxy-oss</string>
    <key>CFBundleIdentifier</key>
    <string>samsung.galaxy.GalaxyOss</string>
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
    <key>NSDockTilePlugIn</key>
    <string>GalaxyDockTilePlugin.docktileplugin</string>
    </dict>
    </plist>
"#.as_bytes());
