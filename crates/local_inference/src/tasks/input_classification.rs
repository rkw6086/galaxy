use async_trait::async_trait;

use crate::InferenceTask;
use crate::engine::{CancellationToken, GenerationConfig, InferenceEngine};

pub struct InputClassificationTask;

#[derive(Clone)]
pub struct InputClassificationInput {
    pub user_input: String,
    pub recent_commands: Vec<String>,
    pub is_follow_up: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InputCategory {
    Shell,
    AgentPrompt,
}

pub struct InputClassificationResult {
    pub category: InputCategory,
    pub confidence: f32,
}

impl InputClassificationTask {
    fn build_prompt(input: &InputClassificationInput) -> String {
        let history = if input.recent_commands.is_empty() {
            String::from("(none)")
        } else {
            input
                .recent_commands
                .iter()
                .take(3)
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let follow_up_hint = if input.is_follow_up {
            " The user just received an AI response, so this may be a follow-up."
        } else {
            ""
        };

        format!(
            "<|im_start|>system\n\
             You classify terminal input. Respond with ONLY one word: \"shell\" or \"agent\".\n\
             \"shell\" = a CLI command the user wants to execute.\n\
             \"agent\" = a natural language prompt for an AI assistant.{follow_up_hint}\
             <|im_end|>\n\
             <|im_start|>user\n\
             Recent commands:\n{history}\n\
             Classify this input: \"{}\"\
             <|im_end|>\n\
             <|im_start|>assistant\n",
            input.user_input
        )
    }

    fn parse_output(output: &str) -> InputClassificationResult {
        let output_lower = output.trim().to_lowercase();

        let category = if output_lower.contains("shell") || output_lower.contains("command") {
            InputCategory::Shell
        } else {
            InputCategory::AgentPrompt
        };

        let confidence = if output_lower == "shell" || output_lower == "agent" {
            0.95
        } else {
            0.7
        };

        InputClassificationResult {
            category,
            confidence,
        }
    }

    pub async fn run_cancellable(
        &self,
        engine: &InferenceEngine,
        input: InputClassificationInput,
        cancel: &CancellationToken,
    ) -> anyhow::Result<InputClassificationResult> {
        let prompt = Self::build_prompt(&input);
        let config = GenerationConfig {
            max_tokens: 4,
            temperature: 0.0,
            top_p: 1.0,
        };
        let output = engine
            .generate_cancellable(&prompt, &config, cancel)
            .await?;
        Ok(Self::parse_output(&output))
    }
}

#[async_trait]
impl InferenceTask for InputClassificationTask {
    type Input = InputClassificationInput;
    type Output = InputClassificationResult;

    async fn run(
        &self,
        engine: &InferenceEngine,
        input: Self::Input,
    ) -> anyhow::Result<InputClassificationResult> {
        let prompt = Self::build_prompt(&input);
        let config = GenerationConfig {
            max_tokens: 4,
            temperature: 0.0,
            top_p: 1.0,
        };
        let output = engine.generate(&prompt, &config).await?;
        Ok(Self::parse_output(&output))
    }
}
