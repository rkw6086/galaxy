use std::collections::HashMap;
use std::sync::Arc;

use galaxy_acp::ContentBlock;
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
    assert_eq!(prompt.len(), 2);
    assert!(matches!(
        &prompt[0],
        ContentBlock::Text(text) if text.text == "What is in this image?"
    ));
    assert!(matches!(
        &prompt[1],
        ContentBlock::Image(image)
            if image.data == "aW1hZ2U="
                && image.mime_type == "image/png"
                && image.uri.as_deref() == Some("attachment://screen.png")
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
    let ContentBlock::Text(text) = &prompt[0] else {
        panic!("expected text");
    };
    assert!(text.text.starts_with("Fix this"));
    assert!(text.text.contains("hidden_from_transcript"));
    assert!(text.text.contains("broken()"));
    assert!(text.text.contains("Run tests first."));
}

#[test]
fn hidden_system_requests_still_reach_the_agent_without_a_user_bubble() {
    let mut params = RequestParams::new_for_test();
    params.input = vec![AIAgentInput::AutoCodeDiffQuery {
        query: "Repair the failing unit test.".to_owned(),
        context: Arc::from([]),
    }];

    let prompt = prompt_content(&params, GalaxyTerminalTools::default()).expect("prompt");
    let ContentBlock::Text(text) = &prompt[0] else {
        panic!("expected text");
    };
    assert!(text.text.starts_with("Handle the Galaxy system request"));
    assert!(text.text.contains("Repair the failing unit test."));
    assert!(text.text.contains("hidden_from_transcript"));
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
    let ContentBlock::Text(text) = &prompt[0] else {
        panic!("expected text");
    };
    assert!(text.text.starts_with("Stop this after 75 seconds."));
    assert!(text.text.contains(block_id.as_str()));
    assert!(text.text.contains("elapsed: 41s"));
    assert!(text.text.contains("galaxy_terminal_status"));
    assert!(text.text.contains("running_for_ms"));
    assert!(text.text.contains("galaxy_terminal_interrupt_at"));
    assert!(text.text.contains("outside the model loop"));
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
    let ContentBlock::Text(text) = &prompt[0] else {
        panic!("expected text");
    };
    assert!(text.text.contains("galaxy_terminal_status"));
    assert!(text.text.contains("no Galaxy terminal mutation tool"));
    assert!(!text.text.contains("galaxy_terminal_interrupt_at"));
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
    let ContentBlock::Text(text) = &prompt[0] else {
        panic!("expected text");
    };
    assert!(!text.text.contains(SECRET));
    assert!(text.text.contains("******************"));
    assert!(text.text.contains("Selected text:"));
    assert!(text.text.contains("Current output:"));
    assert!(text.text.contains("Attachment notes.txt:"));
    assert!(text.text.contains("Galaxy rules:"));
    assert!(matches!(
        &prompt[1],
        ContentBlock::Image(image)
            if image.data == "aW1hZ2U="
                && !image.uri.as_deref().unwrap_or_default().contains(SECRET)
    ));

    // Prompt redaction must not mutate the local transcript copy.
    assert_eq!(
        params.input[0].display_query().as_deref(),
        Some("inspect GALAXY_TEST_SECRET")
    );
}
