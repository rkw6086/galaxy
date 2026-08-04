use std::collections::HashMap;
use std::sync::Arc;

use galaxy_agent_core::{ContentPart, MessageContent, MessageRole, ToolResult, ToolResultStatus};
use warp_multi_agent_api::ToolType;

use super::{input_messages, prepare_rig_turn};
use crate::ai::agent::api::RequestParams;
use crate::ai::agent::{AIAgentContext, AIAgentInput, AnyFileContent, FileContext, UserQueryMode};
use crate::ai::llms::LLMId;
use crate::ai::openai::client::OpenAIClientConfig;

fn config() -> OpenAIClientConfig {
    OpenAIClientConfig {
        base_url: "http://localhost:4000/v1".to_string(),
        api_key: None,
        model: Some("provider-model".to_string()),
        max_input_tokens: Some(128_000),
        max_output_tokens: Some(8_192),
        use_rig: true,
        supports_system_messages: true,
    }
}

fn user_query(query: &str) -> AIAgentInput {
    user_query_with_context(query, Vec::new())
}

fn user_query_with_context(query: &str, context: Vec<AIAgentContext>) -> AIAgentInput {
    AIAgentInput::UserQuery {
        query: query.to_string(),
        context: Arc::from(context),
        static_query_type: None,
        referenced_attachments: HashMap::new(),
        user_query_mode: UserQueryMode::Normal,
        running_command: None,
        intended_agent: None,
    }
}

#[test]
fn native_context_reaches_rig_without_a_proto_context_conversion() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query_with_context(
        "Explain the selected implementation",
        vec![
            AIAgentContext::SelectedText("prepare_rig_turn(params)".to_string()),
            AIAgentContext::File(FileContext::new(
                "/repo/src/runtime.rs".to_string(),
                AnyFileContent::StringContent("fn prepare_rig_turn() {}".to_string()),
                None,
                None,
            )),
            AIAgentContext::Codebase {
                path: "/repo".to_string(),
                name: "galaxy".to_string(),
            },
        ],
    )];

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());
    let prompt = prepared.request.system_prompt.expect("system prompt");

    assert!(prompt.contains("prepare_rig_turn(params)"));
    assert!(prompt.contains("fn prepare_rig_turn() {}"));
    assert!(prompt.contains("Indexed codebase: galaxy (/repo)"));
}

#[test]
fn builds_a_rig_turn_directly_from_galaxy_request_state() {
    let mut params = RequestParams::new_for_test();
    params.model = LLMId::from("selected-model");
    params.root_task_id = Some("task-1".to_string());
    params.input = vec![user_query("Inspect this repository")];

    let prepared = prepare_rig_turn(
        &config(),
        params,
        vec![ToolType::ReadFiles, ToolType::RunShellCommand],
        Vec::new(),
    );

    assert_eq!(prepared.task_id, "task-1");
    assert_eq!(
        prepared.user_query.as_deref(),
        Some("Inspect this repository")
    );
    assert_eq!(prepared.request.model.as_str(), "provider-model");
    assert_eq!(prepared.request.max_output_tokens, Some(8_192));
    assert_eq!(prepared.request.messages, prepared.persistent_messages);
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "read_files"));
    assert!(prepared
        .request
        .tools
        .iter()
        .any(|tool| tool.name == "run_shell_command"));
    assert!(prepared
        .request
        .system_prompt
        .as_deref()
        .is_some_and(|prompt| prompt.contains("Galaxy owns tool permissions and execution")));
    assert!(matches!(
        &prepared.request.messages[0],
        galaxy_agent_core::ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(text),
        } if text == "Inspect this repository"
    ));
}

#[test]
fn normalized_tool_outcomes_are_the_only_action_results_sent_to_rig() {
    let statuses = [
        ("read", ToolResultStatus::Success, false),
        ("shell", ToolResultStatus::Error, true),
        ("denied", ToolResultStatus::Denied, true),
        ("cancelled", ToolResultStatus::Cancelled, false),
    ];
    let tool_results = statuses
        .iter()
        .map(|(call_id, status, _)| ToolResult {
            call_id: (*call_id).to_string(),
            content: format!("normalized-{call_id}"),
            status: *status,
        })
        .collect();

    let messages = input_messages(Vec::new(), tool_results);

    assert_eq!(messages.len(), 1);
    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected normalized tool results to remain in one user turn");
    };
    for ((call_id, _, expected_error), part) in statuses.iter().zip(parts) {
        assert!(matches!(
            part,
            ContentPart::ToolResult {
                tool_use_id,
                content,
                is_error,
            } if tool_use_id == call_id
                && content == &format!("normalized-{call_id}")
                && is_error == expected_error
        ));
    }
}

#[test]
fn progressive_summary_is_provider_context_not_persistent_history() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query("Continue")];
    params.progressive_summary = Some("Earlier work was validated.".to_string());

    let prepared = prepare_rig_turn(&config(), params, Vec::new(), Vec::new());

    assert_eq!(prepared.persistent_messages.len(), 1);
    assert_eq!(prepared.request.messages.len(), 3);
    assert!(matches!(
        &prepared.request.messages[0].content,
        MessageContent::Text(text) if text.contains("Earlier work was validated.")
    ));
    assert!(matches!(
        &prepared.request.messages[2].content,
        MessageContent::Text(text) if text == "Continue"
    ));
}
