use std::sync::Arc;

use galaxyui::{Entity, ModelContext, SingletonEntity};

use crate::server::server_api::ServerApi;

/// No-op telemetry collector. Telemetry has been removed from Galaxy.
pub struct TelemetryCollector {
    _server_api: Arc<ServerApi>,
}

impl TelemetryCollector {
    pub fn new(server_api: Arc<ServerApi>) -> Self {
        Self {
            _server_api: server_api,
        }
    }

    pub fn initialize_telemetry_collection(&self, _ctx: &mut ModelContext<TelemetryCollector>) {}

    pub fn flush_telemetry_events_for_shutdown(&self, _ctx: &mut ModelContext<Self>) {}
}

impl Entity for TelemetryCollector {
    type Event = ();
}

impl SingletonEntity for TelemetryCollector {}
