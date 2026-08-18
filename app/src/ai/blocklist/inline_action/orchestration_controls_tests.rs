use ai::agent::action::RunAgentsExecutionMode;
use ai::agent::orchestration_config::{OrchestrationConfig, OrchestrationExecutionMode};

use super::{
    should_show_auth_secret_picker, should_show_harness_picker, AuthSecretSelection,
    OrchestrationEditState,
};

fn local_config(harness_type: &str, model_id: &str) -> OrchestrationConfig {
    OrchestrationConfig {
        model_id: model_id.to_string(),
        harness_type: harness_type.to_string(),
        execution_mode: OrchestrationExecutionMode::Local,
    }
}

fn remote_config(harness_type: &str, model_id: &str) -> OrchestrationConfig {
    OrchestrationConfig {
        model_id: model_id.to_string(),
        harness_type: harness_type.to_string(),
        execution_mode: OrchestrationExecutionMode::Remote {
            environment_id: "env-1".to_string(),
            worker_host: "warp".to_string(),
        },
    }
}

fn remote_mode() -> RunAgentsExecutionMode {
    RunAgentsExecutionMode::Remote {
        environment_id: "env-1".to_string(),
        worker_host: "warp".to_string(),
        computer_use_enabled: true,
    }
}

#[test]
fn run_agents_remote_mode_is_normalized_to_local() {
    let state = OrchestrationEditState::from_run_agents_fields("sonnet", "claude", &remote_mode());

    assert_eq!(state.harness_type, "claude");
    assert_eq!(state.model_id, "sonnet");
    assert!(matches!(
        state.execution_mode,
        RunAgentsExecutionMode::Local
    ));
    assert!(should_show_harness_picker(&state));
    assert!(!should_show_auth_secret_picker(&state));
}

#[test]
fn remote_orchestration_config_is_normalized_to_local() {
    let state =
        OrchestrationEditState::from_orchestration_config(&remote_config("claude", "sonnet"));

    assert_eq!(state.harness_type, "claude");
    assert_eq!(state.model_id, "sonnet");
    assert!(matches!(
        state.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn remote_toggle_remains_local() {
    let mut state = OrchestrationEditState::from_run_agents_fields(
        "auto",
        "oz",
        &RunAgentsExecutionMode::Local,
    );

    state.toggle_execution_mode_to_remote(true);

    assert!(matches!(
        state.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn local_normalization_sanitizes_disabled_harnesses() {
    let state = OrchestrationEditState::from_run_agents_fields("gpt-5", "codex", &remote_mode());

    assert_eq!(state.harness_type, "oz");
    assert_eq!(state.model_id, "");
    assert!(matches!(
        state.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn resolve_from_remote_config_inherits_fields_but_stays_local() {
    let mut state =
        OrchestrationEditState::from_run_agents_fields("", "", &RunAgentsExecutionMode::Local);

    state.resolve_from_config(&remote_config("claude", "sonnet"));

    assert_eq!(state.harness_type, "claude");
    assert_eq!(state.model_id, "sonnet");
    assert!(matches!(
        state.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn approved_remote_config_override_stays_local() {
    let mut state = OrchestrationEditState::from_run_agents_fields("auto", "oz", &remote_mode());

    state.override_from_approved_config(&remote_config("claude", "sonnet"));

    assert_eq!(state.harness_type, "claude");
    assert_eq!(state.model_id, "sonnet");
    assert!(matches!(
        state.execution_mode,
        RunAgentsExecutionMode::Local
    ));
    assert!(matches!(
        state.to_orchestration_config().execution_mode,
        OrchestrationExecutionMode::Local
    ));
}

#[test]
fn resolve_from_config_sanitizes_disabled_local_codex() {
    let mut state =
        OrchestrationEditState::from_run_agents_fields("", "", &RunAgentsExecutionMode::Local);

    state.resolve_from_config(&local_config("codex", "gpt-5"));

    assert_eq!(state.harness_type, "oz");
    assert_eq!(state.model_id, "");
    assert_eq!(state.accept_disabled_reason(), None);
}

#[test]
fn local_mode_does_not_expose_managed_auth_secret() {
    let mut state = OrchestrationEditState::from_run_agents_fields(
        "sonnet",
        "claude",
        &RunAgentsExecutionMode::Local,
    );
    state.auth_secret_selection = AuthSecretSelection::Named("my-key".to_string());

    assert_eq!(state.auth_secret_name(), None);
    assert!(!should_show_auth_secret_picker(&state));
}

#[test]
fn selecting_create_auth_secret_remains_a_distinct_state() {
    let mut state = OrchestrationEditState::from_run_agents_fields(
        "sonnet",
        "claude",
        &RunAgentsExecutionMode::Local,
    );
    state.auth_secret_selection = AuthSecretSelection::Inherit;

    state.select_create_new_auth_secret();

    assert!(matches!(
        state.auth_secret_selection,
        AuthSecretSelection::CreatingNew
    ));
}
