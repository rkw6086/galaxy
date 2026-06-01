//! Tool definitions filtered by mode.
//!
//! Each mode has a different set of tools available. Code mode gets everything,
//! Plan mode gets read-only tools, Review mode gets read + search, etc.

use crate::ai::bedrock::convert::ToolDefinition;
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
        grep(),
        file_glob(),
        search_codebase(),
        write_to_long_running_shell_command(),
        read_shell_command_output(),
        read_mcp_resource(),
        read_documents(),
        create_documents(),
        edit_documents(),
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
        grep(),
        file_glob(),
        search_codebase(),
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
        grep(),
        file_glob(),
        search_codebase(),
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
        description: "Read the contents of one or more files. Pass ALL file paths you need in a single call for efficiency. Returns file contents with path headers. Binary files are detected and skipped. Use absolute paths.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "files": { "type": "array", "items": { "type": "string" }, "description": "Absolute file paths to read" }
            },
            "required": ["files"]
        }),
    }
}

fn apply_file_diffs() -> ToolDefinition {
    ToolDefinition {
        name: "apply_file_diffs".to_string(),
        description: "Apply search/replace edits to files. Creates files if they don't exist (use empty search string). The search string must uniquely match one location in the file. Include enough surrounding context for uniqueness. For new files, use search=\"\" and put full content in replace.".to_string(),
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
        description: "Search for regex patterns in files. Uses git grep in git repos (respects .gitignore) or ripgrep otherwise. Returns file paths and matching line numbers. Use read_files afterward to see context around matches. Pass ALL patterns you need in one call.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "queries": { "type": "array", "items": { "type": "string" }, "description": "Regex patterns to search for" },
                "path": { "type": "string", "description": "Directory to scope the search to" }
            },
            "required": ["queries"]
        }),
    }
}

fn file_glob() -> ToolDefinition {
    ToolDefinition {
        name: "file_glob".to_string(),
        description: "Find files matching glob patterns. Uses git ls-files in git repos. Returns absolute file paths of matches. Common patterns: '**/*.rs', 'src/**/*.ts', '**/Cargo.toml'. Pass ALL patterns in one call.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to match files" },
                "path": { "type": "string", "description": "Directory to search from" }
            },
            "required": ["patterns"]
        }),
    }
}

fn search_codebase() -> ToolDefinition {
    ToolDefinition {
        name: "search_codebase".to_string(),
        description: "Semantic code search across the indexed codebase. Use for finding relevant code by meaning rather than exact text match. Better than grep for conceptual queries like 'authentication logic' or 'error handling for database connections'.".to_string(),
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
        description: "Send input (stdin) to a currently running shell command. Use this to interact with commands that are waiting for input, like interactive prompts, REPLs, or commands that accept piped input.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "input": { "type": "string", "description": "Text to send as stdin to the running command" }
            },
            "required": ["input"]
        }),
    }
}

fn read_shell_command_output() -> ToolDefinition {
    ToolDefinition {
        name: "read_shell_command_output".to_string(),
        description: "Read the latest output from a previously started long-running shell command. Use to check progress or get results from commands that are still running.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
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
        name: "read_documents".to_string(),
        description: "Read the contents of one or more Galaxy notebook documents by their IDs.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "document_ids": { "type": "array", "items": { "type": "string" }, "description": "Document IDs to read" }
            },
            "required": ["document_ids"]
        }),
    }
}

fn create_documents() -> ToolDefinition {
    ToolDefinition {
        name: "create_documents".to_string(),
        description: "Create new Galaxy notebook documents with the specified title and content.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "documents": { "type": "array", "items": { "type": "object", "properties": { "title": { "type": "string" }, "content": { "type": "string" } }, "required": ["title", "content"] }, "description": "Documents to create" }
            },
            "required": ["documents"]
        }),
    }
}

fn edit_documents() -> ToolDefinition {
    ToolDefinition {
        name: "edit_documents".to_string(),
        description: "Edit existing Galaxy notebook documents using search/replace diffs.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "diffs": { "type": "array", "items": { "type": "object", "properties": { "document_id": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }, "required": ["document_id", "search", "replace"] }, "description": "Edits to apply to documents" }
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
        description: "Read a skill definition to understand available capabilities and how to use them.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "skill": { "type": "string", "description": "Skill identifier to read" }
            },
            "required": ["skill"]
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