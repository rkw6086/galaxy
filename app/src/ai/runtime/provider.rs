use futures::channel::oneshot;

use crate::ai::agent::api::{self, ConvertToAPITypeError};
use crate::ai::provider::ProviderConfig;

/// Application-facing provider runtime dispatcher.
///
/// OpenAI-compatible models can opt into the provider-neutral Rig runtime;
/// other models continue through their current translators while migration is
/// in progress. Both paths preserve the existing UI response stream contract.
pub(crate) struct ProviderRuntime {
    provider_config: ProviderConfig,
}

impl ProviderRuntime {
    pub(crate) fn new(provider_config: ProviderConfig) -> Self {
        Self { provider_config }
    }

    pub(crate) async fn start_turn(
        self,
        params: api::RequestParams,
        cancellation_rx: oneshot::Receiver<()>,
    ) -> Result<api::ResponseStream, ConvertToAPITypeError> {
        api::generate_multi_agent_output(self.provider_config, params, cancellation_rx).await
    }
}
