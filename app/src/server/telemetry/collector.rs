use std::fs::remove_file;
use std::sync::Arc;
use std::time::Duration;

use galaxyui::{Entity, ModelContext, SingletonEntity};

use super::{clear_event_queue, rudder_event_file_path, RUDDER_TELEMETRY_EVENTS_FILE_NAME};
use crate::auth::AuthStateProvider;
use crate::channel::ChannelState;
use crate::features::FeatureFlag;
use crate::server::server_api::ServerApi;
use crate::settings::{PrivacySettings, PrivacySettingsChangedEvent};

// How often we send Active Usage signals.
const ACTIVE_USAGE_DURATION: Duration = Duration::from_secs(60);

/// Duration to wait before flushing the event queue to Rudderstack.
const TELEMETRY_FLUSH_DURATION: Duration = Duration::from_secs(30);

/// Max telemetry events to write to disk. This is bounded to limit the size of the file as well
/// as latency of writing the file.
const MAX_TELEMETRY_EVENTS_TO_STORE: usize = 20;

/// Maximum time to wait for the telemetry flush network request during shutdown.
/// If the network is unavailable or slow, we don't want the CLI process to hang indefinitely.
const TELEMETRY_SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// App singleton responsible for scheduling periodic background tasks for sending batches of
/// telemetry events to Rudderstack.  This model respects the user's telemetry enablement setting.
pub struct TelemetryCollector {
    _server_api: Arc<ServerApi>,
}

impl TelemetryCollector {
    pub fn new(server_api: Arc<ServerApi>) -> Self {
        Self { server_api }
    }

    pub fn initialize_telemetry_collection(&self, ctx: &mut ModelContext<TelemetryCollector>) {
        // Start a background thread to periodically flush events from the telemetry event queue.
        if ChannelState::is_release_bundle() || FeatureFlag::WithSandboxTelemetry.is_enabled() {
            // Flush the events to Rudderstack that were persisted into a file the last time the app was
            // quit.
            self.flush_persisted_events_from_disk(ctx);
        }

        // Send Active App Usage signals
        if FeatureFlag::RecordAppActiveEvents.is_enabled()
            && (ChannelState::is_release_bundle() || FeatureFlag::WithSandboxTelemetry.is_enabled())
        {
            self.schedule_send_active_usage_event(ctx);
        }

        // Start a background thread to periodically flush events from the telemetry event queue.
        if ChannelState::is_release_bundle()
            || FeatureFlag::WithSandboxTelemetry.is_enabled()
            || FeatureFlag::SendTelemetryToFile.is_enabled()
        {
            self.schedule_event_queue_flush(ctx);
        }

        // Clear queued telemetry events when telemetry is enabled or disabled. If telemetry is
        // enabled, we will start sending Rudderstack requests when the event queue is periodically
        // flushed. The initial request should not contain any events recorded when the user was
        // previously opted-out of telemetry. In the case where the user turns the telemetry from
        // on to off, we should not send another request with any telemetry, even if the event was
        // initially recorded prior to the user turning telemetry off.`
        ctx.subscribe_to_model(&PrivacySettings::handle(ctx), |_me, _, event, _ctx| {
            if let PrivacySettingsChangedEvent::UpdateIsTelemetryEnabled { .. } = event {
                clear_event_queue();
            }
        });
    }

    /// Writes all queued but unsent telemetry telemetry events to disk so that they may be sent
    /// on the next app startup.
    pub fn write_telemetry_events_to_disk(&self, ctx: &mut ModelContext<TelemetryCollector>) {
        match self.server_api.persist_telemetry_events(
            MAX_TELEMETRY_EVENTS_TO_STORE,
            PrivacySettings::as_ref(ctx).get_snapshot(ctx),
        ) {
            Ok(()) => {
                log::info!("Successfully wrote telemetry events to disk")
            }
            Err(e) => {
                log::error!("Failed to write telemetry events to disk {e:#}");
            }
        }
    }

    pub fn initialize_telemetry_collection(&self, _ctx: &mut ModelContext<TelemetryCollector>) {}

    pub fn flush_telemetry_events_for_shutdown(&self, _ctx: &mut ModelContext<Self>) {}
}

impl Entity for TelemetryCollector {
    type Event = ();
}

impl SingletonEntity for TelemetryCollector {}
