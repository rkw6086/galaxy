//! Bounded operational evidence retained until a provider run finishes.
//!
//! A failed telemetry POST must not be the only copy of a model failure. The
//! terminal event includes these summaries even when the run later recovers.
//! This is process-local diagnostic state, never provider history/checkpoints.

use std::collections::VecDeque;

use chrono::Utc;
use galaxy_agent_core::AgentError;
use serde_json::{json, Value};

use crate::ai::remote_logging::sanitize_error;

const MAX_FAILURES: usize = 8;
const MAX_ID_CHARS: usize = 256;

#[derive(Default)]
pub(crate) struct ModelFailureDiagnostics {
    observed_count: u64,
    recent: VecDeque<Value>,
}

impl ModelFailureDiagnostics {
    pub(crate) fn record(&mut self, identity: Value, error: &AgentError) {
        self.observed_count = self.observed_count.saturating_add(1);
        // Select operational identifiers explicitly; never retain a request,
        // prompt, provider configuration, or arbitrary caller-supplied context.
        let mut record = json!({
            "timestamp": Utc::now().to_rfc3339(),
            "error_kind": format!("{:?}", error.kind),
            "error_recoverable": error.recoverable,
            "error": sanitize_error(&error.message),
        });
        for key in ["provider_run_id", "profile", "runtime_id", "model_id"] {
            if let Some(value) = identity.get(key).and_then(Value::as_str) {
                record[key] = json!(value.chars().take(MAX_ID_CHARS).collect::<String>());
            }
        }
        for key in ["provider_epoch", "retry_attempt", "elapsed_ms"] {
            if let Some(value) = identity.get(key).and_then(Value::as_u64) {
                record[key] = json!(value);
            }
        }
        if self.recent.len() == MAX_FAILURES {
            self.recent.pop_front();
        }
        self.recent.push_back(record);
    }

    pub(crate) fn snapshot(&self) -> Value {
        json!({
            "scope": "current_process",
            "observed_count": self.observed_count,
            "truncated": self.observed_count > self.recent.len() as u64,
            "recent": self.recent,
        })
    }
}

#[cfg(test)]
#[path = "model_failure_diagnostics_tests.rs"]
mod tests;
