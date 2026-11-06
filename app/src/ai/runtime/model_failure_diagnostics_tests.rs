use galaxy_agent_core::AgentErrorKind;

use super::*;

#[test]
fn retains_sanitized_causes_after_recovery_without_request_content() {
    let mut diagnostics = ModelFailureDiagnostics::default();
    let mut error = AgentError::new(AgentErrorKind::Transport, "sk-private connection failed");
    error.recoverable = true;
    diagnostics.record(
        json!({
            "provider_run_id": "run-1", "provider_epoch": 6,
            "retry_attempt": 0, "elapsed_ms": 100,
            "model_id": "test-model", "runtime_id": "rig:openai", "profile": "base",
            "prompt": "private prompt", "api_key": "private credential",
        }),
        &error,
    );
    let snapshot = diagnostics.snapshot();
    let serialized = snapshot.to_string();
    assert_eq!(snapshot["observed_count"], 1);
    assert_eq!(
        snapshot["recent"][0]["error"],
        "[redacted] connection failed"
    );
    assert_eq!(snapshot["recent"][0]["error_kind"], "Transport");
    assert_eq!(snapshot["recent"][0]["provider_epoch"], 6);
    assert_eq!(snapshot["recent"][0]["error_recoverable"], true);
    assert!(!serialized.contains("private prompt"));
    assert!(!serialized.contains("private credential"));
    assert!(!serialized.contains("sk-private"));
}

#[test]
fn retention_keeps_newest_failures_with_explicit_omission_accounting() {
    let mut diagnostics = ModelFailureDiagnostics::default();
    for epoch in 0..MAX_FAILURES + 2 {
        diagnostics.record(
            json!({"provider_epoch": epoch, "model_id": "é".repeat(10_000)}),
            &AgentError::new(AgentErrorKind::Provider, "overloaded"),
        );
    }
    let snapshot = diagnostics.snapshot();
    assert_eq!(snapshot["scope"], "current_process");
    assert_eq!(snapshot["observed_count"], MAX_FAILURES as u64 + 2);
    assert_eq!(snapshot["truncated"], true);
    let recent = snapshot["recent"].as_array().unwrap();
    assert_eq!(recent.len(), MAX_FAILURES);
    assert_eq!(recent[0]["provider_epoch"], 2);
    assert_eq!(
        recent[0]["model_id"].as_str().unwrap().chars().count(),
        MAX_ID_CHARS
    );
    assert_eq!(
        ModelFailureDiagnostics::default().snapshot()["observed_count"],
        0
    );
}
