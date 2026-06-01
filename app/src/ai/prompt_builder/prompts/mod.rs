//! Prompt text templates organized by provider and mode.

pub mod provider;

mod anthropic;

use crate::ai::prompt_builder::mode::Mode;
use provider::Provider;

/// The base identity statement shared across all modes.
pub fn base_identity(provider: &Provider) -> &'static str {
    match provider {
        Provider::Anthropic => anthropic::BASE_IDENTITY,
    }
}

/// Mode-specific identity/role description.
pub fn mode_identity(mode: &Mode) -> &'static str {
    match mode {
        Mode::Code => "You are in coding mode. Your primary role is to help the user implement changes, debug issues, explore codebases, and complete software engineering tasks.",
        Mode::Plan => "You are in planning mode. Your role is to analyze, research, and design an implementation approach. You must NOT make any edits or run any mutating commands — only read, search, and think.",
        Mode::Review => "You are in code review mode. Your role is to review code changes and provide detailed feedback on correctness, style, potential bugs, and improvements.",
        Mode::Summarize => "You are summarizing a conversation. Preserve key decisions, code changes, file paths, and important context. Be concise but retain all information needed to continue the work.",
        Mode::Title => "Generate a brief conversation title. Output ONLY the title — no explanation, no quotes. Keep it under 50 characters.",
    }
}

/// Mode-specific behavioral instructions.
pub fn mode_instructions(mode: &Mode, provider: &Provider) -> String {
    match provider {
        Provider::Anthropic => anthropic::mode_instructions(mode),
    }
}

/// Tool usage guidelines tailored to mode and provider.
pub fn tool_usage_guidelines(mode: &Mode, provider: &Provider) -> String {
    match provider {
        Provider::Anthropic => anthropic::tool_usage_guidelines(mode),
    }
}