//! Prompt Builder Module
//!
//! Centralized system for constructing AI model prompts based on the current
//! operational mode (code, plan, review, etc.) and provider (Anthropic, etc.).
//!
//! # Architecture
//!
//! The prompt builder separates concerns into:
//! - **Mode**: What the agent is doing (coding, planning, reviewing, etc.)
//! - **Provider**: Which LLM is being used (Anthropic Claude, future: Gemini, GPT, etc.)
//! - **Context**: Environment info (working directory, OS, shell, git state, etc.)
//! - **Rules**: Project-specific rules from AGENTS.md / GALAXY.md files
//! - **Tools**: Which tools are available (varies by mode)
//!
//! The builder composes these layers to produce the final system prompt and tool
//! definitions for each request.

mod context;
mod mode;
mod prompts;
mod tools;

pub use context::PromptContext;
pub use mode::Mode;
pub use prompts::provider::Provider;
pub use tools::tools_for_mode;

use crate::ai::bedrock::convert::ToolDefinition;

/// The fully-resolved prompt configuration ready to send to a model.
#[derive(Debug, Clone)]
pub struct ResolvedPrompt {
    /// The system prompt text.
    pub system_prompt: String,
    /// The tool definitions available for this request.
    pub tools: Vec<ToolDefinition>,
}

/// The main prompt builder. Composes mode, provider, context, and rules
/// into a final system prompt and tool set.
pub struct PromptBuilder {
    mode: Mode,
    provider: Provider,
    context: PromptContext,
    project_rules: Vec<ProjectRule>,
    mcp_tools: Vec<ToolDefinition>,
}

/// A project rule loaded from AGENTS.md or GALAXY.md files.
#[derive(Debug, Clone)]
pub struct ProjectRule {
    pub root_path: String,
    pub content: String,
}

impl PromptBuilder {
    /// Create a new prompt builder with the given mode and provider.
    pub fn new(mode: Mode, provider: Provider) -> Self {
        Self {
            mode,
            provider,
            context: PromptContext::default(),
            project_rules: Vec::new(),
            mcp_tools: Vec::new(),
        }
    }

    /// Set the environment context.
    pub fn with_context(mut self, context: PromptContext) -> Self {
        self.context = context;
        self
    }

    /// Add project rules (from AGENTS.md, GALAXY.md, etc.).
    pub fn with_project_rules(mut self, rules: Vec<ProjectRule>) -> Self {
        self.project_rules = rules;
        self
    }

    /// Add MCP tools from connected servers.
    pub fn with_mcp_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.mcp_tools = tools;
        self
    }

    /// Build the final resolved prompt.
    pub fn build(&self) -> ResolvedPrompt {
        let system_prompt = self.build_system_prompt();
        let tools = self.build_tools();
        ResolvedPrompt {
            system_prompt,
            tools,
        }
    }

    fn build_system_prompt(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(8);

        // 1. Identity + mode-specific base prompt
        parts.push(self.identity_prompt());

        // 2. Environment context
        if let Some(env) = self.environment_section() {
            parts.push(env);
        }

        // 3. Project rules
        if let Some(rules) = self.rules_section() {
            parts.push(rules);
        }

        // 4. Mode-specific instructions
        parts.push(self.mode_instructions());

        // 5. Tool usage guidelines
        parts.push(self.tool_usage_section());

        parts.join("\n\n")
    }

    fn identity_prompt(&self) -> String {
        let base = prompts::base_identity(&self.provider);
        let mode_identity = prompts::mode_identity(&self.mode);
        format!("{base}\n\n{mode_identity}")
    }

    fn environment_section(&self) -> Option<String> {
        let ctx = &self.context;
        if ctx.working_dir.is_empty() && ctx.os.is_empty() {
            return None;
        }

        let mut lines = vec!["## Environment".to_string()];

        if !ctx.working_dir.is_empty() {
            lines.push(format!("- Working directory: {}", ctx.working_dir));
        }
        if !ctx.home_dir.is_empty() {
            lines.push(format!("- Home directory: {}", ctx.home_dir));
        }
        if !ctx.os.is_empty() {
            lines.push(format!("- OS: {}", ctx.os));
        }
        if !ctx.shell.is_empty() {
            lines.push(format!("- Shell: {}", ctx.shell));
        }
        if !ctx.git_branch.is_empty() {
            lines.push(format!("- Git branch: {}", ctx.git_branch));
        }
        if !ctx.current_time.is_empty() {
            lines.push(format!("- Current time (UTC): {}", ctx.current_time));
        }

        Some(lines.join("\n"))
    }

    fn rules_section(&self) -> Option<String> {
        if self.project_rules.is_empty() {
            return None;
        }

        let mut section = String::from("## Project Rules\n");
        for rule in &self.project_rules {
            if !rule.root_path.is_empty() {
                section.push_str(&format!("### Rules from {}\n", rule.root_path));
            }
            section.push_str(&rule.content);
            section.push('\n');
        }
        Some(section)
    }

    fn mode_instructions(&self) -> String {
        prompts::mode_instructions(&self.mode, &self.provider)
    }

    fn tool_usage_section(&self) -> String {
        prompts::tool_usage_guidelines(&self.mode, &self.provider)
    }

    fn build_tools(&self) -> Vec<ToolDefinition> {
        let mut tool_set = tools::tools_for_mode(&self.mode);

        // Append MCP tools
        tool_set.extend(self.mcp_tools.clone());

        tool_set
    }
}

#[cfg(test)]
mod tests;