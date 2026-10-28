use aws_sdk_bedrockruntime::config::Region;
use aws_smithy_http_client::test_util::NeverClient;
use futures::StreamExt;
use galaxy_agent_core::{
    AgentEvent, AgentRuntime, ContentPart, ConversationMessage, MessageContent, MessageRole,
    StopReason, ToolDefinition, TurnCommand, TurnRequest, Usage,
};
use rig_bedrock::streaming::{BedrockStreamingResponse, BedrockUsage};
use rig_core::completion::{AssistantContent, CompletionError, Message};
use rig_core::message::{DocumentSourceKind, ToolResultContent, UserContent};

use super::*;
use crate::stream::{completion_error_stop_reason, map_usage};

#[test]
fn resolves_context_marker_and_us_inference_profile() {
    assert_eq!(
        resolve_bedrock_model_id("anthropic.claude-sonnet-4-6[1m]", "us-east-1", true,).unwrap(),
        "us.anthropic.claude-sonnet-4-6"
    );
}

#[test]
fn resolves_each_supported_inference_geography() {
    for (region, expected_prefix) in [
        ("eu-west-1", "eu"),
        ("il-central-1", "eu"),
        ("ap-northeast-1", "jp"),
        ("ap-southeast-2", "au"),
        ("ap-southeast-1", "apac"),
        ("ca-central-1", "us"),
    ] {
        assert_eq!(
            resolve_bedrock_model_id("anthropic.claude-test", region, true).unwrap(),
            format!("{expected_prefix}.anthropic.claude-test")
        );
    }
}

#[test]
fn preserves_arns_existing_profiles_and_unknown_regions() {
    let arn = "arn:aws:bedrock:us-east-1:123:application-inference-profile/example";
    assert_eq!(
        resolve_bedrock_model_id(arn, "us-east-1", true).unwrap(),
        arn
    );
    assert_eq!(
        resolve_bedrock_model_id("global.anthropic.claude-test", "us-east-1", true).unwrap(),
        "global.anthropic.claude-test"
    );
    assert_eq!(
        resolve_bedrock_model_id("anthropic.claude-test", "me-south-1", true).unwrap(),
        "anthropic.claude-test"
    );
}

#[test]
fn prefixes_amazon_models_instead_of_mistaking_provider_for_geography() {
    assert_eq!(
        resolve_bedrock_model_id("amazon.nova-pro-v1:0", "us-east-1", true).unwrap(),
        "us.amazon.nova-pro-v1:0"
    );
}

#[test]
fn rejects_an_empty_model_id() {
    let error = resolve_bedrock_model_id("  ", "us-east-1", false).unwrap_err();
    assert_eq!(error.kind, AgentErrorKind::Configuration);
}

#[test]
fn normalizes_bedrock_usage_and_max_token_stop() {
    let response = BedrockStreamingResponse {
        usage: Some(BedrockUsage {
            input_tokens: 100,
            output_tokens: 25,
            total_tokens: 125,
            cache_read_input_tokens: Some(40),
            cache_write_input_tokens: Some(10),
        }),
        stop_reason: None,
    };
    assert_eq!(
        map_usage((&response).into()),
        Usage {
            input_tokens: 50,
            output_tokens: 25,
            cached_input_tokens: 40,
            cache_creation_input_tokens: 10,
        }
    );
    assert_eq!(
        completion_error_stop_reason(&CompletionError::ProviderError(
            "Exceeded max tokens".to_string(),
        )),
        Some(StopReason::MaxTokens)
    );
}

#[test]
fn constructs_rig_client_from_galaxys_resolved_aws_client_without_network() {
    let sdk_config = aws_sdk_bedrockruntime::Config::builder()
        .behavior_version_latest()
        .region(Region::new("us-east-1"))
        .http_client(NeverClient::new())
        .build();
    let aws_client = AwsBedrockClient::from_conf(sdk_config);
    let client = BedrockRuntime::from_aws_client(
        aws_client,
        BedrockRigConfig {
            model: "anthropic.claude-test[1M]".to_string(),
            region: "us-east-1".to_string(),
            cross_region_inference: true,
            prompt_caching: true,
            max_output_tokens: Some(8_192),
        },
    )
    .unwrap();

    assert_eq!(client.resolved_model(), "us.anthropic.claude-test");
    let completion_model = client.completion_model();
    assert_eq!(completion_model.model, client.resolved_model());
    assert!(completion_model.prompt_caching);
}

#[tokio::test]
async fn cancellation_before_bedrock_stream_start_never_contacts_aws() {
    let never_client = NeverClient::new();
    let sdk_config = aws_sdk_bedrockruntime::Config::builder()
        .behavior_version_latest()
        .region(Region::new("us-east-1"))
        .http_client(never_client.clone())
        .build();
    let runtime = BedrockRuntime::from_aws_client(
        AwsBedrockClient::from_conf(sdk_config),
        BedrockRigConfig {
            model: "anthropic.claude-test".to_string(),
            region: "us-east-1".to_string(),
            cross_region_inference: false,
            prompt_caching: false,
            max_output_tokens: None,
        },
    )
    .unwrap();
    let (sender, control) = galaxy_agent_core::turn_control();
    sender.send(TurnCommand::Cancel).await.unwrap();

    let events = runtime
        .start_turn(
            TurnRequest::new(
                "anthropic.claude-test",
                vec![ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::Text("Hello".to_string()),
                }],
            ),
            control,
        )
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(matches!(events[0], AgentEvent::TurnStarted { .. }));
    assert_eq!(
        events[1],
        AgentEvent::TurnStopped {
            reason: StopReason::Cancelled,
        }
    );
    assert_eq!(never_client.num_calls(), 0);
}

#[test]
fn bedrock_request_preserves_system_image_reasoning_tool_and_token_semantics() {
    let mut request = TurnRequest::new(
        "anthropic.claude-sonnet-4-6",
        vec![
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::MultiPart(vec![
                    ContentPart::Text("Describe the image".to_string()),
                    ContentPart::Image {
                        data: vec![1, 2, 3, 4],
                        mime_type: "image/png".to_string(),
                    },
                ]),
            },
            ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::MultiPart(vec![
                    ContentPart::Reasoning {
                        text: "I should inspect the manifest.".to_string(),
                        signature: Some("signed-reasoning".to_string()),
                    },
                    ContentPart::ToolUse {
                        tool_use_id: "call-1".to_string(),
                        name: "read_files".to_string(),
                        input: serde_json::json!({"files": ["Cargo.toml"]}),
                    },
                ]),
            },
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::ToolResult {
                    tool_use_id: "call-1".to_string(),
                    content: "permission denied".to_string(),
                    is_error: true,
                },
            },
        ],
    );
    request.system_prompt = Some("Use Galaxy tools safely".to_string());
    request.max_output_tokens = Some(4_096);
    request.tools.push(ToolDefinition {
        name: "read_files".to_string(),
        description: "Read project files".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"files": {"type": "array"}}
        }),
    });

    let converted = build_bedrock_completion_request(request, Some(8_192)).unwrap();
    assert!(converted.additional_params.is_none());
    assert_eq!(converted.max_tokens, Some(4_096));
    assert_eq!(converted.tools.len(), 1);
    assert_eq!(converted.tools[0].name, "read_files");

    let messages = converted.chat_history.iter().collect::<Vec<_>>();
    let [system, user, assistant, result] = messages.as_slice() else {
        panic!("expected system, user, assistant, and tool-result messages");
    };
    assert!(matches!(
        system,
        Message::System { content } if content == "Use Galaxy tools safely"
    ));

    let Message::User { content } = user else {
        panic!("expected a user image message");
    };
    let user_content = content.iter().collect::<Vec<_>>();
    assert!(matches!(
        user_content.as_slice(),
        [UserContent::Text(text), UserContent::Image(image)]
            if text.text == "Describe the image"
                && matches!(&image.data, DocumentSourceKind::Base64(data) if data == "AQIDBA==")
    ));

    let Message::Assistant { content, .. } = assistant else {
        panic!("expected an assistant tool call");
    };
    let assistant_content = content.iter().collect::<Vec<_>>();
    let [
        AssistantContent::Reasoning(reasoning),
        AssistantContent::ToolCall(call),
    ] = assistant_content.as_slice()
    else {
        panic!("expected signed reasoning followed by a tool call");
    };
    assert_eq!(reasoning.display_text(), "I should inspect the manifest.");
    assert_eq!(reasoning.first_signature(), Some("signed-reasoning"));
    assert_eq!(call.id, "call-1");
    assert_eq!(call.function.name, "read_files");

    let Message::User { content } = result else {
        panic!("expected a user tool result");
    };
    let Some(UserContent::ToolResult(result)) = content.iter().next() else {
        panic!("expected tool result content");
    };
    assert_eq!(result.id, "call-1");
    assert!(matches!(
        result.content.iter().next(),
        Some(ToolResultContent::Text(text)) if text.text == "[ERROR] permission denied"
    ));
}

#[test]
fn bedrock_follow_ups_omit_unsigned_reasoning_without_losing_tool_calls_or_results() {
    for model in [
        "deepseek.r1-v1:0",
        "us.amazon.nova-2-lite-v1:0",
        "openai.gpt-oss-120b-1:0",
        "qwen.qwen3-32b-v1:0",
        "anthropic.claude-sonnet-4-6",
    ] {
        let request = TurnRequest::new(
            model,
            vec![
                ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::Text("Inspect the project".to_owned()),
                },
                ConversationMessage {
                    role: MessageRole::Assistant,
                    content: MessageContent::MultiPart(vec![
                        ContentPart::Reasoning {
                            text: "display-only reasoning".to_owned(),
                            signature: None,
                        },
                        ContentPart::Text("Reading the manifest".to_owned()),
                        ContentPart::ToolUse {
                            tool_use_id: "manifest-call".to_owned(),
                            name: "read_files".to_owned(),
                            input: serde_json::json!({"files": ["Cargo.toml"]}),
                        },
                    ]),
                },
                ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::ToolResult {
                        tool_use_id: "manifest-call".to_owned(),
                        content: "manifest contents".to_owned(),
                        is_error: false,
                    },
                },
            ],
        );
        let converted = build_bedrock_completion_request(request, None).unwrap();
        let messages = converted.chat_history.iter().collect::<Vec<_>>();
        let [
            _,
            Message::Assistant { content, .. },
            Message::User { content: result },
        ] = messages.as_slice()
        else {
            panic!("expected the tool exchange for {model}");
        };
        assert!(matches!(content.iter().collect::<Vec<_>>().as_slice(),
            [AssistantContent::Text(text), AssistantContent::ToolCall(call)]
                if text.text == "Reading the manifest" && call.id == "manifest-call"
        ));
        assert!(
            matches!(result.iter().next(), Some(UserContent::ToolResult(result))
            if result.id == "manifest-call")
        );
    }
}

#[test]
fn bedrock_model_switch_drops_foreign_signed_reasoning_and_empty_messages() {
    for model in [
        "amazon.nova-pro-v1:0",
        "us.deepseek.r1-v1:0",
        "openai.gpt-oss-120b-1:0",
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "anthropic.claude-3-haiku-20240307-v1:0",
        "anthropic.claude-instant-v1",
        "anthropic.claude-v1",
        "anthropic.claude-v2:1",
    ] {
        let messages = vec![
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text("Question".to_owned()),
            },
            ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::MultiPart(vec![ContentPart::Reasoning {
                    text: "Earlier thinking".to_owned(),
                    signature: Some("original-signature".to_owned()),
                }]),
            },
            ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::MultiPart(vec![
                    ContentPart::Reasoning {
                        text: "Earlier thinking".to_owned(),
                        signature: Some("original-signature".to_owned()),
                    },
                    ContentPart::Text("Visible answer".to_owned()),
                ]),
            },
        ];
        // The runtime's selected model takes precedence over a historical request model.
        let request = TurnRequest::new("anthropic.claude-sonnet-4-6", messages);
        let converted = build_bedrock_completion_request_for_model(request, None, model).unwrap();
        assert_eq!(converted.chat_history.len(), 2);
        assert!(
            matches!(converted.chat_history.iter().nth(1), Some(Message::Assistant { content, .. })
            if matches!(content.iter().collect::<Vec<_>>().as_slice(), [AssistantContent::Text(text)] if text.text == "Visible answer"))
        );
    }
}

#[test]
fn bedrock_preserves_signed_claude_blocks_exactly_across_identifier_forms() {
    let signed = ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::MultiPart(vec![
            ContentPart::Reasoning {
                text: "Unmodified thinking".to_owned(),
                signature: Some("signed-text".to_owned()),
            },
            ContentPart::Reasoning {
                text: String::new(),
                signature: Some("signature-only".to_owned()),
            },
            ContentPart::Text("Answer".to_owned()),
        ]),
    };
    for model in [
        "anthropic.claude-3-7-sonnet-20250219-v1:0",
        "global.anthropic.claude-sonnet-4-6[1m]",
        "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-east-1:123456789012:application-inference-profile/opaque-profile",
    ] {
        assert_eq!(
            prepare_bedrock_history(model, vec![signed.clone()]),
            vec![signed.clone()]
        );
    }
    for signature in [None, Some(String::new()), Some("  ".to_owned())] {
        let message = ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::MultiPart(vec![ContentPart::Reasoning {
                text: "Display only".to_owned(),
                signature,
            }]),
        };
        assert!(prepare_bedrock_history("anthropic.claude-sonnet-4-6", vec![message]).is_empty());
    }
}
