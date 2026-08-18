use std::io::{self, BufRead as _, Write as _};

use anyhow::Context as _;

pub const SERVER_ARGUMENT: &str = "--rig-test-mcp-server";
pub const TOOL_RESULT_PREFIX: &str = "rig-mcp-echo: ";

pub fn run() -> anyhow::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    for line in stdin.lock().lines() {
        let line = line.context("failed to read MCP request")?;
        let request: serde_json::Value =
            serde_json::from_str(&line).context("failed to parse MCP request")?;
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let result = match method {
            "initialize" => {
                let protocol_version = request
                    .pointer("/params/protocolVersion")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!("2024-11-05"));
                serde_json::json!({
                    "protocolVersion": protocol_version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "rig-integration-mcp",
                        "version": "1.0.0",
                    },
                })
            }
            "tools/list" => serde_json::json!({
                "tools": [{
                    "name": "echo",
                    "description": "Echo text through the hermetic Rig integration MCP server",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"],
                    },
                }],
            }),
            "tools/call" => {
                let text = request
                    .pointer("/params/arguments/text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": format!("{TOOL_RESULT_PREFIX}{text}"),
                    }],
                    "isError": false,
                })
            }
            "ping" => serde_json::json!({}),
            _ => {
                let response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Unsupported test MCP method: {method}"),
                    },
                });
                writeln!(stdout, "{response}").context("failed to write MCP error response")?;
                stdout
                    .flush()
                    .context("failed to flush MCP error response")?;
                continue;
            }
        };
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        writeln!(stdout, "{response}").context("failed to write MCP response")?;
        stdout.flush().context("failed to flush MCP response")?;
    }

    Ok(())
}
