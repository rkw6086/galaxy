use async_trait::async_trait;

use crate::InferenceTask;
use crate::engine::{GenerationConfig, InferenceEngine};

pub struct PromptSuggestionTask;

pub struct PromptSuggestionInput {
    pub recent_commands: Vec<String>,
    pub current_input: String,
    pub working_directory: String,
}

#[async_trait]
impl InferenceTask for PromptSuggestionTask {
    type Input = PromptSuggestionInput;
    type Output = Vec<String>;

    async fn run(
        &self,
        engine: &InferenceEngine,
        input: Self::Input,
    ) -> anyhow::Result<Vec<String>> {
        let history = input
            .recent_commands
            .iter()
            .take(5)
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "<|im_start|>system\n\
             You suggest terminal commands. Give exactly 3 suggestions, one per line. \
             No numbering, no explanation, just the commands.\
             <|im_end|>\n\
             <|im_start|>user\n\
             Directory: {}\n\
             Recent commands:\n{}\n\
             Current partial input: \"{}\"\n\
             Suggest 3 likely next commands:\
             <|im_end|>\n\
             <|im_start|>assistant\n",
            input.working_directory, history, input.current_input
        );

        let config = GenerationConfig {
            max_tokens: 64,
            temperature: 0.6,
            top_p: 0.9,
        };

        let output = engine.generate(&prompt, &config).await?;

        let suggestions: Vec<String> = output
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .take(3)
            .collect();

        Ok(suggestions)
    }
}
