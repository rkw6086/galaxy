//! LLM provider definitions.
//!
//! Each provider may have different prompting strategies, tone preferences,
//! and tool schema requirements. New providers are added here as variants.

/// The LLM provider being targeted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Anthropic Claude models (Sonnet, Opus, Haiku, etc.)
    Anthropic,
    // Future:
    // Gemini,
    // OpenAI,
}

impl Provider {
    pub fn label(&self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}
