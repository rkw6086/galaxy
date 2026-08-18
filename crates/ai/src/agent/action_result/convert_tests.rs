use super::*;

#[test]
fn ask_user_question_skipped_by_auto_approve_converts_to_skipped_answers() {
    let result = api::request::input::tool_call_result::Result::from(
        AskUserQuestionResult::SkippedByAutoApprove {
            question_ids: vec!["q1".to_string(), "q2".to_string()],
        },
    );

    let api::request::input::tool_call_result::Result::AskUserQuestion(result) = result else {
        panic!("expected ask_user_question result");
    };

    let Some(api::ask_user_question_result::Result::Success(success)) = result.result else {
        panic!("expected success result");
    };

    assert_eq!(success.answers.len(), 2);
    assert_eq!(success.answers[0].question_id, "q1");
    assert_eq!(success.answers[1].question_id, "q2");
    assert!(matches!(
        success.answers[0].answer,
        Some(AskUserQuestionAnswer::Skipped(()))
    ));
    assert!(matches!(
        success.answers[1].answer,
        Some(AskUserQuestionAnswer::Skipped(()))
    ));
}

#[test]
fn completed_run_agents_child_converts_to_legacy_launched_wire_outcome() {
    let outcome = RunAgentsAgentOutcome {
        name: "research".to_string(),
        kind: RunAgentsAgentOutcomeKind::Completed {
            agent_id: "child-1".to_string(),
            output: "local output".to_string(),
        },
    };

    let converted = api::run_agents_result::AgentOutcome::from(outcome);
    assert!(matches!(
        converted.result,
        Some(api::run_agents_result::agent_outcome::Result::Launched(
            api::run_agents_result::LaunchedAgent { agent_id }
        )) if agent_id == "child-1"
    ));
}
