use std::path::PathBuf;

use ai::agent::action::{RunAgentsAgentRunConfig, RunAgentsExecutionMode, RunAgentsRequest};
use ai::agent::action_result::{
    RunAgentsAgentOutcome, RunAgentsAgentOutcomeKind, RunAgentsLaunchedExecutionMode,
    RunAgentsResult,
};
use ai::skills::SkillReference;
use warp_util::local_or_remote_path::LocalOrRemotePath;

use super::{
    has_run_agents_child, link_run_agents_child, mark_run_agents_child_removed,
    run_agents_event_matches_card, sync_run_agents_children, RunAgentsChildState,
    RunAgentsEditState, RunAgentsExecutorEvent,
};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::AIAgentActionId;
use crate::ai::blocklist::inline_action::orchestration_controls::OrchestrationEditState;

fn make_request(harness: &str, mode: RunAgentsExecutionMode) -> RunAgentsRequest {
    make_request_with_skills(harness, mode, Vec::new())
}

fn make_request_with_skills(
    harness: &str,
    mode: RunAgentsExecutionMode,
    skills: Vec<SkillReference>,
) -> RunAgentsRequest {
    RunAgentsRequest {
        summary: "summary".to_string(),
        base_prompt: "base".to_string(),
        skills,
        model_id: "auto".to_string(),
        harness_type: harness.to_string(),
        execution_mode: mode,
        agent_run_configs: vec![RunAgentsAgentRunConfig {
            name: "child".to_string(),
            prompt: "do work".to_string(),
            title: "Child agent".to_string(),
            model_id: "auto".to_string(),
        }],
        plan_id: String::new(),
        harness_auth_secret_name: None,
    }
}

fn make_edit_state_with_orch_fields(
    harness: &str,
    mode: RunAgentsExecutionMode,
) -> RunAgentsEditState {
    let request = make_request(harness, mode);
    RunAgentsEditState {
        orch: OrchestrationEditState::from_run_agents_fields(
            &request.model_id,
            &request.harness_type,
            &request.execution_mode,
        ),
        agent_run_configs: request.agent_run_configs,
        base_prompt: request.base_prompt,
        summary: request.summary,
        skills: request.skills,
        plan_id: request.plan_id,
    }
}

#[test]
fn remote_toggle_remains_local() {
    let mut state =
        RunAgentsEditState::from_request(&make_request("oz", RunAgentsExecutionMode::Local));

    state.orch.toggle_execution_mode_to_remote(true);

    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn legacy_remote_request_normalizes_to_local() {
    let mut state = RunAgentsEditState::from_request(&make_request(
        "oz",
        RunAgentsExecutionMode::Remote {
            environment_id: "env-1".to_string(),
            worker_host: "warp".to_string(),
            computer_use_enabled: false,
        },
    ));
    state.orch.toggle_execution_mode_to_remote(false);
    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn remote_toggle_preserves_supported_local_harness() {
    let mut state =
        RunAgentsEditState::from_request(&make_request("opencode", RunAgentsExecutionMode::Local));

    state.orch.toggle_execution_mode_to_remote(true);

    assert_eq!(state.orch.harness_type, "opencode");
    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn legacy_remote_request_without_environment_allows_local_acceptance() {
    let state = RunAgentsEditState::from_request(&make_request(
        "oz",
        RunAgentsExecutionMode::Remote {
            environment_id: String::new(),
            worker_host: "warp".to_string(),
            computer_use_enabled: false,
        },
    ));
    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
    assert!(state.orch.accept_disabled_reason().is_none());
}

#[test]
fn legacy_remote_opencode_request_is_normalized_and_allowed_locally() {
    let state = RunAgentsEditState::from_request(&make_request(
        "opencode",
        RunAgentsExecutionMode::Remote {
            environment_id: "env-1".to_string(),
            worker_host: "warp".to_string(),
            computer_use_enabled: false,
        },
    ));

    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
    assert_eq!(state.orch.accept_disabled_reason(), None);
}

#[test]
fn local_with_any_harness_does_not_disable_accept() {
    for harness in ["oz", "claude", "gemini", "opencode"] {
        let state =
            RunAgentsEditState::from_request(&make_request(harness, RunAgentsExecutionMode::Local));
        assert!(
            state.orch.accept_disabled_reason().is_none(),
            "Local + {harness} should allow Accept"
        );
    }
}

#[test]
fn local_with_disabled_codex_is_sanitized() {
    let state = make_edit_state_with_orch_fields("codex", RunAgentsExecutionMode::Local);

    assert_eq!(state.orch.harness_type, "oz");
    assert_eq!(state.orch.accept_disabled_reason(), None);
}

#[test]
fn from_request_sanitizes_disabled_local_harness_to_oz() {
    let state =
        RunAgentsEditState::from_request(&make_request("codex", RunAgentsExecutionMode::Local));

    assert_eq!(state.orch.harness_type, "oz");
    assert_eq!(state.orch.model_id, "");
    assert!(state.orch.accept_disabled_reason().is_none());
}

#[test]
fn legacy_remote_harnesses_normalize_and_allow_local_acceptance() {
    for harness in ["oz", "claude", "gemini"] {
        let state = RunAgentsEditState::from_request(&make_request(
            harness,
            RunAgentsExecutionMode::Remote {
                environment_id: "env-1".to_string(),
                worker_host: "warp".to_string(),
                computer_use_enabled: false,
            },
        ));
        assert!(matches!(
            state.orch.execution_mode,
            RunAgentsExecutionMode::Local
        ));
        assert!(
            state.orch.accept_disabled_reason().is_none(),
            "normalized local + {harness} should allow Accept"
        );
    }
}

#[test]
fn set_environment_id_no_op_in_local_mode() {
    let mut state =
        RunAgentsEditState::from_request(&make_request("oz", RunAgentsExecutionMode::Local));
    state.orch.set_environment_id("env-1".to_string());
    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn set_environment_id_is_ignored_for_normalized_remote_request() {
    let mut state = RunAgentsEditState::from_request(&make_request(
        "oz",
        RunAgentsExecutionMode::Remote {
            environment_id: "old".to_string(),
            worker_host: "warp".to_string(),
            computer_use_enabled: false,
        },
    ));

    state.orch.set_environment_id("new-env".to_string());

    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}

#[test]
fn to_request_preserves_fields_but_normalizes_execution_to_local() {
    let mut req = make_request_with_skills(
        "claude",
        RunAgentsExecutionMode::Remote {
            environment_id: "env-2".to_string(),
            worker_host: "warp".to_string(),
            computer_use_enabled: true,
        },
        vec![
            SkillReference::BundledSkillId("writing-pr-descriptions".to_string()),
            SkillReference::Path(LocalOrRemotePath::Local(PathBuf::from(
                "/tmp/skill/SKILL.md",
            ))),
        ],
    );
    req.plan_id = "plan-1".to_string();
    let state = RunAgentsEditState::from_request(&req);
    let round_tripped = state.to_request();

    assert_eq!(round_tripped.summary, req.summary);
    assert_eq!(round_tripped.base_prompt, req.base_prompt);
    assert_eq!(round_tripped.model_id, req.model_id);
    assert_eq!(round_tripped.harness_type, req.harness_type);
    assert!(matches!(
        round_tripped.execution_mode,
        RunAgentsExecutionMode::Local
    ));
    assert_eq!(round_tripped.agent_run_configs, req.agent_run_configs);
    assert_eq!(round_tripped.skills, req.skills);
    assert_eq!(round_tripped.plan_id, req.plan_id);
}

#[test]
fn legacy_child_without_model_inherits_the_run_model() {
    let mut request = make_request("oz", RunAgentsExecutionMode::Local);
    request.model_id = "fallback-model".to_string();
    request.agent_run_configs[0].model_id.clear();

    let state = RunAgentsEditState::from_request(&request);

    assert_eq!(state.agent_run_configs[0].model_id, "fallback-model");
    assert_eq!(
        state.to_request().agent_run_configs[0].model_id,
        "fallback-model"
    );
}

#[test]
fn live_child_links_and_removal_survive_streaming_config_sync() {
    let first_id = AIConversationId::new();
    let replacement_id = AIConversationId::new();
    let mut children = vec![
        RunAgentsChildState::new("alpha".to_string()),
        RunAgentsChildState::new("beta".to_string()),
    ];

    assert!(link_run_agents_child(&mut children, "alpha", first_id));
    assert!(has_run_agents_child(&children, first_id));
    assert!(mark_run_agents_child_removed(&mut children, first_id));
    assert!(children[0].removed);

    let configs = vec![
        RunAgentsAgentRunConfig {
            name: "gamma".to_string(),
            prompt: "new work".to_string(),
            title: String::new(),
            model_id: "model-gamma".to_string(),
        },
        RunAgentsAgentRunConfig {
            name: "alpha".to_string(),
            prompt: "updated work".to_string(),
            title: String::new(),
            model_id: "model-alpha".to_string(),
        },
    ];
    sync_run_agents_children(&mut children, &configs);

    assert_eq!(
        children
            .iter()
            .map(|child| child.name.as_str())
            .collect::<Vec<_>>(),
        vec!["gamma", "alpha"]
    );
    assert_eq!(children[1].conversation_id, Some(first_id));
    assert!(children[1].removed);
    assert!(link_run_agents_child(
        &mut children,
        "alpha",
        replacement_id
    ));
    assert_eq!(children[1].conversation_id, Some(replacement_id));
    assert!(!children[1].removed);
    assert!(!link_run_agents_child(
        &mut children,
        "missing",
        AIConversationId::new()
    ));
}

#[test]
fn child_created_with_duplicate_action_id_only_matches_parent_conversation() {
    let card_conversation_id = AIConversationId::new();
    let other_conversation_id = AIConversationId::new();
    let child_conversation_id = AIConversationId::new();
    let duplicate_action_id = AIAgentActionId::from("duplicate-action".to_string());
    let event = RunAgentsExecutorEvent::ChildConversationCreated {
        action_id: duplicate_action_id.clone(),
        agent_name: "child".to_string(),
        parent_conversation_id: other_conversation_id,
        child_conversation_id,
    };

    assert!(!run_agents_event_matches_card(
        &event,
        Some(card_conversation_id),
        &duplicate_action_id,
    ));
    assert!(run_agents_event_matches_card(
        &event,
        Some(other_conversation_id),
        &duplicate_action_id,
    ));
}

mod format_terminal_state_tests {
    use super::super::{format_terminal_state, StatusKind};
    use super::*;

    fn launched(name: &str, agent_id: &str) -> RunAgentsAgentOutcome {
        RunAgentsAgentOutcome {
            name: name.to_string(),
            kind: RunAgentsAgentOutcomeKind::Launched {
                agent_id: agent_id.to_string(),
            },
        }
    }

    fn failed(name: &str, error: &str) -> RunAgentsAgentOutcome {
        RunAgentsAgentOutcome {
            name: name.to_string(),
            kind: RunAgentsAgentOutcomeKind::Failed {
                error: error.to_string(),
            },
        }
    }

    fn completed(name: &str, agent_id: &str) -> RunAgentsAgentOutcome {
        RunAgentsAgentOutcome {
            name: name.to_string(),
            kind: RunAgentsAgentOutcomeKind::Completed {
                agent_id: agent_id.to_string(),
                output: format!("{name} output"),
            },
        }
    }

    fn launched_result(agents: Vec<RunAgentsAgentOutcome>) -> RunAgentsResult {
        RunAgentsResult::Launched {
            model_id: "auto".to_string(),
            harness_type: "oz".to_string(),
            execution_mode: RunAgentsLaunchedExecutionMode::Local,
            agents,
        }
    }

    #[test]
    fn launched_singular_uses_singular_label() {
        let result = launched_result(vec![launched("child", "a-1")]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Spawned 1 agent");
        assert!(matches!(kind, StatusKind::Success));
    }

    #[test]
    fn launched_plural_uses_plural_label() {
        let result = launched_result(vec![
            launched("a", "a-1"),
            launched("b", "a-2"),
            launched("c", "a-3"),
        ]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Spawned 3 agents");
        assert!(matches!(kind, StatusKind::Success));
    }

    #[test]
    fn launched_partial_uses_x_of_y_label_and_mixed_status() {
        let result = launched_result(vec![
            launched("a", "a-1"),
            failed("b", "boom"),
            launched("c", "a-3"),
        ]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Spawned 2 of 3 agents");
        assert!(matches!(kind, StatusKind::Mixed));
    }

    #[test]
    fn all_completed_uses_completed_label_and_success_status() {
        let result = launched_result(vec![
            completed("a", "a-1"),
            completed("b", "a-2"),
            completed("c", "a-3"),
        ]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Completed 3 agents");
        assert!(matches!(kind, StatusKind::Success));
    }

    #[test]
    fn mixed_completed_and_failed_uses_completed_label_and_mixed_status() {
        let result = launched_result(vec![
            completed("a", "a-1"),
            failed("b", "boom"),
            completed("c", "a-3"),
        ]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Completed 2 of 3 agents");
        assert!(matches!(kind, StatusKind::Mixed));
    }

    #[test]
    fn all_failed_uses_failure_status_not_mixed() {
        let result = launched_result(vec![
            failed("a", "boom"),
            failed("b", "boom"),
            failed("c", "boom"),
        ]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Failed to spawn 3 agents");
        assert!(matches!(kind, StatusKind::Failure));
    }

    #[test]
    fn single_failed_uses_singular_failure_label() {
        let result = launched_result(vec![failed("a", "boom")]);
        let (label, kind) = format_terminal_state(&result);
        assert_eq!(label, "Failed to spawn agent");
        assert!(matches!(kind, StatusKind::Failure));
    }

    #[test]
    fn failure_with_error_includes_error_text() {
        let (label, kind) = format_terminal_state(&RunAgentsResult::Failure {
            error: "server rejected request".to_string(),
        });
        assert_eq!(
            label,
            "Failed to start orchestration: server rejected request"
        );
        assert!(matches!(kind, StatusKind::Failure));
    }

    #[test]
    fn failure_with_empty_error_uses_short_label() {
        let (label, kind) = format_terminal_state(&RunAgentsResult::Failure {
            error: String::new(),
        });
        assert_eq!(label, "Failed to start orchestration");
        assert!(matches!(kind, StatusKind::Failure));
    }

    #[test]
    fn denied_with_reason_appends_reason() {
        let (label, kind) = format_terminal_state(&RunAgentsResult::Denied {
            reason: "disapproved".to_string(),
        });
        assert!(label.contains("disapproved"));
        assert!(matches!(kind, StatusKind::Cancelled));
    }

    #[test]
    fn denied_without_reason_uses_short_label() {
        let (label, kind) = format_terminal_state(&RunAgentsResult::Denied {
            reason: String::new(),
        });
        assert!(!label.contains("()"));
        assert!(matches!(kind, StatusKind::Cancelled));
    }

    #[test]
    fn cancelled_uses_cancelled_status() {
        let (label, kind) = format_terminal_state(&RunAgentsResult::Cancelled);
        assert_eq!(label, "Spawn agents cancelled");
        assert!(matches!(kind, StatusKind::Cancelled));
    }
}

mod override_from_approved_config_tests {
    use ai::agent::action::RunAgentsExecutionMode;
    use ai::agent::orchestration_config::{OrchestrationConfig, OrchestrationExecutionMode};

    use super::super::RunAgentsEditState;
    use super::*;

    fn local_config(model: &str, harness: &str) -> OrchestrationConfig {
        OrchestrationConfig {
            model_id: model.to_string(),
            harness_type: harness.to_string(),
            execution_mode: OrchestrationExecutionMode::Local,
        }
    }

    fn remote_config(model: &str, harness: &str, env: &str) -> OrchestrationConfig {
        OrchestrationConfig {
            model_id: model.to_string(),
            harness_type: harness.to_string(),
            execution_mode: OrchestrationExecutionMode::Remote {
                environment_id: env.to_string(),
                worker_host: "warp".to_string(),
            },
        }
    }

    #[test]
    fn overrides_model_and_harness_unconditionally() {
        let mut state =
            RunAgentsEditState::from_request(&make_request("oz", RunAgentsExecutionMode::Local));
        assert_eq!(state.orch.model_id, "auto");
        assert_eq!(state.orch.harness_type, "oz");

        state
            .orch
            .override_from_approved_config(&local_config("claude-4-opus", "claude"));
        assert_eq!(state.orch.model_id, "claude-4-opus");
        assert_eq!(state.orch.harness_type, "claude");
    }

    #[test]
    fn overrides_even_when_request_has_values() {
        let mut state =
            RunAgentsEditState::from_request(&make_request("oz", RunAgentsExecutionMode::Local));
        state
            .orch
            .override_from_approved_config(&local_config("sonnet", "claude"));
        assert_eq!(state.orch.model_id, "sonnet");
        assert_eq!(state.orch.harness_type, "claude");
    }

    #[test]
    fn remote_config_override_stays_local() {
        let mut state =
            RunAgentsEditState::from_request(&make_request("oz", RunAgentsExecutionMode::Local));
        state
            .orch
            .override_from_approved_config(&remote_config("auto", "oz", "env-1"));

        assert!(matches!(
            state.orch.execution_mode,
            RunAgentsExecutionMode::Local
        ));
    }

    #[test]
    fn overrides_remote_to_local() {
        let mut state = RunAgentsEditState::from_request(&make_request(
            "oz",
            RunAgentsExecutionMode::Remote {
                environment_id: "env-1".to_string(),
                worker_host: "warp".to_string(),
                computer_use_enabled: true,
            },
        ));
        state
            .orch
            .override_from_approved_config(&local_config("auto", "oz"));
        assert!(
            matches!(state.orch.execution_mode, RunAgentsExecutionMode::Local),
            "should be Local after override"
        );
    }

    #[test]
    fn remote_request_and_remote_override_drop_computer_use() {
        let mut state = RunAgentsEditState::from_request(&make_request(
            "oz",
            RunAgentsExecutionMode::Remote {
                environment_id: "old-env".to_string(),
                worker_host: "warp".to_string(),
                computer_use_enabled: true,
            },
        ));
        state
            .orch
            .override_from_approved_config(&remote_config("auto", "oz", "new-env"));

        assert!(matches!(
            state.orch.execution_mode,
            RunAgentsExecutionMode::Local
        ));
    }

    #[test]
    fn approved_local_disabled_harness_is_sanitized() {
        let mut state =
            RunAgentsEditState::from_request(&make_request("oz", RunAgentsExecutionMode::Local));
        state
            .orch
            .override_from_approved_config(&local_config("gpt-5", "codex"));

        assert_eq!(state.orch.harness_type, "oz");
        assert_eq!(state.orch.model_id, "");
        assert_eq!(state.orch.accept_disabled_reason(), None);
    }
}

#[test]
fn remote_toggle_is_idempotently_local() {
    let mut state = RunAgentsEditState::from_request(&make_request(
        "oz",
        RunAgentsExecutionMode::Remote {
            environment_id: "env-1".to_string(),
            worker_host: "warp".to_string(),
            computer_use_enabled: true,
        },
    ));

    state.orch.toggle_execution_mode_to_remote(true);

    assert!(matches!(
        state.orch.execution_mode,
        RunAgentsExecutionMode::Local
    ));
}
