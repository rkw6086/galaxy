use crate::settings::{
    AISettings, AcpAgentSettings, AcpConfigOptionSettings, AcpConfigValueSettings,
};
use galaxy_acp::{AcpLaunchConfig, AcpManagerConfig, AcpSessionManager};
use galaxyui::{Entity, ModelContext, SingletonEntity};

/// Long-lived ACP process/session owner shared by all agent response streams.
pub(crate) struct AcpRuntimeModel {
    managed: Option<ManagedRuntime>,
}

struct ManagedRuntime {
    launch: AcpLaunchConfig,
    manager: AcpSessionManager,
}

impl AcpRuntimeModel {
    pub(crate) fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self { managed: None }
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

    pub(crate) fn normalize_config_options(
        options: Vec<galaxy_acp::SessionConfigOption>,
    ) -> Vec<AcpConfigOptionSettings> {
        options
            .into_iter()
            .map(|option| {
                let kind = match &option.kind {
                    galaxy_acp::SessionConfigOptionType::Select => "select",
                    galaxy_acp::SessionConfigOptionType::Boolean => "boolean",
                }
                .to_owned();
                let current_value = serde_json::to_value(&option.current_value).unwrap_or_default();
                let values = option
                    .options
                    .into_iter()
                    .map(|value| AcpConfigValueSettings {
                        value: serde_json::to_value(value.value).unwrap_or_default(),
                        name: value.name,
                        description: value.description,
                    })
                    .collect();
                AcpConfigOptionSettings {
                    id: option.id,
                    name: option.name,
                    description: option.description,
                    category: option
                        .category
                        .map(|category| format!("{category:?}").to_lowercase()),
                    kind,
                    current_value,
                    options: values,
                }
            })
            .collect()
    }

    pub(crate) fn upsert_agent_settings(
        settings: &mut AISettings,
        agent_id: &str,
        options: Vec<galaxy_acp::SessionConfigOption>,
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
            });
        }
        settings
            .acp_agents
            .set_value(agents, &mut settings.context())
    }
}

impl Entity for AcpRuntimeModel {
    type Event = ();
}

impl SingletonEntity for AcpRuntimeModel {}
