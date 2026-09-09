use std::collections::BTreeMap;

use chrono::Utc;
use galaxy_acp::{AcpLaunchConfig, AcpManagerConfig, AcpSessionManager};
use galaxyui::{Entity, ModelContext, SingletonEntity};
use settings::Setting;

use crate::settings::{
    AISettings, AcpAgentSettings, AcpConfigOptionSettings, AcpConfigValueSettings,
};

/// Long-lived ACP process/session owner shared by all agent response streams.
pub(crate) struct AcpRuntimeModel {
    managed: Option<ManagedRuntime>,
    discovery: AcpDiscoveryState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AcpDiscoveryState {
    #[default]
    Idle,
    Running,
    Succeeded {
        option_count: usize,
    },
    Failed {
        message: String,
    },
}

struct ManagedRuntime {
    launch: AcpLaunchConfig,
    manager: AcpSessionManager,
}

impl AcpRuntimeModel {
    pub(crate) fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            managed: None,
            discovery: AcpDiscoveryState::Idle,
        }
    }

    pub(crate) fn discovery_state(&self) -> &AcpDiscoveryState {
        &self.discovery
    }

    pub(crate) fn begin_discovery(&mut self, ctx: &mut ModelContext<Self>) {
        self.discovery = AcpDiscoveryState::Running;
        ctx.emit(AcpRuntimeModelEvent::DiscoveryChanged);
    }

    pub(crate) fn finish_discovery_success(
        &mut self,
        option_count: usize,
        ctx: &mut ModelContext<Self>,
    ) {
        self.discovery = AcpDiscoveryState::Succeeded { option_count };
        ctx.emit(AcpRuntimeModelEvent::DiscoveryChanged);
    }

    pub(crate) fn finish_discovery_failure(
        &mut self,
        message: String,
        ctx: &mut ModelContext<Self>,
    ) {
        self.discovery = AcpDiscoveryState::Failed { message };
        ctx.emit(AcpRuntimeModelEvent::DiscoveryChanged);
    }

    pub(crate) fn manager(
        &mut self,
        config: AcpManagerConfig,
    ) -> Result<AcpSessionManager, String> {
        if let Some(managed) = &self.managed {
            if managed.launch == config.launch && managed.manager.is_alive() {
                return Ok(managed.manager.clone());
            }
        }
        let launch = config.launch.clone();
        let manager = AcpSessionManager::spawn(config).map_err(|error| error.to_string())?;
        self.managed = Some(ManagedRuntime {
            launch,
            manager: manager.clone(),
        });
        Ok(manager)
    }

    pub(crate) fn discovery_config_for_values(
        agent_id: &str,
        command: &str,
        args: &[String],
    ) -> Result<AcpManagerConfig, String> {
        let launch = crate::ai::acp::resolve_acp_launch(agent_id, command, args)?;
        Ok(AcpManagerConfig::new(launch))
    }

    pub(crate) fn normalize_config_options(
        options: Vec<galaxy_acp::SessionConfigOption>,
    ) -> Vec<AcpConfigOptionSettings> {
        options
            .into_iter()
            .map(|option| {
                let (kind, current_value, values) = match option.kind {
                    galaxy_acp::SessionConfigOptionType::Select(select) => {
                        let values = match select.options {
                            galaxy_acp::SessionConfigSelectOptions::Ungrouped(values) => values,
                            galaxy_acp::SessionConfigSelectOptions::Grouped(groups) => {
                                groups.into_iter().flat_map(|group| group.options).collect()
                            }
                            _ => Vec::new(),
                        };
                        (
                            "select",
                            serde_json::Value::String(select.current_value.to_string()),
                            values
                                .into_iter()
                                .map(|value| AcpConfigValueSettings {
                                    value: serde_json::Value::String(value.value.to_string()),
                                    name: value.name,
                                    description: value.description,
                                })
                                .collect(),
                        )
                    }
                    galaxy_acp::SessionConfigOptionType::Boolean(boolean) => (
                        "boolean",
                        serde_json::Value::Bool(boolean.current_value),
                        Vec::new(),
                    ),
                    _ => ("unknown", serde_json::Value::Null, Vec::new()),
                };
                AcpConfigOptionSettings {
                    id: option.id.to_string(),
                    name: option.name,
                    description: option.description,
                    category: option.category.map(|category| match category {
                        galaxy_acp::SessionConfigOptionCategory::Mode => "mode".to_owned(),
                        galaxy_acp::SessionConfigOptionCategory::Model => "model".to_owned(),
                        galaxy_acp::SessionConfigOptionCategory::ModelConfig => {
                            "model_config".to_owned()
                        }
                        galaxy_acp::SessionConfigOptionCategory::ThoughtLevel => {
                            "thought_level".to_owned()
                        }
                        galaxy_acp::SessionConfigOptionCategory::Other(value) => value,
                        _ => "unknown".to_owned(),
                    }),
                    kind: kind.to_owned(),
                    current_value,
                    options: values,
                }
            })
            .collect()
    }

    pub(crate) fn current_config_values(
        options: &[AcpConfigOptionSettings],
    ) -> BTreeMap<String, serde_json::Value> {
        options
            .iter()
            .filter(|option| !option.current_value.is_null())
            .map(|option| (option.id.clone(), option.current_value.clone()))
            .collect()
    }

    pub(crate) fn selection_values_for_model(
        options: &[AcpConfigOptionSettings],
        model_option_id: &str,
        model_value: &serde_json::Value,
    ) -> BTreeMap<String, serde_json::Value> {
        let mut values = Self::current_config_values(options);
        values.insert(model_option_id.to_owned(), model_value.clone());
        values
    }

    pub(crate) fn upsert_agent_settings(
        settings: &mut AISettings,
        agent_id: &str,
        options: Vec<galaxy_acp::SessionConfigOption>,
        ctx: &mut ModelContext<AISettings>,
    ) -> Result<(), String> {
        let config_options = Self::normalize_config_options(options);
        let mut agents = settings.acp_agents.value().clone();
        if let Some(agent) = agents
            .iter_mut()
            .find(|agent| agent.id.eq_ignore_ascii_case(agent_id))
        {
            agent.config_options = config_options;
        } else {
            agents.push(AcpAgentSettings {
                id: agent_id.to_owned(),
                name: agent_id.to_owned(),
                version: None,
                description: None,
                icon_url: None,
                capabilities: Vec::new(),
                config_options,
                discovery_timestamp: None,
                discovery_source: None,
                discovery_error: None,
            });
        }
        settings
            .acp_agents
            .set_value(agents, ctx)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn persist_runtime_options(
        settings: &mut AISettings,
        agent_id: &str,
        options: &[galaxy_acp::SessionConfigOption],
        ctx: &mut ModelContext<AISettings>,
    ) -> Result<(), String> {
        Self::upsert_agent_settings(settings, agent_id, options.to_vec(), ctx)
    }

    pub(crate) fn mark_discovery_success(
        settings: &mut AISettings,
        agent_id: &str,
        option_count: usize,
        ctx: &mut ModelContext<AISettings>,
    ) -> Result<(), String> {
        let mut agents = settings.acp_agents.value().clone();
        if let Some(agent) = agents
            .iter_mut()
            .find(|agent| agent.id.eq_ignore_ascii_case(agent_id))
        {
            agent.discovery_timestamp = Some(Utc::now().to_rfc3339());
            agent.discovery_source = Some(format!("runtime:{option_count}"));
            agent.discovery_error = None;
        }
        settings
            .acp_agents
            .set_value(agents, ctx)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn mark_discovery_failure(
        settings: &mut AISettings,
        agent_id: &str,
        error: String,
        ctx: &mut ModelContext<AISettings>,
    ) -> Result<(), String> {
        let mut agents = settings.acp_agents.value().clone();
        if let Some(agent) = agents
            .iter_mut()
            .find(|agent| agent.id.eq_ignore_ascii_case(agent_id))
        {
            agent.discovery_timestamp = Some(Utc::now().to_rfc3339());
            agent.discovery_error = Some(error);
        }
        settings
            .acp_agents
            .set_value(agents, ctx)
            .map_err(|error| error.to_string())
    }
}

impl Entity for AcpRuntimeModel {
    type Event = AcpRuntimeModelEvent;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AcpRuntimeModelEvent {
    DiscoveryChanged,
}

impl SingletonEntity for AcpRuntimeModel {}

#[cfg(test)]
#[path = "runtime_model_tests.rs"]
mod tests;
