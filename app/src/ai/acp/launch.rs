use galaxy_acp::{AcpAgentPreset, AcpLaunchConfig, CODEX_ACP_NPM_VERSION, OPENCODE_NPM_VERSION};
use sha2::{Digest as _, Sha256};

use crate::persistence::model::AcpConversationData;

pub(crate) fn acp_model_id(agent_id: &str) -> String {
    format!("acp:{}", agent_id.trim().to_ascii_lowercase())
}

pub(crate) fn acp_selection_model_id(
    agent_id: &str,
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> String {
    let suffix = values
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(";");
    if suffix.is_empty() {
        acp_model_id(agent_id)
    } else {
        format!("{}:{suffix}", acp_model_id(agent_id))
    }
}

pub(crate) fn acp_selection_identity(
    agent_id: &str,
    values: &std::collections::BTreeMap<String, serde_json::Value>,
) -> String {
    let mut identity = acp_model_id(agent_id);
    for (key, value) in values {
        identity.push(':');
        identity.push_str(key);
        identity.push('=');
        identity.push_str(&canonical_json_value(value));
    }
    identity
}

fn canonical_json_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_owned(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::String(value) => serde_json::to_string(value).unwrap_or_default(),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json_value)
                .collect::<Vec<_>>()
                .join(",")
        ),
        serde_json::Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_default(),
                        canonical_json_value(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

pub(crate) fn acp_launch_fingerprint(
    agent_id: &str,
    custom_command: &str,
    custom_args: &[String],
) -> String {
    match resolve_acp_launch(agent_id, custom_command, custom_args) {
        Ok(launch) => effective_launch_fingerprint(
            agent_id,
            preset_version(agent_id, custom_command),
            &launch,
            true,
        ),
        Err(_) => {
            let unresolved_launch = if !custom_command.trim().is_empty() {
                AcpLaunchConfig::new(custom_command.trim()).args(custom_args.iter().cloned())
            } else {
                match agent_id.trim().to_ascii_lowercase().as_str() {
                    "codex" => AcpAgentPreset::Codex.launch_config(),
                    "opencode" => AcpAgentPreset::OpenCode.launch_config(),
                    _ => AcpLaunchConfig::new(""),
                }
            };
            effective_launch_fingerprint(
                agent_id,
                preset_version(agent_id, custom_command),
                &unresolved_launch,
                false,
            )
        }
    }
}

fn effective_launch_fingerprint(
    agent_id: &str,
    preset_version: Option<&str>,
    launch: &AcpLaunchConfig,
    command_is_resolved: bool,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"galaxy-acp-launch-v2");
    add_field(&mut hasher, &agent_id.trim().to_ascii_lowercase());
    add_field(
        &mut hasher,
        if command_is_resolved {
            "resolved"
        } else {
            "unresolved"
        },
    );
    add_field(&mut hasher, preset_version.unwrap_or(""));

    let command = if command_is_resolved {
        std::fs::canonicalize(&launch.command).unwrap_or_else(|_| launch.command.clone())
    } else {
        launch.command.clone()
    };
    add_field(&mut hasher, &command.to_string_lossy());

    add_count(&mut hasher, launch.args.len());
    for arg in &launch.args {
        add_field(&mut hasher, arg);
    }
    add_count(&mut hasher, launch.env.len());
    for (name, value) in &launch.env {
        add_field(&mut hasher, name);
        add_field(&mut hasher, value);
    }
    match launch.preferred_auth_method.as_ref() {
        Some(method) => {
            add_field(&mut hasher, "preferred-auth");
            add_field(&mut hasher, &method.to_string());
        }
        None => add_field(&mut hasher, "default-auth"),
    }
    format!("{:x}", hasher.finalize())
}

fn add_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn add_count(hasher: &mut Sha256, count: usize) {
    hasher.update((count as u64).to_le_bytes());
}

fn preset_version(agent_id: &str, custom_command: &str) -> Option<&'static str> {
    if !custom_command.trim().is_empty() {
        return None;
    }
    match agent_id.trim().to_ascii_lowercase().as_str() {
        "codex" => Some(CODEX_ACP_NPM_VERSION),
        "opencode" => Some(OPENCODE_NPM_VERSION),
        _ => None,
    }
}

pub(crate) fn validate_acp_launch_identity(
    backend: &AcpConversationData,
    configured_agent_id: &str,
    custom_command: &str,
    launch: &AcpLaunchConfig,
) -> Result<(), String> {
    if backend.launch_fingerprint.is_empty() {
        return Err(
            "This ACP conversation predates Galaxy's agent-session safety metadata. Disable ACP and start a new conversation, then re-enable ACP and start a fresh ACP conversation."
                .to_owned(),
        );
    }

    let current = effective_launch_fingerprint(
        configured_agent_id,
        preset_version(configured_agent_id, custom_command),
        launch,
        true,
    );
    if backend
        .agent_id
        .trim()
        .eq_ignore_ascii_case(configured_agent_id.trim())
        && backend.launch_fingerprint == current
    {
        return Ok(());
    }

    Err(
        "The ACP agent configuration has changed since this conversation was created, so Galaxy will not send its saved session ID to a different agent process. Start a new conversation to use the current ACP configuration."
            .to_owned(),
    )
}

pub(crate) fn resolve_acp_launch(
    agent_id: &str,
    custom_command: &str,
    custom_args: &[String],
) -> Result<AcpLaunchConfig, String> {
    if !custom_command.trim().is_empty() {
        return AcpLaunchConfig::new(custom_command.trim())
            .args(custom_args.iter().cloned())
            .resolve_command();
    }

    match agent_id.trim().to_ascii_lowercase().as_str() {
        "codex" => AcpAgentPreset::Codex.resolve_launch_config(),
        "opencode" => AcpAgentPreset::OpenCode.resolve_launch_config(),
        unknown => Err(format!(
            "Unknown ACP agent preset {unknown:?}; choose \"codex\" or \"opencode\", or configure a custom ACP executable"
        )),
    }
}

pub(crate) fn validate_acp_dispatch(
    feature_enabled: bool,
    setting_enabled: bool,
    is_remote: bool,
) -> Result<(), String> {
    if !feature_enabled || !setting_enabled {
        return Err(
            "This conversation uses ACP, but Agent Client Protocol is currently disabled in Galaxy settings"
                .to_owned(),
        );
    }
    if is_remote {
        return Err(
            "Galaxy blocked this ACP request because the terminal is remote (for example, through Wormhole or SSH). Local ACP agents could otherwise run native shell and file tools on the wrong host. Open a local terminal, disable ACP, and start a new provider-backed conversation; or start a new local ACP conversation."
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
