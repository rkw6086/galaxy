use std::path::{Component, Path, PathBuf};

use ai::skills::SkillPathOrigin;
use galaxy_agent_core::{ToolCall, ToolDefinition};
use serde::{Deserialize, Serialize};

use crate::ai::blocklist::SessionContext;

/// Scope is captured from the execution session, never from a model-supplied home directory.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct SkillCreationContext {
    project_root: Option<PathBuf>,
    home_directory: Option<PathBuf>,
    local: bool,
}

impl SkillCreationContext {
    pub(crate) fn from_session(session: &SessionContext) -> Self {
        if session.is_remote() {
            // The current remote file transport cannot inspect symlinks or create a file
            // exclusively. Do not validate remote paths against the client's filesystem.
            return Self::default();
        }
        let project_root = session
            .current_working_directory()
            .as_deref()
            .map(|cwd| {
                ai::paths::host_native_absolute_path(
                    cwd,
                    session.shell(),
                    session.current_working_directory(),
                )
            })
            .and_then(|cwd| std::fs::canonicalize(cwd).ok())
            .map(|cwd| {
                let root = cwd
                    .ancestors()
                    .find(|directory| directory.join(".git").exists())
                    .map(Path::to_path_buf);
                root.unwrap_or(cwd)
            });
        Self {
            project_root,
            home_directory: dirs::home_dir().and_then(|home| std::fs::canonicalize(home).ok()),
            local: true,
        }
    }

    /// Lower into an ordinary create-file action so the existing mutation permission,
    /// diff review, protected-path, and persistence mechanisms remain the execution owner.
    pub(crate) fn lower_call(
        &self,
        call: &ToolCall,
        origin: &SkillPathOrigin,
        validate_destination: bool,
    ) -> Result<(ToolCall, PathBuf), String> {
        if !self.local
            || !matches!(
                origin,
                SkillPathOrigin::Local | SkillPathOrigin::RestoredDisplayOnly
            )
        {
            return Err("create_skill requires a local execution session. The remote file transport cannot yet validate skill destinations safely; use the existing remote file-edit tools instead.".to_owned());
        }
        let input = &call.arguments;
        let name = required_string(input, "name")?;
        if name.len() > 63
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !name
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            || !name
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
        {
            return Err("name must contain 1–63 lowercase letters, digits, or hyphens and start and end with a letter or digit".to_owned());
        }
        let description = required_string(input, "description")?.trim();
        if description.chars().count() > 512 {
            return Err("description must contain no more than 512 characters".to_owned());
        }
        let instructions = required_string(input, "instructions")?.trim();
        if instructions.len() > 100_000 {
            return Err("instructions must contain no more than 100,000 bytes".to_owned());
        }
        let scope = required_string(input, "scope")?;
        let project_directory = input
            .get("project_directory")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| "project_directory must be a string".to_owned())
            })
            .transpose()?;
        let root = match scope {
            "project" => self.project_root.as_ref().ok_or_else(|| "The current project directory is unavailable; select a local project before creating a project skill.".to_owned())?,
            "global" => {
                if project_directory.is_some() {
                    return Err("project_directory is only valid for project scope".to_owned());
                }
                self.home_directory.as_ref().ok_or_else(|| "The execution user's home directory is unavailable.".to_owned())?
            }
            _ => return Err("scope must be project or global".to_owned()),
        };
        let directory = match project_directory {
            Some(directory) => {
                let path = Path::new(directory);
                if path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir))
                    || directory.starts_with('~')
                {
                    return Err("project_directory must be an absolute project path or a path relative to the project root, without '..' or '~'".to_owned());
                }
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    root.join(path)
                }
            }
            None => root.clone(),
        };
        let directory = directory.components().collect::<PathBuf>();
        if !directory.starts_with(root) {
            return Err("project_directory must remain inside the current project".to_owned());
        }
        // Historical calls must remain displayable after their skill has been created.
        // Actual proposals always revalidate against the current filesystem before review.
        if validate_destination {
            let canonical = std::fs::canonicalize(&directory).map_err(|error| {
                format!(
                    "Cannot resolve skill scope directory {}: {error}",
                    directory.display()
                )
            })?;
            if canonical != directory {
                return Err("project_directory must not redirect through a symlink".to_owned());
            }
        }
        let path = directory
            .join(".agents")
            .join("skills")
            .join(name)
            .join("SKILL.md");
        let path = if validate_destination {
            validate_new_path(&path, root)?
        } else {
            path
        };
        let content = format!(
            "---\nname: {}\ndescription: {}\n---\n\n{instructions}\n",
            serde_json::to_string(name).map_err(|error| error.to_string())?,
            serde_json::to_string(description).map_err(|error| error.to_string())?,
        );
        let mut lowered = call.clone();
        lowered.name = "apply_file_diffs".to_owned();
        lowered.arguments = serde_json::json!({
            "summary": format!("Create {scope} skill {name}"),
            "new_files": [{ "file_path": path.to_string_lossy(), "content": content }],
        });
        Ok((lowered, root.clone()))
    }
}

fn required_string<'a>(input: &'a serde_json::Value, name: &str) -> Result<&'a str, String> {
    input
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
        .ok_or_else(|| format!("{name} must be a nonempty string without NUL characters"))
}

fn validate_new_path(path: &Path, root: &Path) -> Result<PathBuf, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Err(format!("A skill already exists at {}. Read it and use apply_file_diffs for an explicit update instead of overwriting it.", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect skill destination {}: {error}", path.display())),
    }
    let mut missing = Vec::new();
    let mut parent = path;
    loop {
        match std::fs::symlink_metadata(parent) {
            Ok(_) => {
                let mut resolved = std::fs::canonicalize(parent).map_err(|error| {
                    format!(
                        "Cannot resolve skill destination {}: {error}",
                        parent.display()
                    )
                })?;
                if !resolved.starts_with(root) {
                    return Err(
                        "Skill destination resolves outside the selected scope through a symlink"
                            .to_owned(),
                    );
                }
                if resolved != parent {
                    return Err("Skill destination must not redirect through a symlink".to_owned());
                }
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    parent
                        .file_name()
                        .ok_or_else(|| "Invalid skill destination".to_owned())?
                        .to_owned(),
                );
                parent = parent
                    .parent()
                    .ok_or_else(|| "Invalid skill destination".to_owned())?;
            }
            Err(error) => {
                return Err(format!(
                    "Cannot inspect skill destination {}: {error}",
                    parent.display()
                ))
            }
        }
    }
}

pub(crate) fn create_skill_tool() -> ToolDefinition {
    ToolDefinition {
        name: "create_skill".to_owned(),
        description: "Create a new reusable skill at project scope or global scope. Project skills are written to <project_directory>/.agents/skills/<name>/SKILL.md; project_directory defaults to the current repository root (or working directory outside a repository). Global skills are written to the execution user's ~/.agents/skills/<name>/SKILL.md. Supply only the Markdown body in instructions; Galaxy generates valid YAML name/description frontmatter. The destination is shown in the normal file-edit review and existing skills are never silently replaced. Read an existing skill and use apply_file_diffs to update it. Local execution only; remote sessions must use their existing file-edit tools.".to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "maxLength": 63, "description": "Lowercase skill folder name, using letters, digits, and hyphens." },
                "description": { "type": "string", "maxLength": 512, "description": "What the skill does and when Galaxy should use it." },
                "instructions": { "type": "string", "description": "Markdown instructions for the skill, without YAML frontmatter." },
                "scope": { "type": "string", "enum": ["project", "global"] },
                "project_directory": { "type": "string", "description": "Optional existing directory inside the current project, absolute or relative to the repository root. Project scope only; e.g. app/server." },
            },
            "required": ["name", "description", "instructions", "scope"],
        }),
    }
}

#[cfg(test)]
#[path = "skill_creation_tests.rs"]
mod tests;
