use ai::agent::action::RunAgentsAgentRunConfig;

use super::*;

fn request(task: &str) -> RunAgentsRequest {
    RunAgentsRequest {
        summary: String::new(),
        base_prompt: String::new(),
        skills: vec![],
        model_id: String::new(),
        harness_type: String::new(),
        execution_mode: RunAgentsExecutionMode::Local,
        agent_run_configs: vec![RunAgentsAgentRunConfig {
            name: "worker".into(),
            prompt: task.into(),
            title: String::new(),
            model_id: String::new(),
        }],
        plan_id: String::new(),
        harness_auth_secret_name: None,
    }
}

fn discovery(id: &str, path: &str, content: &str, is_error: bool) -> Vec<ConversationMessage> {
    vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: id.into(),
                name: "read_files".into(),
                input: serde_json::json!({"path":path}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: id.into(),
                content: content.into(),
                is_error,
            },
        },
    ]
}

#[test]
fn includes_relevant_archived_discoveries_and_excludes_unrelated_results() {
    let archive = discovery(
        "old",
        "src/channel.ts",
        "channel reset belongs in publish",
        false,
    );
    let live = discovery("new", "billing.ts", "invoice currency", false);
    let mut req = request("Fix channel reset");
    add_parent_context(&mut req, &live, &archive, None);
    let prompt = &req.agent_run_configs[0].prompt;
    assert!(prompt.contains("src/channel.ts"));
    assert!(prompt.contains("belongs in publish"));
    assert!(!prompt.contains("billing.ts"));
}

#[test]
fn excludes_failed_results_reasoning_and_user_instructions() {
    let mut history = discovery("bad", "channel.ts", "channel failed to read", true);
    history.push(ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::MultiPart(vec![ContentPart::Reasoning {
            text: "channel private reasoning".into(),
            signature: None,
        }]),
    });
    history.push(ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("channel unrelated instruction".into()),
    });
    let mut req = request("Fix channel");
    add_parent_context(&mut req, &history, &[], None);
    assert_eq!(req.agent_run_configs[0].prompt, "Fix channel");
}

#[test]
fn bounds_unicode_handoffs_and_deduplicates_evidence() {
    let mut history = Vec::new();
    for index in 0..20 {
        history.extend(discovery(
            &index.to_string(),
            &format!("channel{index}.ts"),
            &"channel 🦀".repeat(2_000),
            false,
        ));
    }
    let mut req = request("Fix channel");
    add_parent_context(&mut req, &history, &history, Some("channel prior finding"));
    let packet = req.agent_run_configs[0]
        .prompt
        .strip_prefix("Fix channel")
        .unwrap();
    assert!(packet.len() <= MAX_PACKET_BYTES);
    assert!(packet.matches("\n- ").count() <= MAX_ITEMS);
    assert!(packet.contains("read_files"));
}

#[test]
fn does_not_attach_local_transcript_to_remote_workers() {
    let mut req = request("Fix channel");
    req.execution_mode = RunAgentsExecutionMode::Remote {
        environment_id: "remote".into(),
        worker_host: "host".into(),
        computer_use_enabled: false,
    };
    add_parent_context(
        &mut req,
        &discovery("one", "channel.ts", "channel finding", false),
        &[],
        None,
    );
    assert_eq!(req.agent_run_configs[0].prompt, "Fix channel");
}
