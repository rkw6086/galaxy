//! Tool definitions filtered by mode.
//!
//! Each mode has a different set of tools available. Code mode gets everything,
//! Plan mode gets read-only tools, Review mode gets read + search, etc.

use crate::ai::provider::convert::ToolDefinition;
use crate::ai::prompt_builder::mode::Mode;

/// Returns the tool definitions available for the given mode.
pub fn tools_for_mode(mode: &Mode) -> Vec<ToolDefinition> {
    match mode {
        Mode::Code => code_tools(),
        Mode::Plan => plan_tools(),
        Mode::Review => review_tools(),
        Mode::Summarize => vec![],
        Mode::Title => vec![],
    }
}

/// Full tool set for coding mode.
fn code_tools() -> Vec<ToolDefinition> {
    vec![
        run_shell_command(),
        read_files(),
        apply_file_diffs(),
        search_codebase(),
        file_glob(),
        grep(),
        write_to_long_running_shell_command(),
        interrupt_shell_command(),
        read_shell_command_output(),
        read_mcp_resource(),
        read_documents(),
        create_documents(),
        edit_documents(),
        read_notebook(),
        create_notebook(),
        edit_notebook(),
        start_agent(),
        send_message_to_agent(),
        ask_user_question(),
        read_skill(),
        fetch_conversation(),
    ]
}

/// Read-only tools for planning mode.
fn plan_tools() -> Vec<ToolDefinition> {
    vec![
        read_files(),
        search_codebase(),
        file_glob(),
        grep(),
        run_shell_command_readonly(),
        ask_user_question(),
        start_agent(),
        send_message_to_agent(),
        read_skill(),
        fetch_conversation(),
    ]
}

/// Tools for code review mode.
fn review_tools() -> Vec<ToolDefinition> {
    vec![
        read_files(),
        search_codebase(),
        file_glob(),
        grep(),
        run_shell_command_readonly(),
    ]
}

// ─── Tool Definitions ────────────────────────────────────────────────────────

fn run_shell_command() -> ToolDefinition {
    ToolDefinition {
        name: "run_shell_command".to_string(),
        description: "Execute a shell command in the user's terminal and return its output. Use for running builds, tests, git operations, installing packages, or any shell operation. Commands run in the user's actual shell with their environment. Set is_read_only=true for read-only commands (ls, cat, git status) to enable auto-execution. Always use --no-pager for git commands.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "is_read_only": { "type": "boolean", "description": "True if command only reads data and makes no changes" },
                "is_risky": { "type": "boolean", "description": "True if command is destructive or irreversible (rm -rf, git push --force)" }
            },
            "required": ["command"]
        }),
    }
}

fn run_shell_command_readonly() -> ToolDefinition {
    ToolDefinition {
        name: "run_shell_command".to_string(),
        description: "Execute a READ-ONLY shell command and return its output. Only use for commands that inspect state (ls, cat, git log, git status, find, etc.). Do NOT use for commands that modify files or state. Always use --no-pager for git commands.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The read-only shell command to execute" },
                "is_read_only": { "type": "boolean", "description": "Must be true — only read-only commands are allowed in this mode" }
            },
            "required": ["command"]
        }),
    }
}

fn read_files() -> ToolDefinition {
    ToolDefinition {
        name: "read_files".to_string(),
        description: "Read the contents of up to 6 focused files per call. If you need more, make another call. Returns file contents with path headers. Binary files are detected and skipped. Use absolute paths.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "files": { "type": "array", "maxItems": 6, "items": { "type": "string" }, "description": "Absolute file paths to read (maximum 6 per call)" }
            },
            "required": ["files"]
        }),
    }
}

fn apply_file_diffs() -> ToolDefinition {
    ToolDefinition {
        name: "apply_file_diffs".to_string(),
        description: "Apply search/replace edits to exactly one file per call. For multiple files, make separate calls. Creates files if they don't exist (use empty search string). The search string must uniquely match one location in the file. Include enough surrounding context for uniqueness. For new files, use search=\"\" and put full content in replace.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "description": "A brief summary of what these edits accomplish (e.g. 'Add error handling to parse_config')" },
                "diffs": { "type": "array", "items": { "type": "object", "properties": { "file_path": { "type": "string", "description": "Absolute path to the file" }, "search": { "type": "string", "description": "Exact text to find (must match uniquely). Empty string to create a new file." }, "replace": { "type": "string", "description": "Text to replace with" } }, "required": ["file_path", "search", "replace"] }, "description": "Array of file edits to apply" }
            },
            "required": ["summary", "diffs"]
        }),
    }
}

fn grep() -> ToolDefinition {
    ToolDefinition {
        name: "grep".to_string(),
        description: "Search for up to 3 focused regex patterns per call. Use only for exact known text in non-code files; never use this to discover or search source code when search_codebase is available. Uses git grep in git repos (respects .gitignore) or ripgrep otherwise. Returns up to 1,000 matching line locations and reports when additional matches were omitted. Use read_files afterward to see context around matches.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "queries": { "type": "array", "maxItems": 3, "items": { "type": "string" }, "description": "Regex patterns to search for (maximum 3 per call)" },
                "path": { "type": "string", "description": "Directory to scope the search to" }
            },
            "required": ["queries"]
        }),
    }
}

fn file_glob() -> ToolDefinition {
    ToolDefinition {
        name: "file_glob".to_string(),
        description: "Find filenames matching up to 3 focused glob patterns per call. Use this to locate files, not to search their contents. Uses git ls-files in git repos. Returns absolute file paths of matches. Make another call for more patterns.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "patterns": { "type": "array", "maxItems": 3, "items": { "type": "string" }, "description": "Glob patterns to match files (maximum 3 per call)" },
                "path": { "type": "string", "description": "Directory to search from" }
            },
            "required": ["patterns"]
        }),
    }
}

fn search_codebase() -> ToolDefinition {
    ToolDefinition {
        name: "search_codebase".to_string(),
        description: "Primary tool for discovering relevant source code in the local project index. Use this first for all source-code discovery and conceptual, semantic, or structural searches before reading files. Never use grep to search source code when this tool is available.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Natural language search query describing what you're looking for" },
                "path": { "type": "string", "description": "Optional directory path to narrow search scope" }
            },
            "required": ["query"]
        }),
    }
}

fn write_to_long_running_shell_command() -> ToolDefinition {
    ToolDefinition {
        name: "write_to_long_running_shell_command".to_string(),
        description: "Send input (stdin) to a currently running shell command. Use this to interact with commands that are waiting for input, like interactive prompts or REPLs. Do not use printable escape spellings to interrupt a command; use interrupt_shell_command.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command_id": { "type": "string", "description": "Command ID returned by a long-running command result" },
                "input": { "type": "string", "description": "Text to send as stdin to the running command" },
                "mode": { "type": "string", "enum": ["raw", "line", "block"], "default": "raw" }
            },
            "required": ["command_id", "input"]
        }),
    }
}

fn interrupt_shell_command() -> ToolDefinition {
    ToolDefinition {
        name: "interrupt_shell_command".to_string(),
        description: "Interrupt a currently running command with a real terminal Ctrl+C. Use only when the user asks to stop/cancel/interrupt, or when a user-specified stop condition is met.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command_id": { "type": "string", "description": "Command ID returned by a long-running command result" }
            },
            "required": ["command_id"]
        }),
    }
}

fn read_shell_command_output() -> ToolDefinition {
    ToolDefinition {
        name: "read_shell_command_output".to_string(),
        description: "Read the latest output from a previously started long-running shell command. Poll for no more than 10 seconds at a time so steering and stop conditions remain responsive.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command_id": { "type": "string", "description": "Command ID returned by a long-running command result" },
                "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 10, "default": 2 }
            },
            "required": ["command_id"]
        }),
    }
}

fn read_mcp_resource() -> ToolDefinition {
    ToolDefinition {
        name: "read_mcp_resource".to_string(),
        description: "Read a resource from a connected MCP (Model Context Protocol) server. Resources provide context like database schemas, API docs, or live system state.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "server_id": { "type": "string", "description": "MCP server identifier" },
                "uri": { "type": "string", "description": "Resource URI to read" }
            },
            "required": ["server_id", "uri"]
        }),
    }
}

fn read_documents() -> ToolDefinition {
    ToolDefinition {
        name: "read_plan".to_string(),
        description: "Read the contents of up to 6 Galaxy plan documents per call. Make another call for more. Plans are rich-text documents that appear in the Plans folder of Galaxy Drive."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "document_ids": { "type": "array", "maxItems": 6, "items": { "type": "string" }, "description": "Plan document IDs to read (maximum 6 per call)" }
            },
            "required": ["document_ids"]
        }),
    }
}

fn create_documents() -> ToolDefinition {
    ToolDefinition {
        name: "create_plan".to_string(),
        description: "Create exactly one plan document per call. Use separate calls for separate documents. Plans are rich-text documents for tracking tasks, architecture decisions, and project notes."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "documents": { "type": "array", "maxItems": 1, "items": { "type": "object", "properties": { "title": { "type": "string" }, "content": { "type": "string" } }, "required": ["title", "content"] }, "description": "One plan document to create" }
            },
            "required": ["documents"]
        }),
    }
}

fn edit_documents() -> ToolDefinition {
    ToolDefinition {
        name: "edit_plan".to_string(),
        description: "Edit exactly one plan document per call with incremental search/replace diffs. Use separate calls for separate documents."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "diffs": { "type": "array", "maxItems": 1, "items": { "type": "object", "properties": { "document_id": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }, "required": ["document_id", "search", "replace"] }, "description": "One incremental edit to apply to a plan document" }
            },
            "required": ["diffs"]
        }),
    }
}

fn start_agent() -> ToolDefinition {
    ToolDefinition {
        name: "start_agent".to_string(),
        description: "Start a sub-agent to handle a specific task autonomously. Use for delegating independent work that can run in parallel. The agent gets its own conversation context and tool access.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Name for the sub-agent (used for identification)" },
                "prompt": { "type": "string", "description": "The task/instructions for the sub-agent to execute" }
            },
            "required": ["name", "prompt"]
        }),
    }
}

fn send_message_to_agent() -> ToolDefinition {
    ToolDefinition {
        name: "send_message_to_agent".to_string(),
        description: "Send a message to a running sub-agent. Use to provide additional context, ask for updates, or redirect the agent's work.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "agent_id": { "type": "string", "description": "ID of the target sub-agent" },
                "message": { "type": "string", "description": "Message to send to the agent" }
            },
            "required": ["agent_id", "message"]
        }),
    }
}

fn ask_user_question() -> ToolDefinition {
    ToolDefinition {
        name: "ask_user_question".to_string(),
        description: "Ask the user a question when you need clarification or a decision. Present clear options when possible. Use sparingly \u{2014} prefer making reasonable assumptions.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question to ask the user" },
                "options": { "type": "array", "items": { "type": "string" }, "description": "Optional multiple-choice options to present" }
            },
            "required": ["question"]
        }),
    }
}

fn read_skill() -> ToolDefinition {
    ToolDefinition {
        name: "read_skill".to_string(),
        description: "Read a locally available skill definition. Use the exact skill reference and reference type advertised in the Available Skills system-prompt section.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "skill": { "type": "string", "description": "Exact skill path or bundled skill ID from Available Skills" },
                "reference_type": {
                    "type": "string",
                    "enum": ["path", "bundled"],
                    "description": "The exact reference type shown for this skill in Available Skills"
                }
            },
            "required": ["skill", "reference_type"]
        }),
    }
}

fn fetch_conversation() -> ToolDefinition {
    ToolDefinition {
        name: "fetch_conversation".to_string(),
        description: "Fetch the contents of a previous conversation for context. Use when the user references prior work or you need history from another session.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "conversation_id": { "type": "string", "description": "ID of the conversation to fetch" }
            },
            "required": ["conversation_id"]
        }),
    }
}

fn read_notebook() -> ToolDefinition {
    ToolDefinition {
        name: "read_notebook".to_string(),
        description: "Read the contents of up to 6 Galaxy Drive notebooks per call. Make another call for more. Notebooks are user-created rich-text documents stored in Galaxy Drive.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "document_ids": { "type": "array", "maxItems": 6, "items": { "type": "string" }, "description": "Notebook IDs to read (maximum 6 per call)" }
            },
            "required": ["document_ids"]
        }),
    }
}

fn create_notebook() -> ToolDefinition {
    ToolDefinition {
        name: "create_notebook".to_string(),
        description: "Create exactly one notebook per call. Use separate calls for separate notebooks. Notebooks are rich-text documents for general notes, documentation, and reference material.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "documents": { "type": "array", "maxItems": 1, "items": { "type": "object", "properties": { "title": { "type": "string" }, "content": { "type": "string" } }, "required": ["title", "content"] }, "description": "One notebook to create" }
            },
            "required": ["documents"]
        }),
    }
}

fn edit_notebook() -> ToolDefinition {
    ToolDefinition {
        name: "edit_notebook".to_string(),
        description: "Edit exactly one Galaxy Drive notebook per call with incremental search/replace diffs. Use separate calls for separate notebooks."
            .to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "diffs": { "type": "array", "maxItems": 1, "items": { "type": "object", "properties": { "document_id": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }, "required": ["document_id", "search", "replace"] }, "description": "One incremental edit to apply to a notebook" }
            },
            "required": ["diffs"]
        }),
    }
}
