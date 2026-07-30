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
}

impl Entity for AcpRuntimeModel {
    type Event = ();
}

impl SingletonEntity for AcpRuntimeModel {}
