#![allow(dead_code)]

//! Module that builds a static context to attach to each of our events that are sent to Rudderstack.
//! This is needed so we know the backing operating system and version of each telemetry event.

use std::sync::OnceLock;

use galaxy_core::operating_system_info::OperatingSystemInfo;
#[cfg(target_family = "wasm")]
use galaxyui::platform::wasm;
use serde::Serialize;
use serde_json::{json, Value};

use super::rudder_message::Message as RudderMessage;

static TELEMETRY_CONTEXT: OnceLock<TelemetryContext> = OnceLock::new();

#[derive(Serialize)]
struct TelemetryContextInfo {
    /// Info about the operating system of the client.
    #[serde(skip_serializing_if = "Option::is_none")]
    os: Option<&'static OperatingSystemInfo>,
    /// The user agent provided by the browser, if running on Web. If not on
    /// Web, this is always `None`.
    #[serde(rename = "userAgent", skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
}

/// Newtype representing a [`Value`] with a serialized version of the context that we send to
/// Rudderstack.
/// See https://www.rudderstack.com/docs/event-spec/standard-events/common-fields/#contextual-fields.
pub struct TelemetryContext(Value);

impl TelemetryContext {
    pub fn as_value(&self) -> Value {
        self.0.clone()
    }
}

pub fn telemetry_context() -> &'static TelemetryContext {
    static TELEMETRY_CONTEXT: OnceLock<TelemetryContext> = OnceLock::new();
    TELEMETRY_CONTEXT.get_or_init(|| TelemetryContext(json!({})))
}
