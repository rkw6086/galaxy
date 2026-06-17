pub mod types;

use crate::ai::bedrock::client::BedrockClientConfig;
use crate::ai::openai::client::OpenAIClientConfig;

pub enum ProviderConfig {
    Bedrock(BedrockClientConfig),
    OpenAI(OpenAIClientConfig),
    None,
}
