use serde_json::{json, Value};

pub struct TelemetryContext(Value);

impl TelemetryContext {
    pub fn as_value(&self) -> Value {
        self.0.clone()
    }
}

pub fn telemetry_context() -> &'static TelemetryContext {
    use std::sync::OnceLock;
    static TELEMETRY_CONTEXT: OnceLock<TelemetryContext> = OnceLock::new();
    TELEMETRY_CONTEXT.get_or_init(|| TelemetryContext(json!({})))
}
