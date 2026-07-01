mod collector;
mod context;
pub mod context_provider;
pub mod events;
mod macros;
pub mod rudder_message;
pub mod secret_redaction;

use std::fs::File;
#[cfg(not(target_family = "wasm"))]
use std::fs::OpenOptions;
use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
pub use collector::*;
pub use context::telemetry_context;
pub use events::*;
use futures::FutureExt;
use rudder_message::{
    Batch as RudderBatch, BatchMessage as RudderBatchMessageWithMetadata,
    BatchMessageItem as RudderBatchMessage, Message as RudderMessage,
};
use galaxy_core::channel::RudderStackDestination;
use warpui::telemetry::Event;

use crate::auth::UserUid;
use crate::features::FeatureFlag;
use crate::server::telemetry::context::AttachContext;
use crate::server::telemetry_ext::TelemetryExt;
use crate::settings::PrivacySettingsSnapshot;
use crate::ChannelState;

/// Filename for file where telemetry events are written on app quit.
const RUDDER_TELEMETRY_EVENTS_FILE_NAME: &str = "rudder_telemetry_events.json";

/// Filepath where the Rudder events should be written on app quit.
fn rudder_event_file_path() -> PathBuf {
    galaxy_core::paths::secure_state_dir()
        .unwrap_or_else(galaxy_core::paths::state_dir)
        .join(RUDDER_TELEMETRY_EVENTS_FILE_NAME)
}

/// Removes all telemetry events from the app telemetry event queue.
pub fn clear_event_queue() {
    let _ = galaxyui::telemetry::flush_events();
}

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
        user_id: Option<UserUid>,
        anonymous_id: String,
        event: impl galaxy_core::telemetry::TelemetryEvent,
        settings_snapshot: PrivacySettingsSnapshot,
    ) -> Result<()> {
        let event = galaxyui::telemetry::create_event(
            user_id.map(|uid| uid.as_string()),
            anonymous_id,
            event.name().into(),
            event.payload(),
            event.contains_ugc(),
            galaxyui::time::get_current_time(),
        );

        self.send_telemetry_event_internal(event, settings_snapshot)
            .await
    }

    /// Internal implementation for sending telemetry events. This reduces code size, since
    // we:
    // 1. Return a boxed future, so calling `async` functions don't need to inline this one.
    // 2. Don't have to monomorphize for each telemetry event implementation.
    fn send_telemetry_event_internal(
        &self,
        event: Event,
        settings_snapshot: PrivacySettingsSnapshot,
    ) -> impl Future<Output = Result<()>> + '_ {
        let work = async move {
            if settings_snapshot.should_disable_telemetry() {
                log::info!("Not sending telemetry event because telemetry is disabled.");
                return Result::Ok(());
            }

            #[cfg(not(target_family = "wasm"))]
            if FeatureFlag::SendTelemetryToFile.is_enabled() {
                self.persist_events_to_telemetry_log_file(vec![event.clone()])?;
            }

            if !(ChannelState::is_release_bundle()
                || FeatureFlag::WithSandboxTelemetry.is_enabled())
            {
                return Result::Ok(());
            }

            let rudder_batch = vec![event.to_rudder_batch_message()];

            let result = self
                .send_batch_messages_to_rudder(rudder_batch, settings_snapshot)
                .await;

            // This is only conditionally compiled because `is_connect` is not
            // available on wasm.  If additional checks are made against the
            // `reqwest::Error`, this condition should be performed specifically
            // against `is_connect` and not the whole loop.
            #[cfg(not(target_family = "wasm"))]
            if let Err(error) = &result {
                for cause in error.chain() {
                    if let Some(err) = cause.downcast_ref::<reqwest::Error>() {
                        if err.is_connect() {
                            log::warn!("Failed to send telemetry event: {error}");
                            return Ok(());
                        }
                    }
                }
            }

            result
        };

        // On WASM, the work future is non-Send, because the HTTP request future contains a reference to a JS
        // value (which is fine, since our WASM executor is single-threaded). On all other platforms, we must
        // return a Send future in order to use the background executor.
        cfg_if::cfg_if! {
            if #[cfg(target_family = "wasm")] {
                work.boxed_local()
            } else {
                work.boxed()
            }
        }
    }

    /// Send a batch of RudderStack messages to their HTTP API.
    /// Note that the rudderanalytics SDK provides a client, but we don't
    /// use it for a few reasons:
    /// 1. It only supports a blocking HTTP client instead of an async one
    /// 2. We want to use our own HTTP client which has before/after request logging hooks
    #[cfg_attr(target_family = "wasm", allow(clippy::question_mark))]
    async fn send_batch_messages_to_rudder(
        &self,
        messages: Vec<RudderBatchMessageWithMetadata>,
        settings_snapshot: PrivacySettingsSnapshot,
    ) -> Result<()> {
        if messages.is_empty() {
            log::debug!("Dropping empty RudderStack telemetry batch");
            return Ok(());
        }

        if settings_snapshot.should_disable_telemetry() {
            log::info!("Not sending batched messages because telemetry is disabled.");
            return Ok(());
        }

        log::info!("Start to send telemetry events to RudderStack");

        let (mut messages_with_ugc, messages_without_ugc): (Vec<_>, Vec<_>) = messages
            .into_iter()
            .partition(|message| message.contains_ugc);

        // If we shouldn't collect UGC telemetry, forcibly clear any messages with UGC before trying to send.
        if !settings_snapshot.should_collect_ai_ugc_telemetry() {
            messages_with_ugc.clear();
        }

        for (messages, rudder_stack_destination) in [
            (
                messages_with_ugc,
                ChannelState::rudderstack_ugc_destination(),
            ),
            (
                messages_without_ugc,
                ChannelState::rudderstack_non_ugc_destination(),
            ),
        ] {
            if messages.is_empty() {
                continue;
            }

            // Note that timestamp and context are already included in the individual RudderBatchMessages
            // and these are the most important ones,
            // but we also add them to the RudderMessage::Batch wrapper.
            let rudder_message = RudderMessage::Batch(RudderBatch {
                batch: messages
                    .into_iter()
                    .map(|message| message.message)
                    .collect(),
                original_timestamp: Some(Utc::now()),
                ..Default::default()
            });
            if let Err(e) = self
                .send_rudder_request(rudder_message, rudder_stack_destination)
                .await
            {
                // Don't treat a connection issue as an error as these are outside of our control.
                //
                // This is only conditionally compiled because `is_connect` is not
                // available on wasm.  If additional checks are made against the
                // `reqwest::Error`, this condition should be performed specifically
                // against `is_connect` and not the whole loop.
                #[cfg(not(target_family = "wasm"))]
                for cause in e.chain() {
                    if let Some(err) = cause.downcast_ref::<reqwest::Error>() {
                        if err.is_connect() {
                            log::warn!("Failed to send event to RudderStack: {e}");
                            return Ok(());
                        }
                    }
                }
                return Err(e);
            }
        }
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
