//! Operational modes that determine prompt behavior and tool availability.

/// The operational mode the agent is currently in.
///
/// Each mode provides different system prompt instructions and makes
/// different tools available to the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// General coding mode - the default. Full tool access, focused on
    /// implementing changes, debugging, and exploring codebases.
    Code,

    /// Planning mode - read-only exploration and design. The model should
    /// NOT make edits, only analyze, research, and produce a plan.
    Plan,

    /// Code review mode - focused on reviewing diffs/changes and providing
    /// feedback on correctness, style, and potential issues.
    Review,

    /// Conversation summary mode - condensing conversation history while
    /// preserving key decisions and context.
    Summarize,

    /// Title generation mode - producing a short title for a conversation.
    Title,
}

impl Mode {
    /// Whether this mode allows file edits.
    pub fn allows_edits(&self) -> bool {
        matches!(self, Mode::Code)
    }

    /// Whether this mode allows shell command execution.
    pub fn allows_shell(&self) -> bool {
        matches!(self, Mode::Code)
    }

    /// Whether this mode allows read-only exploration tools.
    pub fn allows_read(&self) -> bool {
        matches!(self, Mode::Code | Mode::Plan | Mode::Review)
    }

    /// Whether this mode should use sub-agents.
    pub fn allows_subagents(&self) -> bool {
        matches!(self, Mode::Code | Mode::Plan)
    }

    /// Human-readable label for logging/diagnostics.
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Code => "code",
            Mode::Plan => "plan",
            Mode::Review => "review",
            Mode::Summarize => "summarize",
            Mode::Title => "title",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}