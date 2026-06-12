use async_trait::async_trait;

use crate::InferenceTask;
use crate::engine::{GenerationConfig, InferenceEngine};

pub struct TabNamingTask;

pub struct TabNamingInput {
    pub recent_commands: Vec<String>,
    pub working_directory: String,
}

#[async_trait]
impl InferenceTask for TabNamingTask {
    type Input = TabNamingInput;
    type Output = String;

    async fn run(&self, engine: &InferenceEngine, input: Self::Input) -> anyhow::Result<String> {
        let commands_str = input
            .recent_commands
            .iter()
            .take(5)
            .map(|c| format!("- {c}"))
            .collect::<Vec<_>>()
            .join("\n");

        let prompt = format!(
            "<|im_start|>system\n\
             You name terminal tabs. Respond with ONLY a short name (2-4 words max). No explanation.\
             <|im_end|>\n\
             <|im_start|>user\n\
             Directory: {}\n\
             Recent commands:\n{}\n\
             What should this tab be named?\
             <|im_end|>\n\
             <|im_start|>assistant\n",
            input.working_directory, commands_str
        );

        let config = GenerationConfig {
            max_tokens: 12,
            temperature: 0.3,
            top_p: 0.9,
        };

        let output = engine.generate(&prompt, &config).await?;

        // Clean up: take only the first line, strip quotes
        let name = output
            .lines()
            .next()
            .unwrap_or(&output)
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();

        Ok(name)
    }
}
