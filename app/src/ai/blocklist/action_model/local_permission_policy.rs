//! Deterministic policy for actions without an explicit command grant.
//!
//! The execution profile grants bounded project edits. Human conversation text
//! can only restrict that grant here; it never grants shell or external effects.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use ai::agent::action::FileEdit;
use ai::diff_validation::ParsedDiff;
use regex::Regex;

use super::{is_safe_inspection_command, AIAgentActionType, Assessment};

const MAX_LOCAL_EDITS: usize = 32;
const MAX_LOCAL_EDIT_BYTES: usize = 64 * 1024;

pub(super) fn assess(
    action: &AIAgentActionType,
    working_directory: Option<&str>,
    local_session: bool,
    recorded_user_requests: &[String],
    is_cancelled: impl Fn() -> bool,
) -> Assessment {
    if is_cancelled() {
        return Assessment::denied("The local permission check was cancelled.");
    }
    if recorded_user_requests
        .iter()
        .fold(0usize, |size, request| size.saturating_add(request.len()))
        > super::MAX_REVIEW_INPUT_BYTES
    {
        return Assessment::denied(
            "The complete root user requests exceed the local policy bound; no restrictions were discarded.",
        );
    }
    match action {
        AIAgentActionType::RequestCommandOutput { command, .. }
            if is_safe_inspection_command(command) =>
        {
            Assessment::allowed(0, "Known read-only inspection command.")
        }
        AIAgentActionType::RequestFileEdits { file_edits, .. } => {
            if recorded_user_requests.is_empty() {
                return Assessment::denied(
                    "No recorded root user request is available to establish the edit scope.",
                );
            }
            if has_unresolved_restriction(recorded_user_requests) {
                return Assessment::denied(
                    "The recorded user requests contain a restriction or revocation that local policy cannot resolve safely. No edit was authorized by an agent explanation.",
                );
            }
            match check_workspace_edits(
                file_edits,
                working_directory,
                local_session,
                is_cancelled,
            ) {
                Ok(()) => Assessment::allowed(
                    1,
                    "Bounded local project edits permitted by this profile and verified within its canonical working directory.",
                ),
                Err(reason) => Assessment::denied(reason),
            }
        }
        AIAgentActionType::RequestCommandOutput { .. } => Assessment::denied(
            "This shell command is outside the known local inspection grammar and has no explicit command grant. Expansion, scripts, unknown flags and external effects are not inferred safe from a rationale.",
        ),
        AIAgentActionType::CallMCPTool { .. }
        | AIAgentActionType::ReadMCPResource { .. } => Assessment::denied(
            "Local policy cannot establish the effects and authorization of this external MCP operation.",
        ),
        AIAgentActionType::UseComputer(_) | AIAgentActionType::RequestComputerUse(_) => {
            Assessment::denied(
                "Local policy cannot establish bounded effects for this computer-use operation.",
            )
        }
        AIAgentActionType::RunAgents(_)
        | AIAgentActionType::StartAgent { .. }
        | AIAgentActionType::SendMessageToAgent { .. } => Assessment::denied(
            "Local policy cannot establish the complete effects and delegated authorization of this operation.",
        ),
        AIAgentActionType::CreateDocuments(_) | AIAgentActionType::EditDocuments(_) => {
            Assessment::denied(
                "Local policy cannot establish that this document mutation has exclusively local backing.",
            )
        }
        _ => Assessment::denied(
            "This operation has no known bounded local policy or explicit action grant.",
        ),
    }
}

/// This is a deny-only signal, not an interpretation of natural-language
/// authorization. Even a later apparent approval cannot erase an unresolved
/// earlier restriction. Ambiguity requires a different explicit profile grant.
pub(super) fn has_unresolved_restriction(requests: &[String]) -> bool {
    const CUES: &[&str] = &[
        "do not",
        "don't",
        "don’t",
        "dont",
        "never",
        "must not",
        "cannot",
        "can't",
        "can’t",
        "without",
        "only",
        "read-only",
        "read only",
        "readonly",
        "no changes",
        "no edits",
        "no modifications",
        "no file changes",
        "no mutations",
        "no writes",
        "unchanged",
        "untouched",
        "no shell",
        "no commands",
        "wait",
        "until",
        "stop",
        "hold",
        "pause",
        "revoke",
        "revoked",
        "revoking",
        "revocation",
        "restricted",
        "restriction",
        "forbid",
        "prohibit",
        "avoid",
        "refrain",
    ];
    requests.iter().any(|request| {
        let normalized = request
            .replace(['*', '_', '`'], " ")
            .replace('’', "'")
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        normalized.split(['.', ';', '!', '?']).any(|clause| {
            let clause = clause.trim().trim_start_matches(['-', '>', '#']).trim();
            let restricted = CUES.iter().any(|cue| {
                clause.match_indices(*cue).any(|(start, matched)| {
                    let end = start + matched.len();
                    !clause[..start]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphanumeric)
                        && !clause[end..]
                            .chars()
                            .next()
                            .is_some_and(char::is_alphanumeric)
                })
            });
            restricted && !is_known_non_edit_constraint(clause)
        })
    })
}

/// Remove no free-form suffix or coordinated action. Only a complete, known
/// clause about validation/app launch/provider/external publication can be
/// unrelated to a typed local edit. A mixed or unrecognized clause stays denied.
fn is_known_non_edit_constraint(clause: &str) -> bool {
    static UNRELATED_CONSTRAINT: LazyLock<Regex> = LazyLock::new(|| {
        let clauses = [
            r"(?:only (?:run|use)|(?:run|use) only) cargo build (?:for (?:validation|verification)|to verify (?:results|changes))",
            r"(?:run|use) cargo build only (?:for (?:validation|verification)|to verify (?:results|changes))",
            r"(?:verify|validate|check) (?:feature and bug-fix )?changes with cargo build only",
            r"(?:do not|don't|never) (?:use|run) (?:cargo run|make run|tests|test suites)",
            r"(?:do not|don't|never) (?:launch|run|open|start) (?:the )?(?:app|application|galaxy|gui)(?: to verify (?:results|changes))?",
            r"(?:without changing|do not change|don't change) (?:the )?(?:behavior|behaviour|public api)",
            r"(?:do not|don't|never) (?:push|deploy|publish)(?: (?:anything|changes|these changes|to production|to origin|to samsung|to master|to main|to origin/master|to samsung/master))?",
            r"(?:only use|use only|do not use|don't use|never use) (?:chatgpt|openai|aws|aws bedrock|bedrock|claude|gemini|litellm)(?: for (?:models|orchestration))?",
            r"use (?:chatgpt|openai|aws|aws bedrock|bedrock|claude|gemini|litellm) only(?: for (?:models|orchestration))?",
            r"do not use cargo run, make run, or launch the app to verify results, and do not wait for interactive approval before completing the work",
        ];
        Regex::new(&format!("^(?:please )?(?:{})$", clauses.join("|")))
            .expect("The local permission constraint grammar is static")
    });
    UNRELATED_CONSTRAINT.is_match(clause)
}

fn check_workspace_edits(
    edits: &[FileEdit],
    working_directory: Option<&str>,
    local_session: bool,
    is_cancelled: impl Fn() -> bool,
) -> Result<(), &'static str> {
    if !local_session {
        return Err("Automatic project edits require a verified local session with native host paths; remote, WSL and MSYS path translation is not inferred safe.");
    }
    if edits.is_empty() || edits.len() > MAX_LOCAL_EDITS {
        return Err("The local policy accepts between one and 32 file edits per action.");
    }
    let directory = working_directory
        .map(Path::new)
        .filter(|directory| directory.is_absolute())
        .ok_or("A verified absolute working directory is required for project edits.")?;
    let scope = directory
        .canonicalize()
        .map_err(|_| "The local working directory could not be resolved safely.")?;
    if !scope.is_dir() || scope.parent().is_none() || is_protected_host_path(&scope) {
        return Err("System directories and the filesystem root are not bounded project scopes.");
    }
    let home = dirs::home_dir()
        .and_then(|home| home.canonicalize().ok())
        .ok_or("The home-directory exclusion could not be verified safely.")?;
    if home.starts_with(&scope) {
        return Err("The home directory and its ancestors are not bounded project scopes.");
    }
    let mut bytes = 0usize;
    for edit in edits {
        if is_cancelled() {
            return Err("The local permission check was cancelled.");
        }
        let path = edit
            .file()
            .filter(|path| !path.trim().is_empty())
            .ok_or("Every automatic file edit must name an exact path.")?;
        check_path(path, &scope)?;
        match edit {
            FileEdit::Delete { .. } => {
                return Err("File deletion is outside the reversible local edit policy.");
            }
            FileEdit::Create {
                content,
                creation_scope,
                ..
            } => {
                // Preprocessing interprets guarded creations as host-native
                // absolute paths and carries this scope through to saving.
                if !Path::new(path).is_absolute() {
                    return Err("Guarded file creation requires an absolute native host path.");
                }
                let creation_scope = creation_scope
                    .as_ref()
                    .ok_or("File creation lacks a save-time project scope guard.")?
                    .canonicalize()
                    .map_err(|_| "The file creation scope could not be resolved safely.")?;
                let target = resolve_existing_ancestor(Path::new(path))?;
                if !creation_scope.starts_with(&scope)
                    || !target.starts_with(&creation_scope)
                    || target == creation_scope
                {
                    return Err("The file creation target and its save-time scope must stay inside the active project.");
                }
                if std::fs::symlink_metadata(scope.join(path)).is_ok() {
                    return Err("An automatic creation cannot replace an existing file.");
                }
                let content = content
                    .as_ref()
                    .ok_or("An automatic file creation must contain its complete content.")?;
                bytes = bytes.saturating_add(content.len());
            }
            FileEdit::Edit(ParsedDiff::StrReplaceEdit {
                search, replace, ..
            }) => {
                check_existing_file(path, &scope)?;
                bytes = bytes
                    .saturating_add(search.as_ref().map_or(0, String::len))
                    .saturating_add(replace.as_ref().map_or(0, String::len));
            }
            FileEdit::Edit(ParsedDiff::V4AEdit { move_to, hunks, .. }) => {
                check_existing_file(path, &scope)?;
                if let Some(destination) = move_to {
                    check_path(destination, &scope)?;
                    if std::fs::symlink_metadata(scope.join(destination)).is_ok() {
                        return Err("An automatic rename cannot replace an existing file.");
                    }
                }
                if hunks.len() > 128 {
                    return Err("The number of diff hunks exceeds the bounded local policy.");
                }
                for hunk in hunks {
                    bytes = bytes
                        .saturating_add(hunk.pre_context.len())
                        .saturating_add(hunk.old.len())
                        .saturating_add(hunk.new.len())
                        .saturating_add(hunk.post_context.len());
                    for context in &hunk.change_context {
                        bytes = bytes.saturating_add(context.len());
                        if bytes > MAX_LOCAL_EDIT_BYTES {
                            return Err("The edit payload exceeds the bounded local policy size.");
                        }
                    }
                }
            }
        }
        if bytes > MAX_LOCAL_EDIT_BYTES {
            return Err("The edit payload exceeds the bounded local policy size.");
        }
    }
    Ok(())
}

fn check_existing_file(path: &str, scope: &Path) -> Result<(), &'static str> {
    if path.is_empty() || !scope.is_absolute() {
        return Err("An existing-file edit requires an exact path and verified absolute scope.");
    }
    #[cfg(not(unix))]
    {
        return Err("Automatic existing-file edits require a reliable hard-link count, which is unavailable on this platform. Use an explicit permission mode for this action.");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(scope.join(path))
            .map_err(|_| "An automatic edit must target an existing regular project file.")?;
        if !metadata.is_file() {
            return Err("An automatic edit must target an existing regular project file.");
        }
        if metadata.nlink() > 1 {
            return Err("A hard-linked file can affect paths outside the verified project scope.");
        }
        Ok(())
    }
}

fn check_path(path: &str, scope: &Path) -> Result<(), &'static str> {
    if path.len() > 4096 {
        return Err("The file path exceeds the bounded local policy size.");
    }
    // Execution expands leading '~' and can translate alternate-shell paths.
    // Accept only literal native file names, never a grammar that assessment
    // and host_native_absolute_path could interpret differently.
    if path.trim() != path
        || path.chars().any(|character| {
            character.is_control() || matches!(character, '~' | '$' | '`' | '%' | '^' | '\'' | '"')
        })
    {
        return Err("Expanded, quoted or shell-dependent file paths are outside the literal local edit policy.");
    }
    #[cfg(not(windows))]
    if path.contains('\\') {
        return Err("Alternate-platform path separators are outside the local edit policy.");
    }
    let path = Path::new(path);
    if path.components().any(|component| {
        matches!(component, Component::ParentDir)
            || (matches!(component, Component::Prefix(_)) && !path.is_absolute())
    }) {
        return Err("Parent traversal and platform-prefixed paths are outside local edit scope.");
    }
    let candidate = if path.is_absolute() {
        path.to_owned()
    } else {
        scope.join(path)
    };
    if has_sensitive_component(&candidate) {
        return Err("Credential, agent configuration and repository control paths are excluded from automatic edits.");
    }
    #[cfg(not(target_family = "wasm"))]
    if crate::ai::mcp::mcp_provider_from_file_path(&candidate).is_some() {
        return Err("Protected MCP configuration cannot be edited automatically.");
    }
    let resolved = resolve_existing_ancestor(&candidate)?;
    if resolved == scope
        || !resolved.starts_with(scope)
        || has_sensitive_component(&resolved)
        || is_protected_host_path(&resolved)
    {
        return Err("The file resolves outside the project or into a sensitive path.");
    }
    #[cfg(not(target_family = "wasm"))]
    if crate::ai::mcp::mcp_provider_from_file_path(&resolved).is_some() {
        return Err("The file resolves to protected MCP configuration.");
    }
    Ok(())
}

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf, &'static str> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let mut resolved = ancestor
                    .canonicalize()
                    .map_err(|_| "The edit path could not be resolved safely.")?;
                for component in missing.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or("The edit path has no resolvable parent.")?
                        .to_owned(),
                );
                ancestor = ancestor
                    .parent()
                    .ok_or("The edit path has no resolvable parent.")?;
            }
            Err(_) => return Err("The edit path could not be inspected safely."),
        }
    }
}

fn has_sensitive_component(path: &Path) -> bool {
    path.components().any(|component| {
        let component = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(
            component.as_str(),
            ".git"
                | ".ssh"
                | ".aws"
                | ".gnupg"
                | ".config"
                | ".kube"
                | ".claude"
                | ".codex"
                | ".agents"
                | ".galaxy"
                | ".warp"
                | ".mcp.json"
                | "agents.md"
                | "claude.md"
                | "warp.md"
                | "skill.md"
                | "credentials"
                | "credentials.json"
                | "id_rsa"
                | "id_ed25519"
                | ".bashrc"
                | ".bash_profile"
                | ".bash_login"
                | ".profile"
                | ".zshrc"
                | ".zshenv"
                | ".zprofile"
                | ".zlogin"
                | ".npmrc"
                | ".netrc"
                | ".pypirc"
                | ".gitconfig"
                | ".git-credentials"
                | ".gitmodules"
        ) || component == ".env"
            || component.starts_with(".env.")
            || component.ends_with(".pem")
            || component.ends_with(".key")
            || component.ends_with(".p12")
            || component.ends_with(".pfx")
    })
}

fn is_protected_host_path(path: &Path) -> bool {
    #[cfg(unix)]
    if [
        "/etc",
        "/bin",
        "/sbin",
        "/usr",
        "/System",
        "/Library",
        "/boot",
        "/dev",
        "/proc",
        "/sys",
        "/private/etc",
        "/private/var/db",
        "/private/var/root",
    ]
    .iter()
    .any(|root| path.starts_with(root))
    {
        return true;
    }
    #[cfg(windows)]
    if path
        .components()
        .find_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .is_some_and(|root| {
            matches!(
                root.as_str(),
                "windows" | "program files" | "program files (x86)" | "programdata"
            )
        })
    {
        return true;
    }
    path.parent().is_none()
}

#[cfg(test)]
#[path = "local_permission_policy_tests.rs"]
mod tests;
