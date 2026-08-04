use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use warp::features::FeatureFlag;
use warp::integration_testing::agent_mode::{
    assert_latest_exchange_text, enter_agent_view, set_preferred_agent_mode_llm,
    submit_ai_query_and_wait_until_done,
};
use warp::integration_testing::step::new_step_with_default_assertions;
use warp::integration_testing::terminal::wait_until_bootstrapped_single_pane_for_tab;

use super::new_builder;
use crate::Builder;

const MODEL_ID: &str = "integration-rig-model";
const FINAL_TEXT: &str = "Rig read round trip completed.";
const FIXTURE_CONTENT: &str = "content returned through the Galaxy read executor";

pub fn test_rig_read_tool_round_trip() -> Builder {
    FeatureFlag::AgentView.set_enabled(true);
    let fixture_path = Arc::new(Mutex::new(String::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let (address, server_thread) = start_mock_provider(fixture_path.clone(), stop.clone());
    let server_thread = Arc::new(Mutex::new(Some(server_thread)));

    let setup_fixture_path = fixture_path.clone();
    let cleanup_stop = stop.clone();
    let cleanup_thread = server_thread.clone();
    new_builder()
        .with_setup(move |utils| {
            let fixture = utils.test_dir().join("rig-read-fixture.txt");
            std::fs::write(&fixture, FIXTURE_CONTENT)
                .expect("should write Rig integration fixture");
            *setup_fixture_path.lock().expect("fixture path lock") =
                fixture.to_string_lossy().into_owned();

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
        })
        .with_cleanup(move |_utils| {
            cleanup_stop.store(true, Ordering::SeqCst);
            if let Some(handle) = cleanup_thread.lock().expect("server thread lock").take() {
                handle.join().expect("mock provider should stop cleanly");
            }
        })
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
                    assert_latest_exchange_text(|text| text.contains(FINAL_TEXT)),
                ),
        )
}

fn start_mock_provider(
    fixture_path: Arc<Mutex<String>>,
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
                    serve_request(&mut stream, &fixture_path, &request_count);
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
    fixture_path: &Mutex<String>,
    request_count: &AtomicUsize,
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
    let body = match turn {
        0 => {
            let fixture = fixture_path.lock().expect("fixture path lock").clone();
            tool_call_sse(&fixture)
        }
        1 => {
            assert!(
                request.contains("rig-read-call"),
                "follow-up request should preserve the tool call ID"
            );
            assert!(
                request.contains(FIXTURE_CONTENT),
                "follow-up request should contain the real file contents returned by Galaxy"
            );
            final_text_sse()
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

fn tool_call_sse(fixture_path: &str) -> String {
    let arguments = serde_json::json!({"files": [fixture_path]}).to_string();
    let tool_delta = serde_json::json!({
        "id": "rig-integration-1",
        "model": MODEL_ID,
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "rig-read-call",
                    "type": "function",
                    "function": {
                        "name": "read_files",
                        "arguments": arguments,
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

fn final_text_sse() -> String {
    let text = serde_json::json!({
        "id": "rig-integration-2",
        "model": MODEL_ID,
        "choices": [{
            "delta": {"content": FINAL_TEXT, "tool_calls": []},
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
