use enum_iterator::Sequence;
use galaxy_core::settings::{macros::define_settings_group, SupportedPlatforms, SyncToCloud};
use serde::{Deserialize, Serialize};

/// The app icon to use (mac-only).
///
/// IMPORTANT NOTE: If you add a new icon, you will need to update the logic in GalaxyDockTilePlugin.m
/// to read the new icon and also add the icon to app/DockTilePlugin/Resources.
#[derive(
    Default,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Serialize,
    Deserialize,
    Sequence,
    schemars::JsonSchema,
    settings_value::SettingsValue,
)]
#[schemars(
    description = "The app icon displayed in the dock.",
    rename_all = "snake_case"
)]
pub enum AppIcon {
    #[default]
    #[schemars(description = "Galaxy")]
    Galaxy,
    #[schemars(description = "Dot Matrix")]
    DotMatrix,
    #[schemars(description = "Explorer")]
    Explorer,
    #[schemars(description = "Rainbow")]
    Rainbow,
    #[schemars(description = "Wormhole")]
    Wormhole,
}

impl std::fmt::Display for AppIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match &self {
            AppIcon::Galaxy => "Galaxy",
            AppIcon::DotMatrix => "Dot Matrix",
            AppIcon::Explorer => "Explorer",
            AppIcon::Rainbow => "Rainbow",
            AppIcon::Wormhole => "Wormhole",
        };
        write!(f, "{value}")
    }
}

impl AppIconSettings {
    pub fn get_base_icon_file_name(icon: AppIcon) -> &'static str {
        match icon {
            AppIcon::Galaxy => "galaxy",
            AppIcon::DotMatrix => "galaxy_dotmatrix",
            AppIcon::Explorer => "galaxy_explorer",
            AppIcon::Rainbow => "galaxy_rainbow",
            AppIcon::Wormhole => "galaxy_wormhole",
        }
    }
}

define_settings_group!(AppIconSettings, settings: [
    app_icon: AppIconState {
        type: AppIcon,
        default: AppIcon::Galaxy,
        supported_platforms: SupportedPlatforms::MAC,
        sync_to_cloud: SyncToCloud::Never,
        private: false,
        storage_key: "AppIcon",
        toml_path: "appearance.icon.app_icon",
        description: "The app icon displayed in the dock.",
    },
]);
