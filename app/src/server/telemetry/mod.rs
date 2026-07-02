mod collector;
mod context;
pub mod context_provider;
pub mod events;
mod macros;
pub mod rudder_message;
pub mod secret_redaction;

pub use collector::*;
pub use context::telemetry_context;
pub use events::*;

/// No-op stub. Telemetry has been removed from Galaxy.
pub struct TelemetryApi {
    pub client: http_client::Client,
}

impl Default for TelemetryApi {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryApi {
    pub fn new() -> Self {
        Self {
            client: http_client::Client::default(),
        }
    }

    pub async fn send_telemetry_event(
        &self,
        _user_id: Option<crate::auth::UserUid>,
        _anonymous_id: String,
        _event: impl galaxy_core::telemetry::TelemetryEvent,
        _settings_snapshot: crate::settings::PrivacySettingsSnapshot,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    pub async fn flush_events(
        &self,
        _settings_snapshot: crate::settings::PrivacySettingsSnapshot,
    ) -> anyhow::Result<usize> {
        Ok(0)
    }

    pub async fn flush_persisted_events_to_rudder(
        &self,
        _path: &std::path::Path,
        _settings_snapshot: crate::settings::PrivacySettingsSnapshot,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn flush_and_persist_events(
        &self,
        _max_event_count: usize,
        _settings_snapshot: crate::settings::PrivacySettingsSnapshot,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

#[allow(dead_code)]
pub fn clear_event_queue() {
    let _ = galaxyui::telemetry::flush_events();
}
