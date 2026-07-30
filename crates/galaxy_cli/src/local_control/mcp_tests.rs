use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Cursor;

use clap::Parser as _;
use local_control::protocol::{PaneSelector, PaneTarget};
use serde_json::json;

use super::*;
use crate::local_control::{ControlArgs, ControlCommand};

#[derive(Debug, Clone, PartialEq)]
struct Invocation {
    action: ActionKind,
    target: TargetSelector,
    params: Value,
}

struct RecordingInvoker {
    calls: RefCell<Vec<Invocation>>,
    result: Result<Value, ControlError>,
}

impl RecordingInvoker {
    fn succeeding(result: Value) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            result: Ok(result),
        }
    }
}

impl ActionInvoker for RecordingInvoker {
    fn invoke(
        &self,
        action: ActionKind,
        target: TargetSelector,
        params: Value,
    ) -> Result<Value, ControlError> {
        self.calls.borrow_mut().push(Invocation {
            action,
            target,
            params,
        });
        self.result.clone()
    }
}

struct SequencedInvoker {
    calls: RefCell<Vec<Invocation>>,
    results: RefCell<VecDeque<Result<Value, ControlError>>>,
}

impl SequencedInvoker {
    fn new(results: impl IntoIterator<Item = Result<Value, ControlError>>) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            results: RefCell::new(results.into_iter().collect()),
        }
    }
}

impl ActionInvoker for SequencedInvoker {
    fn invoke(
        &self,
        action: ActionKind,
        target: TargetSelector,
        params: Value,
    ) -> Result<Value, ControlError> {
        self.calls.borrow_mut().push(Invocation {
            action,
            target,
            params,
        });
        self.results
            .borrow_mut()
            .pop_front()
            .expect("test invoker has a response for every call")
    }
}

fn terminal_status(block_id: &str, running_for_ms: Option<u64>) -> Value {
    let is_running = running_for_ms.is_some();
    json!({
        "action": ActionKind::TerminalStatus,
        "session_id": "session_1",
        "active_block_id": block_id,
        "is_executing": is_running,
        "is_command_pending": false,
        "is_long_running": is_running,
        "is_agent_in_control": false,
        "is_idle": !is_running,
        "running_for_ms": running_for_ms,
        "command_summary": "cargo test",
    })
}

fn initialize(session: &mut McpSession<'_>) -> Value {
    process(
        session,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "galaxy-cli-test",
                    "version": "1.0.0",
                },
            },
        }),
    )
}

fn process(session: &mut McpSession<'_>, request: Value) -> Value {
    session
        .process_line(&request.to_string())
        .expect("request produces a response")
}

#[test]
fn initialize_negotiates_protocol_and_advertises_tools() {
    let invoker = RecordingInvoker::succeeding(json!({}));
    let mut session = McpSession::new(&invoker, TargetSelector::default());

    let response = initialize(&mut session);

    assert_eq!(response["jsonrpc"], json!("2.0"));
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["result"]["protocolVersion"], json!("2025-06-18"));
    assert_eq!(
        response["result"]["capabilities"]["tools"]["listChanged"],
        json!(false)
    );
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        json!("galaxy-control")
    );
}

#[test]
fn stdio_transport_emits_one_line_per_request_and_skips_notifications() {
    let invoker = RecordingInvoker::succeeding(json!({}));
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "galaxy-cli-test",
                    "version": "1.0.0",
                },
            },
        }),
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {},
        }),
    ]
    .into_iter()
    .map(|request| format!("{request}\n"))
    .collect::<String>();
    let mut output = Vec::new();

    serve_stdio(
        &invoker,
        TargetSelector::default(),
        McpMode::Catalog,
        Cursor::new(requests),
        &mut output,
    )
    .expect("stdio session succeeds");

    let responses = String::from_utf8(output).expect("responses are UTF-8");
    let responses = responses.lines().collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(
        serde_json::from_str::<Value>(responses[0]).expect("initialize response parses")["id"],
        json!(1)
    );
    assert_eq!(
        serde_json::from_str::<Value>(responses[1]).expect("tools response parses")["id"],
        json!(2)
    );
}

#[test]
fn stdio_transport_bounds_and_drains_an_oversized_line_before_the_next_request() {
    let oversized = [vec![b'x'; MAX_REQUEST_BYTES + 32], vec![b'\n']].concat();
    let initialize = format!(
        "{}\n",
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": "galaxy-cli-test",
                    "version": "1.0.0",
                },
            },
        })
    );
    let input = [oversized, initialize.into_bytes()].concat();

    let mut reader = Cursor::new(input.clone());
    let mut bounded_line = Vec::new();
    assert_eq!(
        read_bounded_request_line(&mut reader, &mut bounded_line)
            .expect("oversized line is drained"),
        RequestLineRead::Oversized
    );
    assert_eq!(bounded_line.len(), MAX_REQUEST_BYTES);
    assert_eq!(
        read_bounded_request_line(&mut reader, &mut bounded_line)
            .expect("next request remains readable"),
        RequestLineRead::Complete
    );
    assert!(bounded_line.len() < MAX_REQUEST_BYTES);

    let invoker = RecordingInvoker::succeeding(json!({}));
    let mut output = Vec::new();
    serve_stdio(
        &invoker,
        TargetSelector::default(),
        McpMode::Catalog,
        Cursor::new(input),
        &mut output,
    )
    .expect("stdio session recovers after oversized input");

    let responses = String::from_utf8(output).expect("responses are UTF-8");
    let responses = responses
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("response parses"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["error"]["code"], json!(INVALID_REQUEST));
    assert_eq!(responses[1]["id"], json!(7));
    assert_eq!(
        responses[1]["result"]["protocolVersion"],
        json!("2025-06-18")
    );
}

#[test]
fn tools_list_exposes_capability_and_allowlisted_invocation_schemas() {
    let invoker = RecordingInvoker::succeeding(json!({}));
    let mut session = McpSession::new(&invoker, TargetSelector::default());
    initialize(&mut session);

    let response = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": "tools",
            "method": "tools/list",
            "params": {},
        }),
    );
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools is an array");

    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["name"], json!(CAPABILITIES_TOOL));
    assert_eq!(
        tools[0]["inputSchema"]["additionalProperties"],
        json!(false)
    );
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(tools[1]["name"], json!(INVOKE_TOOL));
    assert_eq!(tools[1]["annotations"]["destructiveHint"], json!(true));

    let actions = tools[1]["inputSchema"]["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum is an array");
    assert!(actions.contains(&json!("app.active")));
    assert!(actions.contains(&json!("input.insert")));
    assert!(actions.contains(&json!("terminal.status")));
    assert!(actions.contains(&json!("terminal.execute")));
    assert!(actions.contains(&json!("terminal.interrupt")));
    assert!(tools[1]["inputSchema"]["allOf"].is_array());
}

#[test]
fn agent_safe_tools_expose_only_pane_pinned_terminal_operations() {
    let invoker = RecordingInvoker::succeeding(json!({}));
    let target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("pane_123".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let mut session = McpSession::agent_safe(&invoker, target);
    let initialized = initialize(&mut session);
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .is_some_and(|instructions| instructions.contains(TERMINAL_INTERRUPT_AT_TOOL))
    );

    let response = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": "tools",
            "method": "tools/list",
            "params": {},
        }),
    );
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools is an array");
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        [
            TERMINAL_STATUS_TOOL,
            TERMINAL_EXECUTE_TOOL,
            TERMINAL_INTERRUPT_TOOL,
            TERMINAL_INTERRUPT_AT_TOOL,
        ]
    );
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], json!(true));
    assert_eq!(tools[1]["annotations"]["destructiveHint"], json!(true));
    assert_eq!(tools[2]["inputSchema"]["required"], json!(["block_id"]));
    assert_eq!(
        tools[3]["inputSchema"]["required"],
        json!(["block_id", "target_running_for_ms"])
    );
    assert_eq!(
        tools[3]["inputSchema"]["properties"]["target_running_for_ms"]["maximum"],
        json!(MAX_INTERRUPT_AT_RUNNING_FOR_MS)
    );
    assert!(!names.contains(&CAPABILITIES_TOOL));
    assert!(!names.contains(&INVOKE_TOOL));
}

#[test]
fn tools_call_discovers_capabilities_through_authenticated_action() {
    let capability_data = json!({
        "action": "capability.list",
        "capabilities": [{
            "kind": "app.active",
            "name": "app.active",
            "implementation_status": "implemented",
            "target_scope": "instance",
            "parameter_spec": "none",
            "result_spec": "active_target",
        }],
    });
    let invoker = RecordingInvoker::succeeding(capability_data.clone());
    let mut session = McpSession::new(&invoker, TargetSelector::default());
    initialize(&mut session);

    let response = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": CAPABILITIES_TOOL,
                "arguments": {},
            },
        }),
    );

    assert_eq!(response["result"]["isError"], json!(false));
    assert_eq!(response["result"]["structuredContent"], capability_data);
    assert_eq!(
        invoker.calls.borrow().as_slice(),
        &[Invocation {
            action: ActionKind::CapabilityList,
            target: TargetSelector::default(),
            params: json!({}),
        }]
    );
}

#[test]
fn tools_call_invokes_catalog_action_with_default_target() {
    let default_target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("pane_123".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = RecordingInvoker::succeeding(json!({
        "action": "input.insert",
        "ok": true,
    }));
    let mut session = McpSession::new(&invoker, default_target.clone());
    initialize(&mut session);

    let response = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": INVOKE_TOOL,
                "arguments": {
                    "action": "input.insert",
                    "params": {
                        "text": "status",
                    },
                },
            },
        }),
    );

    assert_eq!(response["result"]["isError"], json!(false));
    assert_eq!(
        invoker.calls.borrow().as_slice(),
        &[Invocation {
            action: ActionKind::InputInsert,
            target: default_target,
            params: json!({ "text": "status" }),
        }]
    );
}

#[test]
fn tools_call_preserves_terminal_interrupt_block_guard() {
    let default_target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("pane_123".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = RecordingInvoker::succeeding(json!({
        "action": "terminal.interrupt",
        "ok": true,
    }));
    let mut session = McpSession::new(&invoker, default_target.clone());
    initialize(&mut session);

    let response = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": INVOKE_TOOL,
                "arguments": {
                    "action": "terminal.interrupt",
                    "params": {
                        "block_id": "session_1-42",
                    },
                },
            },
        }),
    );

    assert_eq!(response["result"]["isError"], json!(false));
    assert_eq!(
        invoker.calls.borrow().as_slice(),
        &[Invocation {
            action: ActionKind::TerminalInterrupt,
            target: default_target,
            params: json!({ "block_id": "session_1-42" }),
        }]
    );
}

#[test]
fn agent_safe_terminal_calls_cannot_override_the_delegated_target() {
    let target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("delegated_pane".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = RecordingInvoker::succeeding(json!({ "ok": true }));
    let mut session = McpSession::agent_safe(&invoker, target.clone());
    initialize(&mut session);

    for (id, name, arguments, action, params) in [
        (
            1,
            TERMINAL_STATUS_TOOL,
            json!({}),
            ActionKind::TerminalStatus,
            json!({}),
        ),
        (
            2,
            TERMINAL_EXECUTE_TOOL,
            json!({ "command": "cargo test" }),
            ActionKind::TerminalExecute,
            json!({ "command": "cargo test" }),
        ),
        (
            3,
            TERMINAL_INTERRUPT_TOOL,
            json!({ "block_id": "block-42" }),
            ActionKind::TerminalInterrupt,
            json!({ "block_id": "block-42" }),
        ),
    ] {
        let response = process(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments,
                },
            }),
        );
        assert_eq!(response["result"]["isError"], json!(false));
        let call = invoker
            .calls
            .borrow()
            .last()
            .cloned()
            .expect("recorded call");
        assert_eq!(
            call,
            Invocation {
                action,
                target: target.clone(),
                params,
            }
        );
    }

    let generic_call = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": INVOKE_TOOL,
                "arguments": {
                    "action": "pane.close",
                    "target": {
                        "pane": {
                            "type": "id",
                            "id": "other_pane",
                        },
                    },
                },
            },
        }),
    );
    assert_eq!(generic_call["error"]["code"], json!(INVALID_PARAMS));
    assert_eq!(invoker.calls.borrow().len(), 3);
}

#[test]
fn agent_safe_terminal_execute_rejects_recognized_ssh_launches() {
    let target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("delegated_pane".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = RecordingInvoker::succeeding(json!({ "ok": true }));
    let mut session = McpSession::agent_safe(&invoker, target);
    initialize(&mut session);

    for (id, command) in [
        (1, "ssh user@example.com"),
        (2, "cd /tmp && sudo -u root ssh user@example.com"),
        (3, "bash -lc 'ssh user@example.com'"),
    ] {
        let response = process(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": TERMINAL_EXECUTE_TOOL,
                    "arguments": { "command": command },
                },
            }),
        );

        assert_eq!(response["result"]["isError"], json!(true));
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("cannot start a recognized SSH"))
        );
    }
    assert!(invoker.calls.borrow().is_empty());
}

#[test]
fn interrupt_at_waits_for_the_exact_block_then_uses_the_guarded_interrupt() {
    let target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("delegated_pane".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = SequencedInvoker::new([
        Ok(terminal_status("block-42", Some(50))),
        Ok(terminal_status("block-42", Some(75))),
        Ok(json!({
            "action": ActionKind::TerminalInterrupt,
            "ok": true,
            "block_id": "block-42",
        })),
    ]);

    let result = terminal_interrupt_at(&invoker, target.clone(), "block-42", 75, |_| {});

    assert_eq!(result["isError"], json!(false));
    assert_eq!(result["structuredContent"]["interrupted"], json!(true));
    assert_eq!(
        result["structuredContent"]["observed_running_for_ms"],
        json!(75)
    );
    assert_eq!(
        invoker.calls.borrow().as_slice(),
        &[
            Invocation {
                action: ActionKind::TerminalStatus,
                target: target.clone(),
                params: json!({}),
            },
            Invocation {
                action: ActionKind::TerminalStatus,
                target: target.clone(),
                params: json!({}),
            },
            Invocation {
                action: ActionKind::TerminalInterrupt,
                target,
                params: json!({ "block_id": "block-42" }),
            },
        ]
    );
}

#[test]
fn interrupt_at_returns_without_interrupting_when_block_stops_or_changes() {
    for (status, reason) in [
        (
            terminal_status("replacement-block", Some(70)),
            "block_changed",
        ),
        (terminal_status("block-42", None), "block_stopped"),
    ] {
        let target = TargetSelector {
            pane: Some(PaneTarget::Id {
                id: PaneSelector("delegated_pane".to_owned()),
            }),
            ..TargetSelector::default()
        };
        let invoker = SequencedInvoker::new([Ok(status)]);

        let result = terminal_interrupt_at(&invoker, target, "block-42", 75, |_| {});

        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["structuredContent"]["interrupted"], json!(false));
        assert_eq!(result["structuredContent"]["reason"], json!(reason));
        assert_eq!(invoker.calls.borrow().len(), 1);
        assert_eq!(invoker.calls.borrow()[0].action, ActionKind::TerminalStatus);
    }
}

#[test]
fn interrupt_at_tool_dispatches_and_validates_the_duration_cap() {
    let target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("delegated_pane".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = SequencedInvoker::new([
        Ok(terminal_status("block-42", Some(75))),
        Ok(json!({
            "action": ActionKind::TerminalInterrupt,
            "ok": true,
            "block_id": "block-42",
        })),
    ]);
    let mut session = McpSession::agent_safe(&invoker, target);
    initialize(&mut session);

    let response = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": TERMINAL_INTERRUPT_AT_TOOL,
                "arguments": {
                    "block_id": "block-42",
                    "target_running_for_ms": 75,
                },
            },
        }),
    );

    assert_eq!(response["result"]["isError"], json!(false));
    assert_eq!(
        response["result"]["structuredContent"]["interrupted"],
        json!(true)
    );
    assert_eq!(invoker.calls.borrow().len(), 2);

    let capped_invoker = RecordingInvoker::succeeding(json!({ "ok": true }));
    let mut capped_session = McpSession::agent_safe(&capped_invoker, TargetSelector::default());
    initialize(&mut capped_session);
    let response = process(
        &mut capped_session,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": TERMINAL_INTERRUPT_AT_TOOL,
                "arguments": {
                    "block_id": "block-42",
                    "target_running_for_ms": MAX_INTERRUPT_AT_RUNNING_FOR_MS + 1,
                },
            },
        }),
    );

    assert_eq!(response["result"]["isError"], json!(true));
    assert!(capped_invoker.calls.borrow().is_empty());
}

#[test]
fn malformed_json_rpc_requests_fail_without_invoking_local_control() {
    let invoker = RecordingInvoker::succeeding(json!({}));
    let mut session = McpSession::new(&invoker, TargetSelector::default());

    let parse_error = session
        .process_line("{not json")
        .expect("parse error produces response");
    assert_eq!(parse_error["error"]["code"], json!(PARSE_ERROR));
    assert_eq!(parse_error["id"], Value::Null);

    let invalid_request = session
        .process_line("[]")
        .expect("invalid request produces response");
    assert_eq!(invalid_request["error"]["code"], json!(INVALID_REQUEST));

    let before_initialize = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/list",
        }),
    );
    assert_eq!(
        before_initialize["error"]["code"],
        json!(SERVER_NOT_INITIALIZED)
    );

    initialize(&mut session);
    let bad_call = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": INVOKE_TOOL,
                "arguments": {
                    "action": "input.insert",
                    "instance": "inst_other",
                    "params": {
                        "text": "must not run",
                    },
                },
            },
        }),
    );
    assert_eq!(bad_call["result"]["isError"], json!(true));
    assert_eq!(
        bad_call["result"]["structuredContent"]["error"]["code"],
        json!("invalid_arguments")
    );
    assert!(invoker.calls.borrow().is_empty());
}

#[test]
fn mcp_subcommand_accepts_pid_and_target_selectors() {
    let args = ControlArgs::try_parse_from([
        "galaxyctrl",
        "mcp",
        "--pid",
        "4321",
        "--window",
        "window_1",
        "--pane",
        "pane_2",
    ])
    .expect("mcp arguments parse");
    let ControlCommand::Mcp(mcp) = args.command else {
        panic!("expected mcp command");
    };

    assert_eq!(mcp.target.pid, Some(4321));
    assert_eq!(mcp.target.window.as_deref(), Some("window_1"));
    assert_eq!(mcp.target.pane.as_deref(), Some("pane_2"));
    assert!(!mcp.agent_safe);
    assert!(!mcp.allow_terminal_execute);
    assert!(!mcp.allow_terminal_interrupt);
}

#[test]
fn agent_safe_permissions_are_explicit_hidden_capabilities() {
    let args = ControlArgs::try_parse_from([
        "galaxyctrl",
        "mcp",
        "--pid",
        "4321",
        "--window",
        "window_1",
        "--tab",
        "tab_1",
        "--pane",
        "pane_2",
        "--agent-safe",
        "--allow-terminal-execute",
        "--allow-terminal-interrupt",
    ])
    .expect("agent-safe MCP arguments parse");
    let ControlCommand::Mcp(mcp) = args.command else {
        panic!("expected mcp command");
    };

    assert!(mcp.agent_safe);
    assert!(mcp.allow_terminal_execute);
    assert!(mcp.allow_terminal_interrupt);
}

#[test]
fn agent_safe_read_only_mode_does_not_advertise_or_run_mutations() {
    let target = TargetSelector {
        pane: Some(PaneTarget::Id {
            id: PaneSelector("delegated_pane".to_owned()),
        }),
        ..TargetSelector::default()
    };
    let invoker = RecordingInvoker::succeeding(json!({ "ok": true }));
    let mut session = McpSession::agent_safe_read_only(&invoker, target);
    initialize(&mut session);

    let listed = process(
        &mut session,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {},
        }),
    );
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], json!(TERMINAL_STATUS_TOOL));

    for (id, name, arguments) in [
        (2, TERMINAL_EXECUTE_TOOL, json!({ "command": "cargo test" })),
        (
            3,
            TERMINAL_INTERRUPT_TOOL,
            json!({ "block_id": "block-42" }),
        ),
        (
            4,
            TERMINAL_INTERRUPT_AT_TOOL,
            json!({
                "block_id": "block-42",
                "target_running_for_ms": 75,
            }),
        ),
    ] {
        let response = process(
            &mut session,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": name,
                    "arguments": arguments,
                },
            }),
        );
        assert_eq!(response["error"]["code"], json!(INVALID_PARAMS));
    }
    assert!(invoker.calls.borrow().is_empty());
}

#[test]
fn agent_safe_mcp_requires_one_exact_pane_or_session() {
    let exact_pane = TargetArgs {
        window: Some("window_1".to_owned()),
        tab: Some("tab_1".to_owned()),
        pane: Some("Pane Terminal (42)".to_owned()),
        ..TargetArgs::default()
    };
    assert!(validate_agent_safe_target(&exact_pane).is_ok());

    let exact_session = TargetArgs {
        window: Some("window_1".to_owned()),
        tab: Some("tab_1".to_owned()),
        session: Some("session_1".to_owned()),
        ..TargetArgs::default()
    };
    assert!(validate_agent_safe_target(&exact_session).is_ok());

    for invalid in [
        TargetArgs::default(),
        TargetArgs {
            window: Some("window_1".to_owned()),
            tab: Some("tab_1".to_owned()),
            pane: Some("active".to_owned()),
            ..TargetArgs::default()
        },
        TargetArgs {
            window: Some("window_1".to_owned()),
            tab: Some("tab_1".to_owned()),
            pane: Some("pane_1".to_owned()),
            session: Some("session_1".to_owned()),
            ..TargetArgs::default()
        },
        TargetArgs {
            pane: Some("pane_1".to_owned()),
            window: Some("window_1".to_owned()),
            ..TargetArgs::default()
        },
        TargetArgs {
            window: Some("active".to_owned()),
            tab: Some("tab_1".to_owned()),
            pane: Some("pane_1".to_owned()),
            ..TargetArgs::default()
        },
    ] {
        let error = validate_agent_safe_target(&invalid).expect_err("target is rejected");
        assert_eq!(error.code, ErrorCode::InvalidSelector);
    }
}

#[test]
fn instance_selection_is_pinned_explicitly_and_rejects_ambiguity() {
    let mut one =
        InstanceRecord::for_current_process(None, "dev", "dev.galaxy.Galaxy", None, Vec::new());
    one.instance_id = InstanceId("inst_one".to_owned());
    one.pid = 100;
    let mut two = one.clone();
    two.instance_id = InstanceId("inst_two".to_owned());
    two.pid = 200;
    let records = vec![one, two];

    let pinned = pin_instance(&records, &InstanceSelector::Pid(200)).expect("pid pins instance");
    assert_eq!(pinned.instance_id.0, "inst_two");

    let error = pin_instance(&records, &InstanceSelector::Active)
        .expect_err("unqualified selection is ambiguous");
    assert_eq!(error.code, ErrorCode::AmbiguousInstance);
}
