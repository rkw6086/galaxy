// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

use anyhow::Result;
use galaxy_core::{
    channel::{Channel, ChannelConfig, ChannelState, OzConfig, WarpServerConfig},
    features, AppId,
};

fn main() -> Result<()> {
    ChannelState::set(
        ChannelState::new(
            Channel::Dev,
            ChannelConfig {
                app_id: AppId::new("com", "samsung", "Galaxy-Dev"),
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
        .with_additional_features(features::PREVIEW_FLAGS),
    );

    galaxy::run()
}
