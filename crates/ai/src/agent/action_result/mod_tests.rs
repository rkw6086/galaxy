use super::{
    AIAgentActionResultType, RequestCommandOutputResult, RunAgentsAgentOutcome,
    RunAgentsAgentOutcomeKind, RunAgentsLaunchedExecutionMode, RunAgentsResult, StartAgentResult,
    StartAgentVersion,
};

#[test]
fn shell_execution_error_is_failed_but_not_cancelled() {
    let result =
        AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::ExecutionError {
            command: "cargo test".to_string(),
            message: "terminal is busy".to_string(),
        });

    assert!(result.is_failed());
    assert!(!result.is_cancelled());
    assert!(result.model_content().contains("terminal is busy"));
}

#[test]
fn deserializes_legacy_start_agent_success_without_version_as_v1() {
    let result: StartAgentResult =
        serde_json::from_value(serde_json::json!({ "Success": { "agent_id": "agent-1" } }))
            .expect("legacy start-agent success should deserialize");

    assert_eq!(
        result,
        StartAgentResult::Success {
            agent_id: "agent-1".to_string(),
            version: StartAgentVersion::V1,
        }
    );
}

#[test]
fn deserializes_legacy_start_agent_error_without_version_as_v1() {
    let result: StartAgentResult =
        serde_json::from_value(serde_json::json!({ "Error": { "error": "boom" } }))
            .expect("legacy start-agent error should deserialize");

    assert_eq!(
        result,
        StartAgentResult::Error {
            error: "boom".to_string(),
            version: StartAgentVersion::V1,
        }
    );
}

#[test]
fn deserializes_legacy_start_agent_cancelled_without_version_as_v1() {
    let result: StartAgentResult = serde_json::from_value(serde_json::json!({ "Cancelled": {} }))
        .expect("legacy start-agent cancellation should deserialize");

    assert_eq!(
        result,
        StartAgentResult::Cancelled {
            version: StartAgentVersion::V1,
        }
    );
}

#[test]
fn run_agents_model_content_contains_resolved_config_and_agent_outcomes() {
    let result = AIAgentActionResultType::RunAgents(RunAgentsResult::Launched {
        model_id: "resolved-model".to_string(),
        harness_type: "oz".to_string(),
        execution_mode: RunAgentsLaunchedExecutionMode::Remote {
            environment_id: "env-1".to_string(),
            worker_host: "worker.example".to_string(),
            computer_use_enabled: true,
        },
        agents: vec![
            RunAgentsAgentOutcome {
                name: "research".to_string(),
                kind: RunAgentsAgentOutcomeKind::Launched {
                    agent_id: "agent-1".to_string(),
                },
            },
            RunAgentsAgentOutcome {
                name: "tests".to_string(),
                kind: RunAgentsAgentOutcomeKind::Failed {
                    error: "capacity exhausted".to_string(),
                },
            },
        ],
    });

    let content: serde_json::Value = serde_json::from_str(&result.model_content())
        .expect("run-agents model content should be valid JSON");
    assert_eq!(
        content,
        serde_json::json!({
            "status": "launched",
            "completion_state": "children_running",
            "instruction": "Child agents have only been launched, not completed. Wait for child-agent updates before repeating this work or reporting final results.",
            "model_id": "resolved-model",
            "harness_type": "oz",
            "execution_mode": {
                "type": "remote",
                "environment_id": "env-1",
                "worker_host": "worker.example",
                "computer_use_enabled": true,
            },
            "agents": [
                {
                    "name": "research",
                    "status": "launched",
                    "agent_id": "agent-1",
                },
                {
                    "name": "tests",
                    "status": "failed",
                    "error": "capacity exhausted",
                },
            ],
        })
    );
    assert_eq!(
        result.to_string(),
        "Orchestrate launched (1/2 agents started)"
    );
}

#[test]
fn run_agents_model_content_serializes_terminal_non_launch_outcomes() {
    for (result, expected) in [
        (
            RunAgentsResult::Denied {
                reason: "not approved".to_string(),
            },
            serde_json::json!({ "status": "denied", "reason": "not approved" }),
        ),
        (
            RunAgentsResult::Failure {
                error: "invalid request".to_string(),
            },
            serde_json::json!({ "status": "failure", "error": "invalid request" }),
        ),
        (
            RunAgentsResult::Cancelled,
            serde_json::json!({ "status": "cancelled" }),
        ),
    ] {
        let result = AIAgentActionResultType::RunAgents(result);
        let content: serde_json::Value = serde_json::from_str(&result.model_content())
            .expect("run-agents model content should be valid JSON");
        assert_eq!(content, expected);
    }
}

#[test]
fn completed_local_run_agents_preserves_outputs_and_terminal_state() {
    let result = AIAgentActionResultType::RunAgents(RunAgentsResult::Launched {
        model_id: "model".to_string(),
        harness_type: "codex".to_string(),
        execution_mode: RunAgentsLaunchedExecutionMode::Local,
        agents: vec![RunAgentsAgentOutcome {
            name: "research".to_string(),
            kind: RunAgentsAgentOutcomeKind::Completed {
                agent_id: "child-1".to_string(),
                output: "Found the root cause".to_string(),
            },
        }],
    });

    let content: serde_json::Value = serde_json::from_str(&result.model_content()).unwrap();
    assert_eq!(content["status"], "launched");
    assert_eq!(content["completion_state"], "children_completed");
    assert_eq!(content["agents"][0]["status"], "completed");
    assert_eq!(content["agents"][0]["output"], "Found the root cause");
    assert!(result.is_successful());
    assert!(!result.is_failed());
}

#[test]
fn all_failed_run_agents_is_failure_but_mixed_batch_is_successful() {
    let failed = |name: &str, error: &str| RunAgentsAgentOutcome {
        name: name.to_string(),
        kind: RunAgentsAgentOutcomeKind::Failed {
            error: error.to_string(),
        },
    };
    let result = AIAgentActionResultType::RunAgents(RunAgentsResult::Launched {
        model_id: "model".to_string(),
        harness_type: "codex".to_string(),
        execution_mode: RunAgentsLaunchedExecutionMode::Local,
        agents: vec![failed("one", "first error"), failed("two", "second error")],
    });

    let content: serde_json::Value = serde_json::from_str(&result.model_content()).unwrap();
    assert_eq!(content["status"], "failure");
    assert_eq!(content["agents"][0]["error"], "first error");
    assert_eq!(content["agents"][1]["error"], "second error");
    assert!(result.is_failed());
    assert!(!result.is_successful());

    let mixed = AIAgentActionResultType::RunAgents(RunAgentsResult::Launched {
        model_id: "model".to_string(),
        harness_type: "codex".to_string(),
        execution_mode: RunAgentsLaunchedExecutionMode::Local,
        agents: vec![
            failed("one", "first error"),
            RunAgentsAgentOutcome {
                name: "two".to_string(),
                kind: RunAgentsAgentOutcomeKind::Completed {
                    agent_id: "child-2".to_string(),
                    output: "useful output".to_string(),
                },
            },
        ],
    });
    let mixed_content: serde_json::Value = serde_json::from_str(&mixed.model_content()).unwrap();
    assert_eq!(mixed_content["status"], "launched");
    assert_eq!(mixed_content["agents"][0]["error"], "first error");
    assert_eq!(mixed_content["agents"][1]["output"], "useful output");
    assert!(mixed.is_successful());
    assert!(!mixed.is_failed());
}
