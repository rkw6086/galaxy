pub mod engine;
pub mod tasks;

use async_trait::async_trait;
pub use engine::{CancellationToken, Device, GenerationConfig, InferenceEngine};
pub use tasks::{
    InputCategory, InputClassificationInput, InputClassificationResult, InputClassificationTask,
    PromptSuggestionInput, PromptSuggestionTask, TabNamingInput, TabNamingTask,
};

#[async_trait]
pub trait InferenceTask: Send + Sync {
    type Input: Send;
    type Output: Send;

    async fn run(
        &self,
        engine: &InferenceEngine,
        input: Self::Input,
    ) -> anyhow::Result<Self::Output>;
}
