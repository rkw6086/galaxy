// On Windows, we don't want to display a console window when the application is running in release
// builds. See https://doc.rust-lang.org/reference/runtime.html#the-windows_subsystem-attribute.
#![cfg_attr(feature = "release_bundle", windows_subsystem = "windows")]

use anyhow::Result;
use galaxy_core::{
    channel::{Channel, ChannelConfig, ChannelState, OzConfig, WarpServerConfig},
    AppId,
};

fn main() -> Result<()> {
    ChannelState::set(ChannelState::new(
        Channel::Stable,
        warp_channel_config::load_config!("stable"),
    ));

    galaxy::run()
}
