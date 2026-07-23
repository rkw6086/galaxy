use tracing::subscriber;

pub fn init() -> anyhow::Result<Initialization> {
    subscriber::set_global_default(subscriber::NoSubscriber::new())?;
    Ok(Initialization::default())
}

#[cfg(not(target_family = "wasm"))]
pub fn start_auth_refresh(
    _client: std::sync::Arc<dyn warp_managed_secrets::client::ManagedSecretsClient>,
    _ctx: &mut warpui::AppContext,
) {
}

#[derive(Default)]
pub struct Initialization {
    initialization_warning: Option<anyhow::Error>,
}

impl Initialization {
    pub fn log_initialization_warning(&mut self) {
        if let Some(err) = self.initialization_warning.take() {
            log::warn!("Tracing initialization warning: {err:#}");
        }
    }

    pub(crate) fn shutdown(&mut self) {}
}

impl Drop for Initialization {
    fn drop(&mut self) {
        self.shutdown();
    }
}
