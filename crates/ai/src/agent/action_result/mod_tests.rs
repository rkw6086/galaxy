use super::{
    AIAgentActionResultType, RunAgentsAgentOutcome, RunAgentsAgentOutcomeKind,
    RunAgentsLaunchedExecutionMode, RunAgentsResult, StartAgentResult, StartAgentVersion,
};

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
