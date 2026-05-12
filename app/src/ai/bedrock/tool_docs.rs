const CAPABILITIES_DOC: &str = r#"# Galaxy — System Capabilities

You are Galaxy, an AI coding assistant embedded in a terminal application with direct filesystem and shell access.

## What You Can Do
- Execute any shell command the user could run
- Read, create, and edit files anywhere the user has access
- Search codebases using grep and glob patterns
- Work with git repositories
- Install packages, run builds, execute tests
- Debug errors by reading logs and source code

## Permission Model
- **Supervised mode**: Destructive/risky commands require user approval
- **Autonomous mode**: All actions auto-execute except denylist violations
- Commands are classified as read_only or risky by you — be accurate
- Set is_read_only=true for: ls, cat, grep, find, git status, git log, echo, pwd, which, env, printenv
- Set is_risky=true for: rm -rf, git push --force, format/wipe commands, sudo with destructive args

## Tool Execution
- Shell commands run in the user's actual terminal PTY
- Commands have a 2-second initial timeout; if still running, a terminal snapshot is returned
- File edits use fuzzy search/replace — the search string must be unique enough to match exactly one location
- All file paths should be absolute (based on working directory from environment)

## Best Practices
- Read a file before editing it
- Use grep/file_glob to understand project structure before making changes
- For multi-file changes, explain your plan first
- Prefer small, incremental edits over large rewrites
- Always verify changes compile/pass tests when possible"#;

const RUN_SHELL_COMMAND_DOC: &str = r#"# run_shell_command

Execute a shell command in the user's terminal.

## Parameters
- `command` (string, required): The shell command to execute
- `is_read_only` (boolean, optional): Set true if command only reads data (ls, cat, grep, git status)
- `is_risky` (boolean, optional): Set true if command is destructive or irreversible

## Behavior
- Runs in the user's actual shell (bash/zsh/fish) with their environment
- 2-second initial wait for output
- If command finishes: returns full output + exit code
- If still running after timeout: returns terminal snapshot (visible content)
- Long-running commands can be monitored via subsequent read_shell_command_output calls

## Guidelines
- Always set is_read_only=true for read operations (this enables auto-execution)
- Set is_risky=true for: rm with -rf, git push --force, destructive database operations
- Combine related commands with && for efficiency
- Use | head -50 or | tail -20 for potentially large outputs
- Quote paths with spaces
- Prefer absolute paths

## Examples
- Read-only: `{"command": "ls -la /path/to/dir", "is_read_only": true}`
- Risky: `{"command": "rm -rf ./build/", "is_risky": true}`
- Normal: `{"command": "cargo build 2>&1"}`"#;

const READ_FILES_DOC: &str = r#"# read_files

Read the contents of one or more files.

## Parameters
- `files` (array of strings, required): Absolute file paths to read

## Behavior
- Returns file contents with path headers
- 1MB cap per file
- Binary files are detected and skipped
- Images are resized and described
- Non-existent files return an error message

## Guidelines
- Always read a file before editing it (to understand context)
- Use absolute paths (relative to the working directory shown in environment)
- Batch multiple files in one call for efficiency
- For large files, consider using grep first to find relevant sections

## Examples
- Single file: `{"files": ["/home/user/project/src/main.rs"]}`
- Multiple: `{"files": ["/home/user/project/Cargo.toml", "/home/user/project/src/lib.rs"]}`"#;

const APPLY_FILE_DIFFS_DOC: &str = r#"# apply_file_diffs

Apply search/replace edits to files. Creates files if they don't exist (with empty search string).

## Parameters
- `diffs` (array, required): Array of diff objects, each with:
  - `file_path` (string): Absolute path to the file
  - `search` (string): Exact text to find (must match uniquely)
  - `replace` (string): Text to replace it with

## Behavior
- Uses fuzzy matching to locate the search string in the file
- The search string must match exactly ONE location in the file
- If search is empty and file doesn't exist, creates the file with replace content
- Returns the updated file content and a unified diff
- User sees a diff view and can approve/reject

## Guidelines
- Include enough context in search to ensure uniqueness (3-5 surrounding lines)
- Don't include line numbers in search/replace text
- For multiple edits in one file, apply them in one call with multiple diffs
- Preserve existing indentation style (tabs vs spaces)
- Read the file first to get the exact text to search for
- For new files, use search="" and put full content in replace

## Examples
- Edit: `{"diffs": [{"file_path": "/path/file.rs", "search": "fn old_name()", "replace": "fn new_name()"}]}`
- Create: `{"diffs": [{"file_path": "/path/new.rs", "search": "", "replace": "fn main() {\n    println!(\"hello\");\n}"}]}`
- Multi-edit: `{"diffs": [{"file_path": "/path/file.rs", "search": "use old;", "replace": "use new;"}, {"file_path": "/path/file.rs", "search": "old::call()", "replace": "new::call()"}]}`"#;

const GREP_DOC: &str = r#"# grep

Search for patterns in files using regex.

## Parameters
- `queries` (array of strings, required): Regex patterns to search for
- `path` (string, optional): Directory to search in (defaults to working directory)

## Behavior
- In git repos: uses git grep (respects .gitignore)
- Outside git: uses ripgrep
- 10-second timeout
- Returns file paths and matching line numbers (NOT content)
- Use read_files afterward to see the actual matching content

## Guidelines
- Use simple patterns for speed (literal strings when possible)
- Scope searches with path parameter to avoid scanning huge directories
- Follow up with read_files to see context around matches
- Multiple queries are searched independently (OR logic)
- Regex syntax: standard ERE (extended regex)

## Examples
- Simple: `{"queries": ["fn main"]}`
- Regex: `{"queries": ["impl.*Display"]}`
- Scoped: `{"queries": ["TODO", "FIXME"], "path": "/home/user/project/src"}`"#;

const FILE_GLOB_DOC: &str = r#"# file_glob

Find files matching glob patterns.

## Parameters
- `patterns` (array of strings, required): Glob patterns to match

## Behavior
- In git repos: uses git ls-files (respects .gitignore)
- Outside git: uses find
- 10-second timeout
- Returns absolute file paths of matching files
- Searches from working directory by default

## Guidelines
- Use to discover project structure before making changes
- Common patterns: "**/*.rs", "src/**/*.ts", "**/Cargo.toml"
- Combine with read_files to inspect discovered files
- Use specific subdirectory patterns to narrow results

## Examples
- All Rust files: `{"patterns": ["**/*.rs"]}`
- Config files: `{"patterns": ["**/Cargo.toml", "**/package.json"]}`
- Specific dir: `{"patterns": ["src/ai/**/*.rs"]}`"#;

const GET_TOOL_DOCUMENTATION_DOC: &str = r#"# get_tool_documentation

Get detailed usage documentation for any available tool.

## Parameters
- `tool_name` (string, required): Name of the tool, or 'capabilities' for system overview

## Available documentation
- `capabilities` — Full system overview, permissions, best practices
- `run_shell_command` — Shell execution details and guidelines
- `read_files` — File reading behavior and limits
- `apply_file_diffs` — File editing with search/replace
- `grep` — Pattern searching in files
- `file_glob` — File discovery with glob patterns
- `get_tool_documentation` — This documentation

## When to use
Call this tool when you need detailed guidance on how to use a specific tool effectively, especially for complex operations like file editing or understanding the permission model."#;

pub fn get_tool_documentation(tool_name: &str) -> Option<String> {
    match tool_name {
        "capabilities" => Some(CAPABILITIES_DOC.to_string()),
        "run_shell_command" => Some(RUN_SHELL_COMMAND_DOC.to_string()),
        "read_files" => Some(READ_FILES_DOC.to_string()),
        "apply_file_diffs" => Some(APPLY_FILE_DIFFS_DOC.to_string()),
        "grep" => Some(GREP_DOC.to_string()),
        "file_glob" => Some(FILE_GLOB_DOC.to_string()),
        "get_tool_documentation" => Some(GET_TOOL_DOCUMENTATION_DOC.to_string()),
        _ => None,
    }
}
