use galaxy_core::{features::FeatureFlag, settings::Setting};
use galaxy_util::path::ShellFamily;
use serde::{Deserialize, Serialize};

use crate::terminal::wormhole::settings::WormholeSettings;

/// The different possible outcomes of detecting an interactive SSH session.
/// Also the payload for the [`crate::server::telemetry::TelemetryEvent::SshInteractiveSessionDetected`] event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SshInteractiveSessionDetected {
    #[serde(rename = "feature_disabled")]
    FeatureDisabled,
    #[serde(rename = "host_denylisted")]
    HostDenylisted,
    #[serde(rename = "wormhole_prompt")]
    ShouldPromptWormholing {
        #[serde(skip)]
        command: String,
        #[serde(skip)]
        host: Option<String>,
    },
}

/// Determines whether a host could be wormholed.
pub fn evaluate_wormhole_ssh_host(
    command: &str,
    ssh_host: Option<&str>,
    shell_family: ShellFamily,
    wormhole_settings: &WormholeSettings,
) -> SshInteractiveSessionDetected {
    let should_prompt_ssh_tmux_wrapper = *wormhole_settings.enable_ssh_wormholing.value()
        && *wormhole_settings.use_ssh_tmux_wrapper.value();
    let matches_subshell = wormhole_settings.is_denylisted_subshell_command(command)
        || wormhole_settings.is_compatible_subshell_command(command, shell_family);
    if !should_prompt_ssh_tmux_wrapper
        || matches_subshell
        || !FeatureFlag::SSHTmuxWrapper.is_enabled()
    {
        return SshInteractiveSessionDetected::FeatureDisabled;
    }

    if let Some(ssh_host) = ssh_host {
        if wormhole_settings.is_ssh_host_denylisted(ssh_host) {
            return SshInteractiveSessionDetected::HostDenylisted;
        }
    }

    SshInteractiveSessionDetected::ShouldPromptWormholing {
        host: ssh_host.map(|host| host.to_owned()),
        command: command.to_string(),
    }
}
