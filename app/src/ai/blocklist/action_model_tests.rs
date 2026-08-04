use std::collections::HashMap;
use std::sync::Arc;

use super::*;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentActionResultType, AnyFileContent, FileContext, GrepResult, ReadFilesResult,
};

fn make_action_result(id: &str) -> Arc<AIAgentActionResult> {
    Arc::new(AIAgentActionResult {
        id: AIAgentActionId::from(id.to_owned()),
        task_id: TaskId::new("task".to_owned()),
        result: AIAgentActionResultType::InitProject,
    })
}

fn action_result(id: &str, result: AIAgentActionResultType) -> AIAgentActionResult {
    AIAgentActionResult {
        id: AIAgentActionId::from(id.to_owned()),
        task_id: TaskId::new("task".to_owned()),
        result,
    }
}

fn count_startable_actions_for_pass(phases: &[(RunningActionPhase, bool)]) -> usize {
    let mut current_phase = None;
    let mut count = 0;

    for (phase, can_autoexecute) in phases {
        if let Some(current_phase) = current_phase {
            if !can_start_action_with_current_phase(current_phase, *phase, *can_autoexecute) {
                break;
            }
        }

        count += 1;
        current_phase = Some(*phase);

        if matches!(*phase, RunningActionPhase::Serial) {
            break;
        }
    }

    count
}

#[test]
fn parallel_phase_only_admits_matching_autoexecutable_actions() {
    let phase =
        RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext);

    assert!(can_start_action_with_current_phase(phase, phase, true));
    assert!(!can_start_action_with_current_phase(phase, phase, false));
    assert!(!can_start_action_with_current_phase(
        phase,
        RunningActionPhase::Serial,
        true
    ));
    assert!(!can_start_action_with_current_phase(
        RunningActionPhase::Serial,
        phase,
        true
    ));
}

#[test]
fn phased_scheduling_stops_at_serial_barrier_and_resumes_afterward() {
    let read_only_phase =
        RunningActionPhase::Parallel(execute::ParallelExecutionPolicy::ReadOnlyLocalContext);
    let actions = vec![
        (read_only_phase, true),
        (read_only_phase, true),
        (RunningActionPhase::Serial, true),
        (read_only_phase, true),
        (read_only_phase, true),
    ];

    assert_eq!(count_startable_actions_for_pass(&actions), 2);
    assert_eq!(count_startable_actions_for_pass(&actions[2..]), 1);
    assert_eq!(count_startable_actions_for_pass(&actions[3..]), 2);
}

#[test]
fn finished_results_stay_in_original_action_order() {
    let action_order = HashMap::from([
        (AIAgentActionId::from("first".to_owned()), 0),
        (AIAgentActionId::from("second".to_owned()), 1),
        (AIAgentActionId::from("third".to_owned()), 2),
    ]);
    let mut finished_results = [
        make_action_result("third"),
        make_action_result("first"),
        make_action_result("second"),
    ];

    finished_results
        .sort_by_key(|result| action_order.get(&result.id).copied().unwrap_or(usize::MAX));

    assert_eq!(
        finished_results[0].id,
        AIAgentActionId::from("first".to_owned())
    );
    assert_eq!(
        finished_results[1].id,
        AIAgentActionId::from("second".to_owned())
    );
    assert_eq!(
        finished_results[2].id,
        AIAgentActionId::from("third".to_owned())
    );
}

#[test]
fn domain_tool_results_preserve_success_failure_cancellation_and_denial() {
    let success = domain_tool_result(
        &action_result("success", AIAgentActionResultType::InitProject),
        false,
    );
    let failure = domain_tool_result(
        &action_result(
            "failure",
            AIAgentActionResultType::Grep(GrepResult::Error("boom".to_string())),
        ),
        false,
    );
    let cancelled_result = action_result(
        "cancelled",
        AIAgentActionResultType::Grep(GrepResult::Cancelled),
    );
    let cancelled = domain_tool_result(&cancelled_result, false);
    let denied = domain_tool_result(&cancelled_result, true);

    assert_eq!(success.status, ToolResultStatus::Success);
    assert_eq!(failure.status, ToolResultStatus::Error);
    assert_eq!(cancelled.status, ToolResultStatus::Cancelled);
    assert_eq!(denied.status, ToolResultStatus::Denied);
    assert_eq!(success.call_id, "success");
    assert_eq!(failure.call_id, "failure");
    assert_eq!(cancelled.call_id, "cancelled");
    assert_eq!(denied.call_id, "cancelled");
}

#[test]
fn domain_read_result_contains_the_file_contents_for_the_next_model_turn() {
    let result = action_result(
        "read-call",
        AIAgentActionResultType::ReadFiles(ReadFilesResult::Success {
            files: vec![FileContext::new(
                "/workspace/src/lib.rs".to_string(),
                AnyFileContent::StringContent("pub fn answer() -> u8 { 42 }".to_string()),
                None,
                None,
            )],
        }),
    );

    let result = domain_tool_result(&result, false);

    assert_eq!(result.status, ToolResultStatus::Success);
    assert_eq!(result.call_id, "read-call");
    assert!(result.content.contains("/workspace/src/lib.rs"));
    assert!(result.content.contains("pub fn answer() -> u8 { 42 }"));
}

#[test]
fn action_permission_kinds_match_the_safety_boundary() {
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::Grep {
            queries: vec!["needle".to_string()],
            path: ".".to_string(),
        }),
        PermissionKind::Read
    );
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::InitProject),
        PermissionKind::Write
    );
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::RequestCommandOutput {
            command: "cargo test".to_string(),
            is_read_only: Some(true),
            is_risky: Some(false),
            wait_until_completion: true,
            uses_pager: Some(false),
            rationale: None,
            citations: Vec::new(),
        }),
        PermissionKind::Execute
    );
    assert_eq!(
        permission_kind_for_action(&AIAgentActionType::CallMCPTool {
            server_id: None,
            name: "tool".to_string(),
            input: serde_json::json!({}),
        }),
        PermissionKind::ExternalTool
    );
}

#[test]
fn only_rejecting_a_blocked_action_is_a_permission_denial() {
    assert!(is_permission_denial(
        CancellationReason::ManuallyCancelled,
        Some(&AIActionStatus::Blocked),
    ));
    assert!(!is_permission_denial(
        CancellationReason::ManuallyCancelled,
        Some(&AIActionStatus::Queued),
    ));
    assert!(!is_permission_denial(
        CancellationReason::FollowUpSubmitted {
            is_for_same_conversation: true,
        },
        Some(&AIActionStatus::Blocked),
    ));
}
