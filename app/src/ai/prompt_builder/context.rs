//! Environment context that gets injected into system prompts.

/// Environment context about the user's machine and project.
#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    pub working_dir: String,
    pub home_dir: String,
    pub os: String,
    pub shell: String,
    pub git_branch: String,
    pub current_time: String,
}

impl PromptContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_working_dir(mut self, dir: impl Into<String>) -> Self {
        self.working_dir = dir.into();
        self
    }

    pub fn with_home_dir(mut self, dir: impl Into<String>) -> Self {
        self.home_dir = dir.into();
        self
    }

    pub fn with_os(mut self, os: impl Into<String>) -> Self {
        self.os = os.into();
        self
    }

    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    pub fn with_git_branch(mut self, branch: impl Into<String>) -> Self {
        self.git_branch = branch.into();
        self
    }

    pub fn with_current_time(mut self, time: impl Into<String>) -> Self {
        self.current_time = time.into();
        self
    }
}
