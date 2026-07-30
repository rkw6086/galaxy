//! MCP stdio adapter for the authenticated Galaxy Control protocol.
//!
//! This module intentionally contains no app-side control implementation. Every
//! tool invocation is translated into an existing allowlisted local-control
//! action, obtains an action-scoped credential through the owner-authenticated
//! broker, and is sent to the selected app's loopback control endpoint.

use std::io::{BufRead, Write};
use std::time::Duration;

use ::local_control::remote_command::is_potential_remote_ssh_command;
use galaxy_core::channel::ChannelState;
use instant::Instant;
use local_control::discovery::{InstanceId, InstanceRecord};
use local_control::protocol::{
    Action, ActionKind, ActionNameParams, ActionParameterSpec, BindingNameParams,
    BooleanValueParams, ColorValueParams, ControlError, ControlResponse, DirectionParams,
    EmptyParams, ErrorCode, FileOpenParams, KeyParams, KeyValueParams, NamespaceParams,
    PageQueryParams, QueryParams, RenameParams, RequestEnvelope, ResizeParams, TabActivateParams,
    TabCloseParams, TabCreateParams, TargetSelector, TerminalExecuteParams,
    TerminalInterruptParams, TerminalStatusResult, TextParams, ThemeNameParams,
};
use local_control::selection::{InstanceSelector, select_instance};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::local_control::selectors::{instance_selector, target_selector};
use crate::local_control::{McpArgs, TargetArgs};

const JSON_RPC_VERSION: &str = "2.0";
const LATEST_MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_MCP_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const CAPABILITIES_TOOL: &str = "galaxy_control_capabilities";
const INVOKE_TOOL: &str = "galaxy_control_invoke";
const TERMINAL_STATUS_TOOL: &str = "galaxy_terminal_status";
const TERMINAL_EXECUTE_TOOL: &str = "galaxy_terminal_execute";
const TERMINAL_INTERRUPT_TOOL: &str = "galaxy_terminal_interrupt";
const TERMINAL_INTERRUPT_AT_TOOL: &str = "galaxy_terminal_interrupt_at";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_AGENT_BLOCK_ID_BYTES: usize = 4 * 1024;
const MAX_INTERRUPT_AT_RUNNING_FOR_MS: u64 = 60 * 60 * 1_000;
const INTERRUPT_AT_POLL_INTERVAL_MS: u64 = 100;
const INTERRUPT_AT_POLL_INTERVAL: Duration = Duration::from_millis(INTERRUPT_AT_POLL_INTERVAL_MS);
const INTERRUPT_AT_WAIT_GRACE: Duration = Duration::from_secs(30);

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_NOT_INITIALIZED: i64 = -32002;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum McpMode {
    Catalog,
    AgentSafe {
        allow_execute: bool,
        allow_interrupt: bool,
    },
}

pub(super) fn run_mcp_server(args: McpArgs) -> Result<(), ControlError> {
    let mode = if args.agent_safe {
        validate_agent_safe_target(&args.target)?;
        McpMode::AgentSafe {
            allow_execute: args.allow_terminal_execute,
            allow_interrupt: args.allow_terminal_interrupt,
        }
    } else {
        McpMode::Catalog
    };
    let channel = ChannelState::channel()
        .local_control_channel_name()
        .to_owned();
    let target = target_selector(&args.target)?;
    let records = local_control::discovery::list_instances(&channel);
    let instance = pin_instance(&records, &instance_selector(&args.target))?;
    let invoker = AuthenticatedInvoker {
        channel,
        instance_id: instance.instance_id,
    };

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve_stdio(&invoker, target, mode, stdin.lock(), stdout.lock())
}

fn validate_agent_safe_target(args: &TargetArgs) -> Result<(), ControlError> {
    let is_exact = |selector: &str| !selector.is_empty() && selector != "active";
    let has_exact_window = args.window.as_deref().is_some_and(is_exact);
    let has_exact_tab = args.tab.as_deref().is_some_and(is_exact);
    let has_exact_pane = args.pane.as_deref().is_some_and(is_exact);
    let has_exact_session = args.session.as_deref().is_some_and(is_exact);
    let has_non_exact_target = args.window.as_deref().is_some_and(|value| !is_exact(value))
        || args.tab.as_deref().is_some_and(|value| !is_exact(value))
        || args.pane.as_deref().is_some_and(|value| !is_exact(value))
        || args
            .session
            .as_deref()
            .is_some_and(|value| !is_exact(value))
        || args.window_index.is_some()
        || args.window_title.is_some()
        || args.tab_index.is_some()
        || args.tab_title.is_some()
        || args.pane_index.is_some();
    if has_non_exact_target
        || !has_exact_window
        || !has_exact_tab
        || has_exact_pane == has_exact_session
    {
        return Err(ControlError::new(
            ErrorCode::InvalidSelector,
            "agent-safe MCP requires opaque --window and --tab selectors plus exactly one opaque --pane or --session selector",
        ));
    }
    Ok(())
}

fn pin_instance(
    records: &[InstanceRecord],
    selector: &InstanceSelector,
) -> Result<InstanceRecord, ControlError> {
    select_instance(records, selector)
}

trait ActionInvoker {
    fn invoke(
        &self,
        action: ActionKind,
        target: TargetSelector,
        params: Value,
    ) -> Result<Value, ControlError>;
}

struct AuthenticatedInvoker {
    channel: String,
    instance_id: InstanceId,
}

impl ActionInvoker for AuthenticatedInvoker {
    fn invoke(
        &self,
        action: ActionKind,
        target: TargetSelector,
        params: Value,
    ) -> Result<Value, ControlError> {
        // Resolve the pinned opaque ID again for every call. This refreshes
        // endpoint metadata while refusing to drift to another app process.
        let records = local_control::discovery::list_instances(&self.channel);
        let instance = select_instance(&records, &InstanceSelector::Id(self.instance_id.clone()))?;
        let mut request = RequestEnvelope::new(Action {
            kind: action,
            params,
        });
        request.target = target;

        let response = local_control::client::send_request(&instance, &request)?;
        match response.response {
            ControlResponse::Ok { data } => Ok(data),
            ControlResponse::Error { error } => Err(error),
        }
    }
}

fn serve_stdio(
    invoker: &dyn ActionInvoker,
    default_target: TargetSelector,
    mode: McpMode,
    mut reader: impl BufRead,
    mut writer: impl Write,
) -> Result<(), ControlError> {
    let mut session = McpSession::with_mode(invoker, default_target, mode);
    let mut line = Vec::new();

    loop {
        let line_read = read_bounded_request_line(&mut reader, &mut line).map_err(|error| {
            ControlError::with_details(
                ErrorCode::TransportUnavailable,
                "failed to read MCP stdio request",
                error.to_string(),
            )
        })?;
        let response = match line_read {
            RequestLineRead::EndOfInput => return Ok(()),
            RequestLineRead::Oversized => Some(json_rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "MCP request exceeds the 1 MiB limit",
                None,
            )),
            RequestLineRead::Complete => {
                let line = std::str::from_utf8(&line).map_err(|error| {
                    ControlError::with_details(
                        ErrorCode::TransportUnavailable,
                        "failed to read MCP stdio request",
                        error.to_string(),
                    )
                })?;
                if line.trim().is_empty() {
                    continue;
                }
                session.process_line(line.trim_end_matches(['\r', '\n']))
            }
        };
        let Some(response) = response else {
            continue;
        };

        serde_json::to_writer(&mut writer, &response).map_err(|error| {
            ControlError::with_details(
                ErrorCode::TransportUnavailable,
                "failed to serialize MCP stdio response",
                error.to_string(),
            )
        })?;
        writeln!(&mut writer).map_err(|error| {
            ControlError::with_details(
                ErrorCode::TransportUnavailable,
                "failed to write MCP stdio response",
                error.to_string(),
            )
        })?;
        writer.flush().map_err(|error| {
            ControlError::with_details(
                ErrorCode::TransportUnavailable,
                "failed to flush MCP stdio response",
                error.to_string(),
            )
        })?;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestLineRead {
    EndOfInput,
    Complete,
    Oversized,
}

fn read_bounded_request_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
) -> std::io::Result<RequestLineRead> {
    line.clear();
    let mut read_any = false;
    let mut oversized = false;

    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if !read_any {
                RequestLineRead::EndOfInput
            } else if oversized {
                RequestLineRead::Oversized
            } else {
                RequestLineRead::Complete
            });
        }

        read_any = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if !oversized {
            let remaining = MAX_REQUEST_BYTES.saturating_sub(line.len());
            let copied = consumed.min(remaining);
            line.extend_from_slice(&available[..copied]);
            oversized = copied < consumed;
        }
        reader.consume(consumed);

        if newline.is_some() {
            return Ok(if oversized {
                RequestLineRead::Oversized
            } else {
                RequestLineRead::Complete
            });
        }
    }
}

struct McpSession<'a> {
    invoker: &'a dyn ActionInvoker,
    default_target: TargetSelector,
    mode: McpMode,
    initialized: bool,
}

impl<'a> McpSession<'a> {
    #[cfg(test)]
    fn new(invoker: &'a dyn ActionInvoker, default_target: TargetSelector) -> Self {
        Self::with_mode(invoker, default_target, McpMode::Catalog)
    }

    #[cfg(test)]
    fn agent_safe(invoker: &'a dyn ActionInvoker, default_target: TargetSelector) -> Self {
        Self::with_mode(
            invoker,
            default_target,
            McpMode::AgentSafe {
                allow_execute: true,
                allow_interrupt: true,
            },
        )
    }

    #[cfg(test)]
    fn agent_safe_read_only(
        invoker: &'a dyn ActionInvoker,
        default_target: TargetSelector,
    ) -> Self {
        Self::with_mode(
            invoker,
            default_target,
            McpMode::AgentSafe {
                allow_execute: false,
                allow_interrupt: false,
            },
        )
    }

    fn with_mode(
        invoker: &'a dyn ActionInvoker,
        default_target: TargetSelector,
        mode: McpMode,
    ) -> Self {
        Self {
            invoker,
            default_target,
            mode,
            initialized: false,
        }
    }

    fn process_line(&mut self, line: &str) -> Option<Value> {
        match serde_json::from_str(line) {
            Ok(request) => self.handle_message(request),
            Err(error) => Some(json_rpc_error(
                Value::Null,
                PARSE_ERROR,
                "Parse error",
                Some(json!({ "details": error.to_string() })),
            )),
        }
    }

    fn handle_message(&mut self, request: Value) -> Option<Value> {
        let Some(object) = request.as_object() else {
            return Some(json_rpc_error(
                Value::Null,
                INVALID_REQUEST,
                "Invalid Request",
                None,
            ));
        };

        let id = object.get("id").cloned();
        let response_id = id.clone().unwrap_or(Value::Null);
        if object.get("jsonrpc").and_then(Value::as_str) != Some(JSON_RPC_VERSION)
            || object
                .get("method")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || id
                .as_ref()
                .is_some_and(|id| !matches!(id, Value::Null | Value::Number(_) | Value::String(_)))
        {
            return Some(json_rpc_error(
                response_id,
                INVALID_REQUEST,
                "Invalid Request",
                None,
            ));
        }

        let method = object
            .get("method")
            .and_then(Value::as_str)
            .expect("validated method");
        let params = object.get("params");
        id.as_ref()?;

        let result = match method {
            "initialize" => self.initialize(params),
            "ping" => Ok(json!({})),
            _ if !self.initialized => Err(RpcError::new(
                SERVER_NOT_INITIALIZED,
                "MCP server has not been initialized",
            )),
            "tools/list" => self.list_tools(params),
            "tools/call" => self.call_tool(params),
            _ => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("Method not found: {method}"),
            )),
        };

        Some(match result {
            Ok(result) => json!({
                "jsonrpc": JSON_RPC_VERSION,
                "id": response_id,
                "result": result,
            }),
            Err(error) => json_rpc_error(response_id, error.code, error.message, error.data),
        })
    }

    fn initialize(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        if self.initialized {
            return Err(RpcError::new(
                INVALID_REQUEST,
                "MCP server is already initialized",
            ));
        }
        let params = required_object(params, "initialize params")?;
        let requested_version = required_string(params, "protocolVersion")?;
        required_object(params.get("capabilities"), "client capabilities")?;
        let client_info = required_object(params.get("clientInfo"), "client info")?;
        required_string(client_info, "name")?;
        required_string(client_info, "version")?;

        let protocol_version = if SUPPORTED_MCP_PROTOCOL_VERSIONS.contains(&requested_version) {
            requested_version
        } else {
            LATEST_MCP_PROTOCOL_VERSION
        };
        self.initialized = true;

        let instructions = match self.mode {
            McpMode::Catalog => {
                "Inspect galaxy_control_capabilities before invoking an action. All calls use Galaxy's existing allowlisted local-control protocol and remain pinned to the app instance selected when this server started."
            }
            McpMode::AgentSafe { .. } => {
                "Use the concrete Galaxy terminal tools exposed for the exact local terminal pane delegated to this agent. Available mutation tools reflect the user's active Galaxy permissions. Terminal execution requires an idle pane and rejects recognized SSH-session launches; this narrow guard is not a general network sandbox. Interruption requires the exact active block ID returned by galaxy_terminal_status. For an elapsed-time deadline, call galaxy_terminal_interrupt_at once so Galaxy monitors that exact block and interrupts it without relying on model polling."
            }
        };
        Ok(json!({
            "protocolVersion": protocol_version,
            "capabilities": {
                "tools": {
                    "listChanged": false,
                },
            },
            "serverInfo": {
                "name": "galaxy-control",
                "title": "Galaxy Control",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": instructions,
        }))
    }

    fn list_tools(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        if let Some(params) = params {
            let params = required_object(Some(params), "tools/list params")?;
            if params.get("cursor").is_some_and(|cursor| !cursor.is_null()) {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "tools/list pagination is not supported",
                ));
            }
        }

        let tools = match self.mode {
            McpMode::Catalog => vec![capabilities_tool_definition(), invoke_tool_definition()],
            McpMode::AgentSafe {
                allow_execute,
                allow_interrupt,
            } => {
                let mut tools = vec![terminal_status_tool_definition()];
                if allow_execute {
                    tools.push(terminal_execute_tool_definition());
                }
                if allow_interrupt {
                    tools.push(terminal_interrupt_tool_definition());
                    tools.push(terminal_interrupt_at_tool_definition());
                }
                tools
            }
        };
        Ok(json!({ "tools": tools }))
    }

    fn call_tool(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = required_object(params, "tools/call params")?;
        reject_unknown_keys(
            params,
            &["name", "arguments", "_meta", "task"],
            "tools/call params",
        )
        .map_err(invalid_params)?;
        if params.get("task").is_some_and(|task| !task.is_null()) {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "task-augmented tool calls are not supported",
            ));
        }
        let name = required_string(params, "name")?;
        let empty_arguments = Map::new();
        let arguments = match params.get("arguments") {
            Some(arguments) => required_object(Some(arguments), "tool arguments")?,
            None => &empty_arguments,
        };

        match (self.mode, name) {
            (McpMode::AgentSafe { .. }, TERMINAL_STATUS_TOOL) => {
                if !arguments.is_empty() {
                    return Ok(tool_argument_error(
                        "galaxy_terminal_status does not accept arguments",
                    ));
                }
                Ok(self.invoke_tool(
                    ActionKind::TerminalStatus,
                    self.default_target.clone(),
                    json!({}),
                ))
            }
            (
                McpMode::AgentSafe {
                    allow_execute: true,
                    ..
                },
                TERMINAL_EXECUTE_TOOL,
            ) => {
                reject_unknown_keys(arguments, &["command"], "galaxy_terminal_execute arguments")
                    .map_err(invalid_params)?;
                let command = required_string(arguments, "command")?;
                if is_potential_remote_ssh_command(command) {
                    return Ok(tool_argument_error(
                        "galaxy_terminal_execute cannot start a recognized SSH-backed session; ACP terminal tools remain pinned to local sessions",
                    ));
                }
                Ok(self.invoke_tool(
                    ActionKind::TerminalExecute,
                    self.default_target.clone(),
                    json!({ "command": command }),
                ))
            }
            (
                McpMode::AgentSafe {
                    allow_interrupt: true,
                    ..
                },
                TERMINAL_INTERRUPT_TOOL,
            ) => {
                reject_unknown_keys(
                    arguments,
                    &["block_id"],
                    "galaxy_terminal_interrupt arguments",
                )
                .map_err(invalid_params)?;
                let block_id = required_string(arguments, "block_id")?;
                Ok(self.invoke_tool(
                    ActionKind::TerminalInterrupt,
                    self.default_target.clone(),
                    json!({ "block_id": block_id }),
                ))
            }
            (
                McpMode::AgentSafe {
                    allow_interrupt: true,
                    ..
                },
                TERMINAL_INTERRUPT_AT_TOOL,
            ) => {
                reject_unknown_keys(
                    arguments,
                    &["block_id", "target_running_for_ms"],
                    "galaxy_terminal_interrupt_at arguments",
                )
                .map_err(invalid_params)?;
                let block_id = required_string(arguments, "block_id")?;
                let target_running_for_ms = required_u64(arguments, "target_running_for_ms")?;
                if block_id.len() > MAX_AGENT_BLOCK_ID_BYTES || block_id.as_bytes().contains(&0) {
                    return Ok(tool_argument_error(format!(
                        "block_id must contain at most {MAX_AGENT_BLOCK_ID_BYTES} bytes and no NUL characters"
                    )));
                }
                if target_running_for_ms > MAX_INTERRUPT_AT_RUNNING_FOR_MS {
                    return Ok(tool_argument_error(format!(
                        "target_running_for_ms must not exceed {MAX_INTERRUPT_AT_RUNNING_FOR_MS}"
                    )));
                }
                Ok(terminal_interrupt_at(
                    self.invoker,
                    self.default_target.clone(),
                    block_id,
                    target_running_for_ms,
                    std::thread::sleep,
                ))
            }
            (McpMode::AgentSafe { .. }, _) => Err(RpcError::new(
                INVALID_PARAMS,
                format!("unknown agent-safe MCP tool {name:?}"),
            )),
            (McpMode::Catalog, CAPABILITIES_TOOL) => {
                if !arguments.is_empty() {
                    return Ok(tool_argument_error(
                        "galaxy_control_capabilities does not accept arguments",
                    ));
                }
                Ok(
                    match self.invoker.invoke(
                        ActionKind::CapabilityList,
                        TargetSelector::default(),
                        json!({}),
                    ) {
                        Ok(data) => successful_tool_result(data),
                        Err(error) => control_tool_error(error),
                    },
                )
            }
            (McpMode::Catalog, INVOKE_TOOL) => Ok(
                match parse_invoke_arguments(arguments, &self.default_target) {
                    Ok((action, target, params)) => {
                        match self.invoker.invoke(action, target, params) {
                            Ok(data) => successful_tool_result(data),
                            Err(error) => control_tool_error(error),
                        }
                    }
                    Err(message) => tool_argument_error(message),
                },
            ),
            (McpMode::Catalog, _) => Err(RpcError::new(
                INVALID_PARAMS,
                format!("unknown MCP tool {name:?}"),
            )),
        }
    }

    fn invoke_tool(&self, action: ActionKind, target: TargetSelector, params: Value) -> Value {
        match self.invoker.invoke(action, target, params) {
            Ok(data) => successful_tool_result(data),
            Err(error) => control_tool_error(error),
        }
    }
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

fn invalid_params(message: String) -> RpcError {
    RpcError::new(INVALID_PARAMS, message)
}

fn json_rpc_error(id: Value, code: i64, message: impl Into<String>, data: Option<Value>) -> Value {
    let mut error = json!({
        "code": code,
        "message": message.into(),
    });
    if let Some(data) = data {
        error
            .as_object_mut()
            .expect("error object")
            .insert("data".to_owned(), data);
    }
    json!({
        "jsonrpc": JSON_RPC_VERSION,
        "id": id,
        "error": error,
    })
}

fn required_object<'a>(
    value: Option<&'a Value>,
    description: &str,
) -> Result<&'a Map<String, Value>, RpcError> {
    value
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("{description} must be an object")))
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("{key} must be a non-empty string")))
}

fn required_u64(object: &Map<String, Value>, key: &str) -> Result<u64, RpcError> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("{key} must be an unsigned integer")))
}

fn parse_invoke_arguments(
    arguments: &Map<String, Value>,
    default_target: &TargetSelector,
) -> Result<(ActionKind, TargetSelector, Value), String> {
    reject_unknown_keys(
        arguments,
        &["action", "target", "params"],
        "galaxy_control_invoke arguments",
    )?;
    let action_name = arguments
        .get("action")
        .and_then(Value::as_str)
        .filter(|action| !action.is_empty())
        .ok_or_else(|| "action must be a non-empty string".to_owned())?;
    let action: ActionKind = serde_json::from_value(Value::String(action_name.to_owned()))
        .map_err(|_| format!("unknown Galaxy Control action {action_name:?}"))?;
    if !action.is_implemented() {
        return Err(format!(
            "Galaxy Control action {action_name:?} is not implemented"
        ));
    }

    let explicit_target = arguments
        .get("target")
        .map(|target| {
            validate_target_value(target)?;
            serde_json::from_value(target.clone())
                .map_err(|error| format!("invalid target selector: {error}"))
        })
        .transpose()?;
    if explicit_target
        .as_ref()
        .is_some_and(|target| !action_accepts_target(action) && target_selector_is_present(target))
    {
        return Err(format!(
            "{} does not accept target selectors",
            action.as_str()
        ));
    }
    let target = explicit_target.unwrap_or_else(|| {
        if action_accepts_target(action) {
            default_target.clone()
        } else {
            TargetSelector::default()
        }
    });

    let params = arguments
        .get("params")
        .cloned()
        .unwrap_or_else(|| json!({}));
    validate_action_params(action, &params)?;
    Ok((action, target, params))
}

fn action_accepts_target(action: ActionKind) -> bool {
    use local_control::TargetScope;

    match action.metadata().target_scope {
        TargetScope::Instance => action == ActionKind::AppFocus,
        TargetScope::Window
        | TargetScope::Tab
        | TargetScope::Pane
        | TargetScope::Session
        | TargetScope::Input
        | TargetScope::Surface
        | TargetScope::File => true,
        TargetScope::Settings
        | TargetScope::Appearance
        | TargetScope::Keybinding
        | TargetScope::Action
        | TargetScope::Capability => false,
    }
}

fn target_selector_is_present(target: &TargetSelector) -> bool {
    target.window.is_some()
        || target.tab.is_some()
        || target.pane.is_some()
        || target.session.is_some()
}

fn validate_action_params(action: ActionKind, params: &Value) -> Result<(), String> {
    match action.metadata().parameter_spec {
        ActionParameterSpec::None => decode_params::<EmptyParams>(action, params),
        ActionParameterSpec::ActionName => {
            let params = serde_json::from_value::<ActionNameParams>(params.clone())
                .map_err(|error| invalid_action_params(action, error))?;
            if ActionKind::implemented_metadata()
                .iter()
                .any(|metadata| metadata.name == params.action)
            {
                Ok(())
            } else {
                Err(format!("unknown implemented action {:?}", params.action))
            }
        }
        ActionParameterSpec::BindingName => decode_params::<BindingNameParams>(action, params),
        ActionParameterSpec::BooleanValue => decode_params::<BooleanValueParams>(action, params),
        ActionParameterSpec::ColorValue => decode_params::<ColorValueParams>(action, params),
        ActionParameterSpec::Direction => decode_params::<DirectionParams>(action, params),
        ActionParameterSpec::FileOpen => decode_params::<FileOpenParams>(action, params),
        ActionParameterSpec::Key => decode_params::<KeyParams>(action, params),
        ActionParameterSpec::KeyValue => decode_params::<KeyValueParams>(action, params),
        ActionParameterSpec::Namespace => decode_params::<NamespaceParams>(action, params),
        ActionParameterSpec::PageQuery => decode_params::<PageQueryParams>(action, params),
        ActionParameterSpec::Query => decode_params::<QueryParams>(action, params),
        ActionParameterSpec::Rename => decode_params::<RenameParams>(action, params),
        ActionParameterSpec::Resize => decode_params::<ResizeParams>(action, params),
        ActionParameterSpec::TabActivate => decode_params::<TabActivateParams>(action, params),
        ActionParameterSpec::TabClose => decode_params::<TabCloseParams>(action, params),
        ActionParameterSpec::TabCreate => decode_params::<TabCreateParams>(action, params),
        ActionParameterSpec::TerminalExecute => {
            decode_params::<TerminalExecuteParams>(action, params)
        }
        ActionParameterSpec::TerminalInterrupt => {
            decode_params::<TerminalInterruptParams>(action, params)
        }
        ActionParameterSpec::Text => decode_params::<TextParams>(action, params),
        ActionParameterSpec::ThemeName => decode_params::<ThemeNameParams>(action, params),
    }
}

fn decode_params<T: DeserializeOwned>(action: ActionKind, params: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(params.clone())
        .map(|_| ())
        .map_err(|error| invalid_action_params(action, error))
}

fn invalid_action_params(action: ActionKind, error: serde_json::Error) -> String {
    format!("invalid {} parameters: {error}", action.as_str())
}

fn validate_target_value(target: &Value) -> Result<(), String> {
    let object = target
        .as_object()
        .ok_or_else(|| "target must be an object".to_owned())?;
    reject_unknown_keys(object, &["window", "tab", "pane", "session"], "target")?;
    if let Some(window) = object.get("window") {
        validate_tagged_selector(window, "window", &["active", "id", "index", "title"])?;
    }
    if let Some(tab) = object.get("tab") {
        validate_tagged_selector(tab, "tab", &["active", "id", "index", "title"])?;
    }
    if let Some(pane) = object.get("pane") {
        validate_tagged_selector(pane, "pane", &["active", "id", "index"])?;
    }
    if let Some(session) = object.get("session") {
        validate_tagged_selector(session, "session", &["active", "id"])?;
    }
    Ok(())
}

fn validate_tagged_selector(
    selector: &Value,
    family: &str,
    allowed_types: &[&str],
) -> Result<(), String> {
    let object = selector
        .as_object()
        .ok_or_else(|| format!("{family} selector must be an object"))?;
    let selector_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{family} selector type must be a string"))?;
    if !allowed_types.contains(&selector_type) {
        return Err(format!(
            "unsupported {family} selector type {selector_type:?}"
        ));
    }

    let value_key = match selector_type {
        "active" => None,
        "id" => Some("id"),
        "index" => Some("index"),
        "title" => Some("title"),
        _ => unreachable!("validated selector type"),
    };
    let mut allowed_keys = vec!["type"];
    if let Some(value_key) = value_key {
        allowed_keys.push(value_key);
    }
    reject_unknown_keys(object, &allowed_keys, &format!("{family} selector"))?;

    match value_key {
        None => Ok(()),
        Some("index") => object
            .get("index")
            .and_then(Value::as_u64)
            .filter(|index| u32::try_from(*index).is_ok())
            .map(|_| ())
            .ok_or_else(|| format!("{family} selector index must be a 32-bit unsigned integer")),
        Some(key) => object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|_| ())
            .ok_or_else(|| format!("{family} selector {key} must be a non-empty string")),
    }
}

fn reject_unknown_keys(
    object: &Map<String, Value>,
    allowed: &[&str],
    description: &str,
) -> Result<(), String> {
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("{description} contains unknown field {key:?}"));
    }
    Ok(())
}

fn terminal_interrupt_at(
    invoker: &dyn ActionInvoker,
    target: TargetSelector,
    expected_block_id: &str,
    target_running_for_ms: u64,
    mut sleep: impl FnMut(Duration),
) -> Value {
    let pending_deadline = Instant::now() + INTERRUPT_AT_WAIT_GRACE;
    let mut running_deadline = None;

    loop {
        let status_data =
            match invoker.invoke(ActionKind::TerminalStatus, target.clone(), json!({})) {
                Ok(data) => data,
                Err(error) => return control_tool_error(error),
            };
        let status = match serde_json::from_value::<TerminalStatusResult>(status_data) {
            Ok(status) if status.action == ActionKind::TerminalStatus => status,
            Ok(status) => {
                return interrupt_at_protocol_error(format!(
                    "expected terminal.status, received {}",
                    status.action.as_str()
                ));
            }
            Err(error) => {
                return interrupt_at_protocol_error(format!(
                    "failed to decode terminal.status response: {error}"
                ));
            }
        };

        if status.active_block_id != expected_block_id {
            return interrupt_at_not_needed(
                "block_changed",
                expected_block_id,
                &status,
                target_running_for_ms,
            );
        }

        let is_running = status.is_executing || status.is_command_pending || status.is_long_running;
        if status.is_idle || !is_running {
            return interrupt_at_not_needed(
                "block_stopped",
                expected_block_id,
                &status,
                target_running_for_ms,
            );
        }

        let Some(running_for_ms) = status.running_for_ms else {
            if Instant::now() >= pending_deadline {
                return interrupt_at_wait_error(
                    expected_block_id,
                    target_running_for_ms,
                    "the running block did not expose elapsed time before the wait limit",
                );
            }
            sleep(INTERRUPT_AT_POLL_INTERVAL);
            continue;
        };

        if running_for_ms >= target_running_for_ms {
            return match invoker.invoke(
                ActionKind::TerminalInterrupt,
                target,
                json!({ "block_id": expected_block_id }),
            ) {
                Ok(interrupt) => successful_tool_result(json!({
                    "tool": TERMINAL_INTERRUPT_AT_TOOL,
                    "ok": true,
                    "interrupted": true,
                    "block_id": expected_block_id,
                    "target_running_for_ms": target_running_for_ms,
                    "observed_running_for_ms": running_for_ms,
                    "interrupt": interrupt,
                })),
                Err(error) => control_tool_error(error),
            };
        }

        let remaining_ms = target_running_for_ms - running_for_ms;
        let deadline = *running_deadline.get_or_insert_with(|| {
            Instant::now()
                + Duration::from_millis(remaining_ms).saturating_add(INTERRUPT_AT_WAIT_GRACE)
        });
        if Instant::now() >= deadline {
            return interrupt_at_wait_error(
                expected_block_id,
                target_running_for_ms,
                "the block did not reach the requested elapsed time before the wait limit",
            );
        }
        sleep(Duration::from_millis(
            remaining_ms.min(INTERRUPT_AT_POLL_INTERVAL_MS),
        ));
    }
}

fn interrupt_at_not_needed(
    reason: &str,
    expected_block_id: &str,
    status: &TerminalStatusResult,
    target_running_for_ms: u64,
) -> Value {
    successful_tool_result(json!({
        "tool": TERMINAL_INTERRUPT_AT_TOOL,
        "ok": true,
        "interrupted": false,
        "reason": reason,
        "block_id": expected_block_id,
        "active_block_id": status.active_block_id,
        "target_running_for_ms": target_running_for_ms,
        "observed_running_for_ms": status.running_for_ms,
    }))
}

fn interrupt_at_protocol_error(details: String) -> Value {
    tool_error(
        "Galaxy returned an invalid terminal status response".to_owned(),
        json!({
            "error": {
                "code": "invalid_terminal_status",
                "message": "Galaxy returned an invalid terminal status response",
                "details": details,
            },
        }),
    )
}

fn interrupt_at_wait_error(block_id: &str, target_running_for_ms: u64, message: &str) -> Value {
    tool_error(
        message.to_owned(),
        json!({
            "error": {
                "code": "terminal_deadline_not_observed",
                "message": message,
                "block_id": block_id,
                "target_running_for_ms": target_running_for_ms,
            },
        }),
    )
}

fn successful_tool_result(data: Value) -> Value {
    let text = serde_json::to_string_pretty(&data).unwrap_or_else(|_| data.to_string());
    let structured = ensure_structured_object(data);
    json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "structuredContent": structured,
        "isError": false,
    })
}

fn control_tool_error(error: ControlError) -> Value {
    let message = format!("{}: {}", error.code, error.message);
    let structured = json!({
        "error": serde_json::to_value(error).unwrap_or_else(|serialization_error| {
            json!({
                "code": "internal",
                "message": "failed to serialize local-control error",
                "details": serialization_error.to_string(),
            })
        }),
    });
    tool_error(message, structured)
}

fn tool_argument_error(message: impl Into<String>) -> Value {
    let message = message.into();
    tool_error(
        message.clone(),
        json!({
            "error": {
                "code": "invalid_arguments",
                "message": message,
            },
        }),
    )
}

fn tool_error(message: String, structured: Value) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": message,
        }],
        "structuredContent": structured,
        "isError": true,
    })
}

fn ensure_structured_object(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({ "result": value })
    }
}

fn capabilities_tool_definition() -> Value {
    json!({
        "name": CAPABILITIES_TOOL,
        "title": "Discover Galaxy Control capabilities",
        "description": "Return the exact allowlisted actions advertised by the selected running Galaxy app. Call this before galaxy_control_invoke.",
        "inputSchema": empty_object_schema(),
        "outputSchema": capabilities_output_schema(),
        "annotations": {
            "title": "Discover Galaxy Control capabilities",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

fn invoke_tool_definition() -> Value {
    json!({
        "name": INVOKE_TOOL,
        "title": "Invoke a Galaxy Control action",
        "description": "Invoke one existing allowlisted Galaxy Control action on the app instance selected when this MCP server started. This cannot override the selected app instance. Use opaque target IDs returned by Galaxy metadata and call galaxy_control_capabilities first.",
        "inputSchema": invoke_input_schema(),
        "outputSchema": {
            "type": "object",
            "additionalProperties": true,
        },
        "annotations": {
            "title": "Invoke a Galaxy Control action",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

fn terminal_status_tool_definition() -> Value {
    json!({
        "name": TERMINAL_STATUS_TOOL,
        "title": "Inspect the delegated Galaxy terminal",
        "description": "Return the active block ID, running/idle state, elapsed running time, and secret-obfuscated command summary for the exact Galaxy terminal pane delegated to this agent. Call it immediately before execute or interrupt decisions. For a future elapsed-time deadline, use galaxy_terminal_interrupt_at instead of model-driven polling.",
        "inputSchema": empty_object_schema(),
        "outputSchema": {
            "type": "object",
            "additionalProperties": true,
        },
        "annotations": {
            "title": "Inspect Galaxy terminal",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    })
}

fn terminal_execute_tool_definition() -> Value {
    json!({
        "name": TERMINAL_EXECUTE_TOOL,
        "title": "Execute in the delegated Galaxy terminal",
        "description": "Submit one command to the exact delegated local Galaxy terminal pane. The call fails safely unless that terminal is idle and rejects recognized SSH-session launches; this is not a general network sandbox. Inspect the terminal first with galaxy_terminal_status.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 65536,
                    "description": "Command text to submit followed by Enter.",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": true,
        },
        "annotations": {
            "title": "Execute Galaxy terminal command",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

fn terminal_interrupt_tool_definition() -> Value {
    json!({
        "name": TERMINAL_INTERRUPT_TOOL,
        "title": "Interrupt the delegated Galaxy terminal",
        "description": "Send Ctrl-C only if the exact delegated terminal is still running the supplied active block ID. Obtain the ID immediately beforehand with galaxy_terminal_status; stale IDs are rejected.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "block_id": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_AGENT_BLOCK_ID_BYTES,
                    "description": "Exact active_block_id returned by galaxy_terminal_status.",
                },
            },
            "required": ["block_id"],
            "additionalProperties": false,
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": true,
        },
        "annotations": {
            "title": "Interrupt Galaxy terminal command",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

fn terminal_interrupt_at_tool_definition() -> Value {
    json!({
        "name": TERMINAL_INTERRUPT_AT_TOOL,
        "title": "Interrupt the delegated Galaxy terminal at an elapsed-time deadline",
        "description": "Monitor the exact delegated terminal block inside Galaxy's MCP subprocess and send Ctrl-C once its running_for_ms reaches the requested target. This call polls host state without additional model turns, returns without interrupting if that block stops or is replaced, and performs a final exact-block compare-and-swap before Ctrl-C.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "block_id": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_AGENT_BLOCK_ID_BYTES,
                    "description": "Exact active_block_id returned by galaxy_terminal_status.",
                },
                "target_running_for_ms": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_INTERRUPT_AT_RUNNING_FOR_MS,
                    "description": "Interrupt once this exact block's running_for_ms reaches this elapsed-time target.",
                },
            },
            "required": ["block_id", "target_running_for_ms"],
            "additionalProperties": false,
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": true,
        },
        "annotations": {
            "title": "Interrupt Galaxy terminal command at deadline",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false,
        },
    })
}

fn empty_object_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false,
        "maxProperties": 0,
    })
}

fn capabilities_output_schema() -> Value {
    let action_names = implemented_action_names();
    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "const": ActionKind::CapabilityList.as_str(),
            },
            "capabilities": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "kind": {
                            "type": "string",
                            "enum": action_names,
                        },
                        "name": { "type": "string" },
                        "implementation_status": {
                            "type": "string",
                            "const": "implemented",
                        },
                        "target_scope": { "type": "string" },
                        "parameter_spec": { "type": "string" },
                        "result_spec": { "type": "string" },
                    },
                    "required": [
                        "kind",
                        "name",
                        "implementation_status",
                        "target_scope",
                        "parameter_spec",
                        "result_spec",
                    ],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["action", "capabilities"],
        "additionalProperties": false,
    })
}

fn invoke_input_schema() -> Value {
    let metadata = ActionKind::implemented_metadata();
    let action_names = metadata
        .iter()
        .map(|metadata| Value::String(metadata.name.clone()))
        .collect::<Vec<_>>();
    let action_conditions = metadata
        .into_iter()
        .map(|metadata| {
            let mut then = json!({
                "properties": {
                    "params": parameter_schema(metadata.parameter_spec),
                },
            });
            if parameter_spec_requires_params(metadata.parameter_spec) {
                then.as_object_mut()
                    .expect("then schema object")
                    .insert("required".to_owned(), json!(["params"]));
            }
            json!({
                "if": {
                    "properties": {
                        "action": {
                            "const": metadata.name,
                        },
                    },
                    "required": ["action"],
                },
                "then": then,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "description": "Canonical allowlisted Galaxy Control action name.",
                "enum": action_names,
            },
            "target": target_schema(),
            "params": {
                "type": "object",
                "description": "Parameters whose shape is selected by the action.",
            },
        },
        "required": ["action"],
        "additionalProperties": false,
        "allOf": action_conditions,
    })
}

fn implemented_action_names() -> Vec<Value> {
    ActionKind::implemented_metadata()
        .into_iter()
        .map(|metadata| Value::String(metadata.name))
        .collect()
}

fn parameter_spec_requires_params(spec: ActionParameterSpec) -> bool {
    match spec {
        ActionParameterSpec::None
        | ActionParameterSpec::Namespace
        | ActionParameterSpec::PageQuery
        | ActionParameterSpec::Query
        | ActionParameterSpec::TabCreate => false,
        ActionParameterSpec::ActionName
        | ActionParameterSpec::BindingName
        | ActionParameterSpec::BooleanValue
        | ActionParameterSpec::ColorValue
        | ActionParameterSpec::Direction
        | ActionParameterSpec::FileOpen
        | ActionParameterSpec::Key
        | ActionParameterSpec::KeyValue
        | ActionParameterSpec::Rename
        | ActionParameterSpec::Resize
        | ActionParameterSpec::TabActivate
        | ActionParameterSpec::TabClose
        | ActionParameterSpec::TerminalExecute
        | ActionParameterSpec::TerminalInterrupt
        | ActionParameterSpec::Text
        | ActionParameterSpec::ThemeName => true,
    }
}

fn parameter_schema(spec: ActionParameterSpec) -> Value {
    match spec {
        ActionParameterSpec::None => empty_object_schema(),
        ActionParameterSpec::ActionName => object_schema(
            json!({
                "action": {
                    "type": "string",
                    "enum": implemented_action_names(),
                },
            }),
            &["action"],
        ),
        ActionParameterSpec::BindingName => object_schema(
            json!({ "binding_name": { "type": "string", "minLength": 1 } }),
            &["binding_name"],
        ),
        ActionParameterSpec::BooleanValue => {
            object_schema(json!({ "value": { "type": "boolean" } }), &["value"])
        }
        ActionParameterSpec::ColorValue => object_schema(
            json!({ "color": { "type": "string", "minLength": 1 } }),
            &["color"],
        ),
        ActionParameterSpec::Direction => object_schema(
            json!({
                "direction": {
                    "type": "string",
                    "enum": ["left", "right", "up", "down", "previous", "next"],
                },
            }),
            &["direction"],
        ),
        ActionParameterSpec::FileOpen => object_schema(
            json!({
                "path": { "type": "string", "minLength": 1 },
                "line": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                "column": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                "new_tab": { "type": "boolean", "default": false },
            }),
            &["path"],
        ),
        ActionParameterSpec::Key => object_schema(
            json!({ "key": { "type": "string", "minLength": 1 } }),
            &["key"],
        ),
        ActionParameterSpec::KeyValue => object_schema(
            json!({
                "key": { "type": "string", "minLength": 1 },
                "value": {},
            }),
            &["key", "value"],
        ),
        ActionParameterSpec::Namespace => object_schema(
            json!({ "namespace": { "type": "string", "minLength": 1 } }),
            &[],
        ),
        ActionParameterSpec::PageQuery => object_schema(
            json!({
                "page": { "type": "string" },
                "query": { "type": "string" },
            }),
            &[],
        ),
        ActionParameterSpec::Query => object_schema(json!({ "query": { "type": "string" } }), &[]),
        ActionParameterSpec::Rename => {
            object_schema(json!({ "title": { "type": "string" } }), &["title"])
        }
        ActionParameterSpec::Resize => object_schema(
            json!({
                "direction": {
                    "type": "string",
                    "enum": ["left", "right", "up", "down", "previous", "next"],
                },
                "amount": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
            }),
            &["direction"],
        ),
        ActionParameterSpec::TabActivate => object_schema(
            json!({
                "mode": {
                    "type": "string",
                    "enum": ["target", "previous", "next", "last"],
                },
            }),
            &["mode"],
        ),
        ActionParameterSpec::TabClose => object_schema(
            json!({
                "mode": {
                    "type": "string",
                    "enum": ["target", "active", "others", "right_of"],
                },
            }),
            &["mode"],
        ),
        ActionParameterSpec::TabCreate => object_schema(
            json!({
                "tab_type": {
                    "type": "string",
                    "enum": ["terminal", "agent", "default"],
                },
                "shell": { "type": "string", "minLength": 1 },
            }),
            &[],
        ),
        ActionParameterSpec::TerminalExecute => object_schema(
            json!({
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 65536,
                    "description": "Command to submit only when the target terminal is idle.",
                },
            }),
            &["command"],
        ),
        ActionParameterSpec::TerminalInterrupt => object_schema(
            json!({
                "block_id": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 4096,
                    "description": "Exact active block ID returned by terminal.status.",
                },
            }),
            &["block_id"],
        ),
        ActionParameterSpec::Text => {
            object_schema(json!({ "text": { "type": "string" } }), &["text"])
        }
        ActionParameterSpec::ThemeName => object_schema(
            json!({ "theme_name": { "type": "string", "minLength": 1 } }),
            &["theme_name"],
        ),
    }
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn target_schema() -> Value {
    json!({
        "type": "object",
        "description": "Optional target within the pinned Galaxy app. Omit to use the selectors supplied when galaxyctrl mcp started, or Galaxy's active/single target resolution.",
        "properties": {
            "window": tagged_selector_schema(&["active", "id", "index", "title"]),
            "tab": tagged_selector_schema(&["active", "id", "index", "title"]),
            "pane": tagged_selector_schema(&["active", "id", "index"]),
            "session": tagged_selector_schema(&["active", "id"]),
        },
        "additionalProperties": false,
    })
}

fn tagged_selector_schema(types: &[&str]) -> Value {
    let variants = types
        .iter()
        .map(|selector_type| match *selector_type {
            "active" => object_schema(json!({ "type": { "const": "active" } }), &["type"]),
            "id" => object_schema(
                json!({
                    "type": { "const": "id" },
                    "id": { "type": "string", "minLength": 1 },
                }),
                &["type", "id"],
            ),
            "index" => object_schema(
                json!({
                    "type": { "const": "index" },
                    "index": { "type": "integer", "minimum": 0, "maximum": u32::MAX },
                }),
                &["type", "index"],
            ),
            "title" => object_schema(
                json!({
                    "type": { "const": "title" },
                    "title": { "type": "string", "minLength": 1 },
                }),
                &["type", "title"],
            ),
            _ => unreachable!("static selector type"),
        })
        .collect::<Vec<_>>();
    json!({ "oneOf": variants })
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
