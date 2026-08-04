use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use galaxyui_core::async_assert;
use warp::features::FeatureFlag;
use warp::integration_testing::agent_mode::{
    assert_latest_exchange_text, assert_task_is_cancelled, enter_agent_view,
    set_execution_profile_auto_apply_code_diffs, set_execution_profile_auto_execute,
    set_execution_profile_auto_execute_mcp_tools, set_execution_profile_no_auto_execute,
    set_preferred_agent_mode_llm, start_ephemeral_mcp_server_for_testing, submit_ai_query,
    submit_ai_query_and_wait_until_blocked, submit_ai_query_and_wait_until_done,
    wait_until_mcp_server_is_active_for_testing, ConversationTarget,
};
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::{
    assert_input_is_focused, wait_until_bootstrapped_single_pane_for_tab,
};

use super::new_builder;
use crate::rig_test_mcp_server::{SERVER_ARGUMENT as MCP_SERVER_ARGUMENT, TOOL_RESULT_PREFIX};
use crate::Builder;

const MODEL_ID: &str = "integration-rig-model";
const READ_CALL_ID: &str = "rig-read-call";
const READ_FINAL_TEXT: &str = "Rig read round trip completed.";
const FIXTURE_CONTENT: &str = "content returned through the Galaxy read executor";
const SHELL_CALL_ID: &str = "rig-shell-call";
const SHELL_SUCCESS_OUTPUT: &str = "rig-shell-success-output";
const SHELL_SUCCESS_FINAL_TEXT: &str = "Rig shell success round trip completed.";
const SHELL_FAILURE_OUTPUT: &str = "rig-shell-failure-output";
const SHELL_FAILURE_FINAL_TEXT: &str = "Rig shell failure round trip completed.";
const SHELL_DENIED_FINAL_TEXT: &str = "Rig shell denial was preserved.";
const EDIT_CALL_ID: &str = "rig-edit-call";
const EDIT_INITIAL_CONTENT: &str = "before Rig edit\n";
const EDIT_UPDATED_CONTENT: &str = "after Rig edit\n";
const EDIT_FINAL_TEXT: &str = "Rig edit round trip completed.";
const MCP_CALL_ID: &str = "rig-mcp-call";
const MCP_INSTALLATION_ID: &str = "11111111-1111-4111-8111-111111111111";
const MCP_TEMPLATE_ID: &str = "22222222-2222-4222-8222-222222222222";
const MCP_SERVER_NAME: &str = "rig-integration";
const MCP_TOOL_NAME: &str = "mcp__11111111-1111-4111-8111-111111111111__echo";
const MCP_INPUT: &str = "hello from Rig";
const MCP_FINAL_TEXT: &str = "Rig MCP round trip completed.";

#[derive(Clone)]
enum MockScenario {
    Read {
        fixture_path: Arc<Mutex<String>>,
    },
    ShellSuccess,
    ShellFailure,
    ShellDenied {
        marker_path: Arc<Mutex<String>>,
    },
    Edit {
        fixture_path: Arc<Mutex<String>>,
    },
    Cancellation {
        stream_started: Arc<AtomicBool>,
        stream_cancelled: Arc<AtomicBool>,
    },
    Mcp,
}

pub fn test_rig_read_tool_round_trip() -> Builder {
    let fixture_path = Arc::new(Mutex::new(String::new()));
    rig_builder(MockScenario::Read { fixture_path })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_preferred_agent_mode_llm(MODEL_ID))
        .with_step(enter_agent_view())
        .with_step(submit_ai_query_and_wait_until_done(
            "Read the integration fixture and report when the read is complete.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Assert Rig read result reached Agent Mode")
                .add_named_assertion(
                    "Final response follows the real read tool result",
                    assert_latest_exchange_text(|text| text.contains(READ_FINAL_TEXT)),
                ),
        )
}

pub fn test_rig_shell_tool_success_round_trip() -> Builder {
    rig_builder(MockScenario::ShellSuccess)
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_preferred_agent_mode_llm(MODEL_ID))
        .with_step(set_execution_profile_auto_execute())
        .with_step(enter_agent_view())
        .with_step(submit_ai_query_and_wait_until_done(
            "Run the requested shell success check.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Assert Rig shell success reached Agent Mode")
                .add_named_assertion(
                    "Final response follows the successful shell result",
                    assert_latest_exchange_text(|text| text.contains(SHELL_SUCCESS_FINAL_TEXT)),
                ),
        )
}

pub fn test_rig_shell_tool_failure_round_trip() -> Builder {
    rig_builder(MockScenario::ShellFailure)
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_preferred_agent_mode_llm(MODEL_ID))
        .with_step(set_execution_profile_auto_execute())
        .with_step(enter_agent_view())
        .with_step(submit_ai_query_and_wait_until_done(
            "Run the requested failing shell check.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Assert Rig shell failure reached Agent Mode")
                .add_named_assertion(
                    "Final response follows the failed shell result",
                    assert_latest_exchange_text(|text| text.contains(SHELL_FAILURE_FINAL_TEXT)),
                ),
        )
}

pub fn test_rig_shell_tool_permission_denial() -> Builder {
    let marker_path = Arc::new(Mutex::new(String::new()));
    rig_builder(MockScenario::ShellDenied { marker_path })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_preferred_agent_mode_llm(MODEL_ID))
        .with_step(set_execution_profile_no_auto_execute())
        .with_step(enter_agent_view())
        .with_step(submit_ai_query_and_wait_until_blocked(
            "Propose the requested shell command and wait for my decision.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Deny the blocked Rig shell command")
                .with_keystrokes(&["ctrl-c"])
                .add_named_assertion(
                    "Input returns after denying the command",
                    assert_input_is_focused(),
                ),
        )
        .with_step(submit_ai_query_and_wait_until_done(
            "Continue without running the denied command.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Assert Rig denial reached Agent Mode")
                .add_named_assertion(
                    "Final response follows the explicit denied result",
                    assert_latest_exchange_text(|text| text.contains(SHELL_DENIED_FINAL_TEXT)),
                ),
        )
}

pub fn test_rig_edit_tool_round_trip() -> Builder {
    let fixture_path = Arc::new(Mutex::new(String::new()));
    rig_builder(MockScenario::Edit { fixture_path })
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(set_preferred_agent_mode_llm(MODEL_ID))
        .with_step(set_execution_profile_auto_apply_code_diffs())
        .with_step(enter_agent_view())
        .with_step(submit_ai_query_and_wait_until_done(
            "Apply the requested edit to the integration fixture.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Assert Rig edit result reached Agent Mode")
                .add_named_assertion(
                    "Final response follows the real edit result",
                    assert_latest_exchange_text(|text| text.contains(EDIT_FINAL_TEXT)),
                ),
        )
}

pub fn test_rig_in_flight_cancellation() -> Builder {
    let stream_started = Arc::new(AtomicBool::new(false));
    let stream_cancelled = Arc::new(AtomicBool::new(false));
    rig_builder(MockScenario::Cancellation {
        stream_started: stream_started.clone(),
        stream_cancelled: stream_cancelled.clone(),
    })
    .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
    .with_step(set_preferred_agent_mode_llm(MODEL_ID))
    .with_step(enter_agent_view())
    .with_step(submit_ai_query(
        "Start the cancellable Rig response.",
        Duration::from_secs(60),
    ))
    .with_step(
        new_step_with_default_assertions("Wait for the Rig provider stream to start")
            .set_timeout(Duration::from_secs(30))
            .add_named_assertion("Provider stream started", move |_app, _window_id| {
                async_assert!(
                    stream_started.load(Ordering::SeqCst),
                    "Waiting for the provider stream to start"
                )
            }),
    )
    .with_step(
        new_step_with_default_assertions("Cancel the in-flight Rig provider stream")
            .set_timeout(Duration::from_secs(30))
            .with_keystrokes(&["ctrl-c"])
            .add_named_assertion(
                "Conversation is marked cancelled",
                assert_task_is_cancelled(ConversationTarget::Active),
            )
            .add_named_assertion(
                "Provider connection was dropped",
                move |_app, _window_id| {
                    async_assert!(
                        stream_cancelled.load(Ordering::SeqCst),
                        "Waiting for cancellation to close the provider connection"
                    )
                },
            )
            .add_named_assertion(
                "Input returns after cancelling the stream",
                assert_input_is_focused(),
            ),
    )
}

pub fn test_rig_mcp_tool_round_trip() -> Builder {
    let command = std::env::current_exe()
        .expect("integration executable path")
        .to_string_lossy()
        .into_owned();
    rig_builder(MockScenario::Mcp)
        .with_step(wait_until_bootstrapped_single_pane_for_tab(0))
        .with_step(start_ephemeral_mcp_server_for_testing(
            command,
            MCP_SERVER_ARGUMENT.to_string(),
            MCP_INSTALLATION_ID,
            MCP_TEMPLATE_ID,
            MCP_SERVER_NAME,
        ))
        .with_step(wait_until_mcp_server_is_active_for_testing(
            MCP_INSTALLATION_ID,
            Duration::from_secs(30),
        ))
        .with_step(set_preferred_agent_mode_llm(MODEL_ID))
        .with_step(set_execution_profile_auto_execute_mcp_tools())
        .with_step(enter_agent_view())
        .with_step(submit_ai_query_and_wait_until_done(
            "Call the hermetic MCP echo tool.",
            Duration::from_secs(60),
        ))
        .with_step(
            new_step_with_default_assertions("Assert Rig MCP result reached Agent Mode")
                .add_named_assertion(
                    "Final response follows the real MCP result",
                    assert_latest_exchange_text(|text| text.contains(MCP_FINAL_TEXT)),
                ),
        )
}

fn rig_builder(scenario: MockScenario) -> Builder {
    FeatureFlag::AgentView.set_enabled(true);
    FeatureFlag::MCPGroupedServerContext.set_enabled(true);
    let stop = Arc::new(AtomicBool::new(false));
    let setup_scenario = scenario.clone();
    let (address, server_thread) = start_mock_provider(scenario, stop.clone());
    let server_thread = Arc::new(Mutex::new(Some(server_thread)));
    let cleanup_stop = stop.clone();
    let cleanup_thread = server_thread.clone();

    new_builder()
        .with_setup(move |utils| {
            write_provider_settings(address);
            match &setup_scenario {
                MockScenario::Read { fixture_path } => {
                    let fixture = utils.test_dir().join("rig-read-fixture.txt");
                    std::fs::write(&fixture, FIXTURE_CONTENT)
                        .expect("should write Rig integration fixture");
                    *fixture_path.lock().expect("fixture path lock") =
                        fixture.to_string_lossy().into_owned();
                }
                MockScenario::ShellDenied { marker_path } => {
                    let marker = utils.test_dir().join("rig-denied-command-marker.txt");
                    *marker_path.lock().expect("marker path lock") =
                        marker.to_string_lossy().into_owned();
                }
                MockScenario::Edit { fixture_path } => {
                    let fixture = utils.test_dir().join("rig-edit-fixture.txt");
                    std::fs::write(&fixture, EDIT_INITIAL_CONTENT)
                        .expect("should write Rig edit integration fixture");
                    *fixture_path.lock().expect("fixture path lock") =
                        fixture.to_string_lossy().into_owned();
                }
                MockScenario::ShellSuccess
                | MockScenario::ShellFailure
                | MockScenario::Cancellation { .. }
                | MockScenario::Mcp => {}
            }
        })
        .with_cleanup(move |_utils| {
            cleanup_stop.store(true, Ordering::SeqCst);
            if let Some(handle) = cleanup_thread.lock().expect("server thread lock").take() {
                handle.join().expect("mock provider should stop cleanly");
            }
        })
}

fn write_provider_settings(address: SocketAddr) {
    let settings_path = warp::settings::user_preferences_toml_file_path();
    std::fs::create_dir_all(settings_path.parent().expect("settings parent"))
        .expect("should create settings directory");
    let settings = format!(
        r#"[ai.openai]
enabled = true

[[ai.providers]]
name = "Rig Integration"
base_url = "http://{address}/v1"

[[ai.providers.models]]
model_id = "{MODEL_ID}"
display_name = "Rig Integration Model"
context_size = 128000
use_rig = true
supports_system_messages = false
"#
    );
    std::fs::write(settings_path, settings).expect("should write provider settings");
}

fn start_mock_provider(
    scenario: MockScenario,
    stop: Arc<AtomicBool>,
) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("should bind mock Rig provider");
    let address = listener.local_addr().expect("mock provider address");
    listener
        .set_nonblocking(true)
        .expect("should make mock provider nonblocking");
    let request_count = AtomicUsize::new(0);
    let thread = thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    serve_request(&mut stream, &scenario, &request_count, &stop);
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("mock Rig provider accept failed: {error}"),
            }
        }
    });
    (address, thread)
}

fn serve_request(
    stream: &mut TcpStream,
    scenario: &MockScenario,
    request_count: &AtomicUsize,
    stop: &AtomicBool,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("should set request timeout");
    let request = read_request(stream);
    let request_line = request.lines().next().unwrap_or_default();

    if request_line.contains("/models") {
        let body =
            format!(r#"{{"object":"list","data":[{{"id":"{MODEL_ID}","object":"model"}}]}}"#);
        write_response(stream, "application/json", &body);
        return;
    }

    assert!(
        request_line.contains("/chat/completions"),
        "unexpected mock provider request: {request_line}"
    );
    let turn = request_count.fetch_add(1, Ordering::SeqCst);
    if let MockScenario::Cancellation {
        stream_started,
        stream_cancelled,
    } = scenario
    {
        assert_eq!(turn, 0, "unexpected extra cancellation chat request");
        write_cancellable_response(stream, stream_started, stream_cancelled, stop);
        return;
    }
    let body = match turn {
        0 => tool_call_sse(scenario),
        1 => {
            assert_follow_up_request(scenario, &request);
            final_text_sse(final_text(scenario))
        }
        _ => panic!("unexpected extra chat completion request"),
    };
    write_response(stream, "text/event-stream", &body);
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut chunk = [0; 8 * 1024];
    loop {
        let bytes_read = stream
            .read(&mut chunk)
            .expect("should read provider request");
        if bytes_read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..bytes_read]);
        assert!(
            request.len() <= 1024 * 1024,
            "mock provider request exceeded 1 MiB"
        );

        let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let body_start = headers_end + 4;
        let headers = String::from_utf8_lossy(&request[..headers_end]);
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        });
        match content_length {
            Some(content_length) if request.len() < body_start + content_length => continue,
            Some(_) | None => break,
        }
    }
    String::from_utf8(request).expect("provider request should be valid UTF-8")
}

fn tool_call_sse(scenario: &MockScenario) -> String {
    let (call_id, tool_name, arguments) = match scenario {
        MockScenario::Read { fixture_path } => {
            let fixture_path = fixture_path.lock().expect("fixture path lock").clone();
            (
                READ_CALL_ID,
                "read_files",
                serde_json::json!({"files": [fixture_path]}),
            )
        }
        MockScenario::ShellSuccess => (
            SHELL_CALL_ID,
            "run_shell_command",
            shell_arguments("printf '%s\\n' 'rig-shell-success-output'"),
        ),
        MockScenario::ShellFailure => (
            SHELL_CALL_ID,
            "run_shell_command",
            shell_arguments("(printf '%s\\n' 'rig-shell-failure-output' >&2; exit 7)"),
        ),
        MockScenario::ShellDenied { marker_path } => {
            let marker_path = marker_path.lock().expect("marker path lock").clone();
            (
                SHELL_CALL_ID,
                "run_shell_command",
                shell_arguments(&format!(
                    "printf '%s\\n' 'command-must-not-run' > '{marker_path}'"
                )),
            )
        }
        MockScenario::Edit { fixture_path } => {
            let fixture_path = fixture_path.lock().expect("fixture path lock").clone();
            (
                EDIT_CALL_ID,
                "apply_file_diffs",
                serde_json::json!({
                    "summary": "Update the Rig edit integration fixture",
                    "diffs": [{
                        "file_path": fixture_path,
                        "search": EDIT_INITIAL_CONTENT,
                        "replace": EDIT_UPDATED_CONTENT,
                    }],
                }),
            )
        }
        MockScenario::Cancellation { .. } => {
            unreachable!("cancellation streams do not issue tool calls")
        }
        MockScenario::Mcp => (
            MCP_CALL_ID,
            MCP_TOOL_NAME,
            serde_json::json!({"text": MCP_INPUT}),
        ),
    };
    let tool_delta = serde_json::json!({
        "id": "rig-integration-1",
        "model": MODEL_ID,
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "arguments": arguments.to_string(),
                    },
                }],
            },
            "finish_reason": null,
        }],
        "usage": null,
    });
    let tool_stop = serde_json::json!({
        "id": "rig-integration-1",
        "model": MODEL_ID,
        "choices": [{"delta": {"tool_calls": []}, "finish_reason": "tool_calls"}],
        "usage": null,
    });
    let usage = serde_json::json!({
        "choices": [],
        "usage": {"prompt_tokens": 20, "completion_tokens": 8, "total_tokens": 28},
    });
    format!("data: {tool_delta}\n\ndata: {tool_stop}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
}

fn shell_arguments(command: &str) -> serde_json::Value {
    serde_json::json!({
        "command": command,
        "is_read_only": false,
        "is_risky": false,
        "uses_pager": false,
        "wait_until_complete": true,
    })
}

fn assert_follow_up_request(scenario: &MockScenario, request: &str) {
    match scenario {
        MockScenario::Read { .. } => {
            assert!(
                request.contains(READ_CALL_ID),
                "follow-up request should preserve the read tool call ID"
            );
            assert!(
                request.contains(FIXTURE_CONTENT),
                "follow-up request should contain the real file contents returned by Galaxy"
            );
        }
        MockScenario::ShellSuccess => {
            assert!(
                request.contains(SHELL_CALL_ID),
                "follow-up request should preserve the shell tool call ID"
            );
            assert!(
                request.contains(SHELL_SUCCESS_OUTPUT),
                "follow-up request should contain successful shell output"
            );
            assert!(
                request.contains("exit code 0"),
                "follow-up request should contain the successful exit code"
            );
        }
        MockScenario::ShellFailure => {
            assert!(
                request.contains(SHELL_CALL_ID),
                "follow-up request should preserve the failed shell tool call ID"
            );
            assert!(
                request.contains(SHELL_FAILURE_OUTPUT),
                "follow-up request should contain failed shell output"
            );
            assert!(
                request.contains("exit code 7"),
                "follow-up request should contain the failed exit code"
            );
            assert!(
                request.contains("[ERROR]"),
                "failed shell result should remain an explicit model error"
            );
        }
        MockScenario::ShellDenied { marker_path } => {
            assert!(
                request.contains(SHELL_CALL_ID),
                "follow-up request should preserve the denied shell tool call ID"
            );
            assert!(
                request.contains("[ERROR] Permission denied by the user"),
                "denied shell result should remain an explicit model error"
            );
            let marker_path = marker_path.lock().expect("marker path lock");
            assert!(
                !Path::new(marker_path.as_str()).exists(),
                "denied shell command must not create its marker file"
            );
        }
        MockScenario::Edit { fixture_path } => {
            assert!(
                request.contains(EDIT_CALL_ID),
                "follow-up request should preserve the edit tool call ID"
            );
            assert!(
                request.contains(EDIT_UPDATED_CONTENT.trim()),
                "follow-up request should contain the updated file content"
            );
            let fixture_path = fixture_path.lock().expect("fixture path lock");
            let actual = std::fs::read_to_string(fixture_path.as_str())
                .expect("edited fixture should remain readable");
            assert_eq!(actual, EDIT_UPDATED_CONTENT);
        }
        MockScenario::Cancellation { .. } => {
            unreachable!("cancellation streams do not issue follow-up requests")
        }
        MockScenario::Mcp => {
            assert!(
                request.contains(MCP_CALL_ID),
                "follow-up request should preserve the MCP tool call ID"
            );
            assert!(
                request.contains(&format!("{TOOL_RESULT_PREFIX}{MCP_INPUT}")),
                "follow-up request should contain the real MCP tool result"
            );
        }
    }
}

fn final_text(scenario: &MockScenario) -> &'static str {
    match scenario {
        MockScenario::Read { .. } => READ_FINAL_TEXT,
        MockScenario::ShellSuccess => SHELL_SUCCESS_FINAL_TEXT,
        MockScenario::ShellFailure => SHELL_FAILURE_FINAL_TEXT,
        MockScenario::ShellDenied { .. } => SHELL_DENIED_FINAL_TEXT,
        MockScenario::Edit { .. } => EDIT_FINAL_TEXT,
        MockScenario::Cancellation { .. } => {
            unreachable!("cancellation streams do not produce final text")
        }
        MockScenario::Mcp => MCP_FINAL_TEXT,
    }
}

fn final_text_sse(final_text: &str) -> String {
    let text = serde_json::json!({
        "id": "rig-integration-2",
        "model": MODEL_ID,
        "choices": [{
            "delta": {"content": final_text, "tool_calls": []},
            "finish_reason": "stop",
        }],
        "usage": null,
    });
    let usage = serde_json::json!({
        "choices": [],
        "usage": {"prompt_tokens": 30, "completion_tokens": 6, "total_tokens": 36},
    });
    format!("data: {text}\n\ndata: {usage}\n\ndata: [DONE]\n\n")
}

fn write_response(stream: &mut TcpStream, content_type: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .expect("should write mock provider response");
    stream.flush().expect("should flush mock provider response");
}

fn write_cancellable_response(
    stream: &mut TcpStream,
    stream_started: &AtomicBool,
    stream_cancelled: &AtomicBool,
    stop: &AtomicBool,
) {
    let text = serde_json::json!({
        "id": "rig-cancellation-1",
        "model": MODEL_ID,
        "choices": [{
            "delta": {"content": "Rig cancellation stream is active.", "tool_calls": []},
            "finish_reason": null,
        }],
        "usage": null,
    });
    let initial_body = format!("data: {text}\n\n");
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n{initial_body}"
    )
    .expect("should start cancellable provider response");
    stream
        .flush()
        .expect("should flush cancellable provider response");
    stream_started.store(true, Ordering::SeqCst);

    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(20));
        if let Err(error) = stream
            .write_all(b": keepalive\n\n")
            .and_then(|()| stream.flush())
        {
            assert!(
                matches!(
                    error.kind(),
                    ErrorKind::BrokenPipe
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::NotConnected
                ),
                "unexpected cancellable stream error: {error}"
            );
            stream_cancelled.store(true, Ordering::SeqCst);
            return;
        }
    }
}
