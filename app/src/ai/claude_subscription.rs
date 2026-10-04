//! User-supplied subscription tokens stay in Galaxy's secure credential store.
use galaxyui::AppContext;
use galaxyui_extras::secure_storage::AppContextExt as _;

use crate::settings::{OpenAIProviderConfig, OpenAIProviderKind};

const TOKEN_REFERENCE_PREFIX: &str = "galaxy-claude-token:";
pub(crate) const OAUTH_REFERENCE_PREFIX: &str = "galaxy-claude-oauth:";
pub(crate) const SUBSCRIPTION_WARNING: &str = "Unsupported Claude subscription integration. Anthropic does not permit third-party applications to use Claude subscription tokens. This may stop working without notice or lead to account restrictions. Galaxy retains its own tools, permissions, discovery, and orchestration.";

pub(crate) fn is_subscription_credential(credential: &str) -> bool {
    credential.trim().contains("sk-ant-oat")
        || credential.starts_with(TOKEN_REFERENCE_PREFIX)
        || is_stored_credential(credential)
}

pub(crate) fn is_stored_credential(credential: &str) -> bool {
    credential.starts_with(TOKEN_REFERENCE_PREFIX) || credential.starts_with(OAUTH_REFERENCE_PREFIX)
}

pub(crate) fn store_credential(
    provider: &mut OpenAIProviderConfig,
    ctx: &AppContext,
) -> Result<Option<String>, String> {
    if provider.kind != OpenAIProviderKind::Anthropic {
        return Ok(None);
    }
    let Some(token) = provider
        .api_key
        .as_deref()
        .filter(|token| token.trim().contains("sk-ant-oat"))
    else {
        return Ok(None);
    };
    let reference = format!("{TOKEN_REFERENCE_PREFIX}{}", uuid::Uuid::new_v4());
    ctx.secure_storage()
        .write_value_with_owner_only_fallback(&reference, token.trim())
        .map_err(|_| {
            "Could not securely save the Claude subscription token. The connection was not saved."
                .to_string()
        })?;
    provider.api_key = Some(reference.clone());
    Ok(Some(reference))
}

pub(crate) fn remove_credential(reference: &str, ctx: &AppContext) {
    if is_stored_credential(reference) {
        #[cfg(not(target_family = "wasm"))]
        super::claude_oauth::invalidate(reference);
        let _ = ctx.secure_storage().remove_value(reference);
    }
}
