use std::collections::HashMap;
use std::sync::Arc;

use galaxy_agent_core::{ContentPart, MessageContent};
use regex::Regex;
use serial_test::serial;

use super::{prompt_content, GalaxyTerminalTools};
use crate::ai::agent::api::RequestParams;
use crate::ai::agent::{
    AIAgentAttachment, AIAgentContext, AIAgentInput, ImageContext, RunningCommand, UserQueryMode,
};
use crate::terminal::model::block::BlockId;
use crate::terminal::model::secrets;

struct SecretRegexReset;

impl Drop for SecretRegexReset {
    fn drop(&mut self) {
        secrets::set_user_and_enterprise_secret_regexes(
            std::iter::empty::<&Regex>(),
            std::iter::empty::<&Regex>(),
        );
    }
}

fn install_test_secret_regex(pattern: &str) -> SecretRegexReset {
    let regex = Regex::new(pattern).expect("valid test secret regex");
    secrets::set_user_and_enterprise_secret_regexes([&regex], std::iter::empty::<&Regex>());
    SecretRegexReset
}

fn user_query(query: &str, context: Vec<AIAgentContext>) -> AIAgentInput {
    AIAgentInput::UserQuery {
        query: query.to_owned(),
        context: Arc::from(context),
        static_query_type: None,
        referenced_attachments: HashMap::new(),
        user_query_mode: UserQueryMode::Normal,
        running_command: None,
        intended_agent: None,
    }
}

fn prompt_text(prompt: &MessageContent) -> &str {
    match prompt {
        MessageContent::Text(text) => text,
        MessageContent::MultiPart(parts) => match &parts[0] {
            ContentPart::Text(text) => text,
            _ => panic!("expected prompt text first"),
        },
        MessageContent::ToolUse { .. } | MessageContent::ToolResult { .. } => {
            panic!("expected user prompt")
        }
    }
}

#[test]
fn keeps_images_as_native_acp_content() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query(
        "What is in this image?",
        vec![AIAgentContext::Image(ImageContext {
            data: "aW1hZ2U=".to_owned(),
            mime_type: "image/png".to_owned(),
            file_name: "screen.png".to_owned(),
            is_figma: false,
        })],
    )];

    let prompt = prompt_content(&params, GalaxyTerminalTools::default()).expect("prompt");
    let MessageContent::MultiPart(parts) = &prompt else {
        panic!("expected multipart prompt");
    };
    assert_eq!(parts.len(), 2);
    assert!(matches!(
        &parts[0],
        ContentPart::Text(text) if text == "What is in this image?"
    ));
    assert!(matches!(
        &parts[1],
        ContentPart::Image { data, mime_type }
            if data == b"image"
                && mime_type == "image/png"
    ));
}

#[test]
fn sends_rules_and_selected_text_without_changing_visible_query() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![user_query(
        "Fix this",
        vec![AIAgentContext::SelectedText("broken()".to_owned())],
    )];
    params.global_rules = vec![("Safety".to_owned(), "Run tests first.".to_owned())];

    let prompt = prompt_content(&params, GalaxyTerminalTools::default()).expect("prompt");
    let text = prompt_text(&prompt);
    assert!(text.starts_with("Fix this"));
    assert!(text.contains("hidden_from_transcript"));
    assert!(text.contains("broken()"));
    assert!(text.contains("Run tests first."));
}

#[test]
fn hidden_system_requests_still_reach_the_agent_without_a_user_bubble() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![AIAgentInput::AutoCodeDiffQuery {
        query: "Repair the failing unit test.".to_owned(),
        context: Arc::from([]),
    }];

    let prompt = prompt_content(&params, GalaxyTerminalTools::default()).expect("prompt");
    let text = prompt_text(&prompt);
    assert!(text.starts_with("Handle the Galaxy system request"));
    assert!(text.contains("Repair the failing unit test."));
    assert!(text.contains("hidden_from_transcript"));
}

#[test]
fn completed_command_assessment_uses_hidden_context_without_monitor_guidance() {
    let block_id = BlockId::from("completed-session-42".to_owned());
    let mut params = RequestParams::new_for_test();
    params.input = vec![AIAgentInput::CommandCompletionAssessment {
        prompt: "Report whether the command succeeded.".to_owned(),
        context: Arc::from([AIAgentContext::SelectedText("root context".to_owned())]),
        completed_command: RunningCommand {
            command: "script/run-soak-test".to_owned(),
            block_id: block_id.clone(),
            grid_contents: "completed successfully".to_owned(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        },
    }];

    let prompt = prompt_content(
        &params,
        GalaxyTerminalTools {
            status: true,
            interrupt: true,
        },
    )
    .expect("prompt");
    let text = prompt_text(&prompt);

    assert!(text.starts_with("Handle the Galaxy system request"));
    assert!(text.contains("hidden_from_transcript"));
    assert!(text.contains("A monitored command has completed."));
    assert!(text.contains("script/run-soak-test"));
    assert!(text.contains(block_id.as_str()));
    assert!(text.contains("Final output:\ncompleted successfully"));
    assert!(text.contains("Report whether the command succeeded."));
    assert!(text.contains("Selected text:\nroot context"));
    assert!(!text.contains("galaxy_terminal_status"));
    assert!(!text.contains("galaxy_terminal_interrupt"));
    assert!(!text.contains("running_for_ms"));
}

#[test]
fn running_command_identity_and_output_are_sent_as_hidden_context() {
    let block_id = BlockId::from("session-42".to_owned());
    let mut params = RequestParams::new_for_test();
    params.input = vec![AIAgentInput::UserQuery {
        query: "Stop this after 75 seconds.".to_owned(),
        context: Arc::from([]),
        static_query_type: None,
        referenced_attachments: HashMap::new(),
        user_query_mode: UserQueryMode::Normal,
        running_command: Some(RunningCommand {
            command: "script/run-soak-test".to_owned(),
            block_id: block_id.clone(),
            grid_contents: "elapsed: 41s".to_owned(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        }),
        intended_agent: None,
    }];

    let prompt = prompt_content(
        &params,
        GalaxyTerminalTools {
            status: true,
            interrupt: true,
        },
    )
    .expect("prompt");
    let text = prompt_text(&prompt);
    assert!(text.starts_with("Stop this after 75 seconds."));
    assert!(text.contains(block_id.as_str()));
    assert!(text.contains("elapsed: 41s"));
    assert!(text.contains("galaxy_terminal_status"));
    assert!(text.contains("running_for_ms"));
    assert!(text.contains("galaxy_terminal_interrupt_at"));
    assert!(text.contains("outside the model loop"));
}

#[test]
fn running_command_prompt_does_not_advertise_unavailable_mutations() {
    let block_id = BlockId::from("session-42".to_owned());
    let mut params = RequestParams::new_for_test();
    params.input = vec![AIAgentInput::UserQuery {
        query: "Stop this after 75 seconds.".to_owned(),
        context: Arc::from([]),
        static_query_type: None,
        referenced_attachments: HashMap::new(),
        user_query_mode: UserQueryMode::Normal,
        running_command: Some(RunningCommand {
            command: "script/run-soak-test".to_owned(),
            block_id,
            grid_contents: String::new(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        }),
        intended_agent: None,
    }];

    let prompt = prompt_content(
        &params,
        GalaxyTerminalTools {
            status: true,
            interrupt: false,
        },
    )
    .expect("prompt");
    let text = prompt_text(&prompt);
    assert!(text.contains("galaxy_terminal_status"));
    assert!(text.contains("no Galaxy terminal mutation tool"));
    assert!(!text.contains("galaxy_terminal_interrupt_at"));
}

#[test]
#[serial]
fn redacts_request_text_before_creating_acp_content_blocks() {
    const SECRET: &str = "GALAXY_TEST_SECRET";
    let _secret_regex_reset = install_test_secret_regex(SECRET);
    let block_id = BlockId::from("session-redaction".to_owned());
    let mut referenced_attachments = HashMap::new();
    referenced_attachments.insert(
        "notes.txt".to_owned(),
        AIAgentAttachment::PlainText(format!("attachment {SECRET}")),
    );
    let mut params = RequestParams::new_for_test();
    params.enable_secret_redaction_for_test();
    params.global_rules = vec![("Private rule".to_owned(), format!("Never print {SECRET}."))];
    params.input = vec![AIAgentInput::UserQuery {
        query: format!("inspect {SECRET}"),
        context: Arc::from([
            AIAgentContext::SelectedText(format!("selected {SECRET}")),
            AIAgentContext::Image(ImageContext {
                data: "aW1hZ2U=".to_owned(),
                mime_type: "image/png".to_owned(),
                file_name: format!("{SECRET}.png"),
                is_figma: false,
            }),
        ]),
        static_query_type: None,
        referenced_attachments,
        user_query_mode: UserQueryMode::Normal,
        running_command: Some(RunningCommand {
            command: format!("echo {SECRET}"),
            block_id,
            grid_contents: format!("output {SECRET}"),
            cursor: format!("cursor {SECRET}"),
            requested_command_id: None,
            is_alt_screen_active: false,
        }),
        intended_agent: None,
    }];

    let prompt = prompt_content(&params, GalaxyTerminalTools::default()).expect("prompt");
    let MessageContent::MultiPart(parts) = &prompt else {
        panic!("expected multipart prompt");
    };
    let text = prompt_text(&prompt);
    assert!(!text.contains(SECRET));
    assert!(text.contains("******************"));
    assert!(text.contains("Selected text:"));
    assert!(text.contains("Current output:"));
    assert!(text.contains("Attachment notes.txt:"));
    assert!(text.contains("Galaxy rules:"));
    assert!(matches!(
        &parts[1],
        ContentPart::Image { data, .. } if data == b"image"
    ));

    // Prompt redaction must not mutate the local transcript copy.
    assert_eq!(
        params.input[0].display_query().as_deref(),
        Some("inspect GALAXY_TEST_SECRET")
    );
}
