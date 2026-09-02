pub mod types;

use crate::ai::openai::client::OpenAIClientConfig;

#[cfg(not(target_family = "wasm"))]
use crate::ai::provider::client::BedrockClientConfig;

#[allow(dead_code)]
#[derive(Clone)]
pub enum ProviderConfig {
    #[cfg(not(target_family = "wasm"))]
    Bedrock(BedrockClientConfig),
    OpenAI(OpenAIClientConfig),
    None,
}

// Provider-neutral module boundary; Bedrock-specific transport helpers live here without a
// provider-named directory so the provider layer can be reorganized independently of callers.
#[cfg(not(target_family = "wasm"))]
pub mod client;
#[cfg(not(target_family = "wasm"))]
pub mod convert;
#[cfg(not(target_family = "wasm"))]
pub mod crash_log;
#[cfg(not(target_family = "wasm"))]
pub mod diagnostic;
#[cfg(not(target_family = "wasm"))]
pub mod discovery;
#[cfg(not(target_family = "wasm"))]
pub mod external_config;
#[cfg(not(target_family = "wasm"))]
pub mod models;
#[cfg(not(target_family = "wasm"))]
pub mod request_translator;
#[cfg(not(target_family = "wasm"))]
pub mod response_translator;
#[cfg(not(target_family = "wasm"))]
pub mod settings_view;

#[cfg(not(target_family = "wasm"))]
#[cfg(test)]
mod convert_tests;
#[cfg(not(target_family = "wasm"))]
#[cfg(test)]
#[allow(dead_code)]
mod e2e_tests;
#[cfg(not(target_family = "wasm"))]
#[cfg(test)]
#[allow(dead_code)]
mod integration_tests;
#[cfg(not(target_family = "wasm"))]
#[cfg(test)]
mod models_tests;
#[cfg(not(target_family = "wasm"))]
#[cfg(test)]
mod response_translator_tests;
