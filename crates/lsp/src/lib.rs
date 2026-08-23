mod command_builder;
pub use command_builder::CommandBuilder;

mod config;
mod language_server_candidate;
pub use language_server_candidate::LanguageServerCandidate;
pub mod install;
mod manager;
mod model;

#[cfg_attr(not(target_family = "wasm"), path = "server_repo_watcher.rs")]
#[cfg_attr(target_family = "wasm", path = "server_repo_watcher_wasm.rs")]
mod server_repo_watcher;

pub mod servers;
mod service;
pub mod supported_servers;
#[cfg(not(target_arch = "wasm32"))]
mod transport;
pub mod types;

pub use config::{default_init_params, LanguageId, LspServerConfig};
pub use jsonrpc::{JsonRpcService, ServerNotificationEvent, Transport};
pub use lsp_types::notification::{self};
pub use lsp_types::{CompletionItem, Position, Range};
pub use manager::{LspManagerModel, LspManagerModelEvent};
pub use model::{
    BackgroundTaskInfo, DocumentDiagnostics, LanguageServerId, LspEvent, LspServerModel, LspState,
};
pub use service::LspService;
pub use types::{
    CodeActionData, CompletionItemData, CompletionKind, CompletionResult, CompletionTrigger,
    FileEdits, HoverContents, HoverResult, MarkupKind, ParameterInfo, ParameterLabel,
    PrepareRenameResult, ReferenceLocation, RenameResult, SignatureHelpResult, SignatureInfo,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LspServerLogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for LspServerLogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let level = match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        };
        f.write_str(level)
    }
}

use std::sync::Arc;

use anyhow::Result;
use galaxyui_core::r#async::executor::Background;
use galaxyui_core::AppContext;
#[cfg(not(target_arch = "wasm32"))]
use simple_logger::SimpleLogger;

pub struct LspServiceInitializationResult {
    pub service: LspService,
    pub channel: async_channel::Receiver<ServerNotificationEvent>,
}

/// Creates a complete LspService from an LSP server configuration.
///
/// If `logger` is provided, stderr output from the LSP server will be written
/// to its file for debugging purposes.
#[cfg(not(target_arch = "wasm32"))]
pub async fn spawn_lsp_service(
    config: LspServerConfig,
    executor: Arc<Background>,
    logger: Option<SimpleLogger>,
) -> Result<LspServiceInitializationResult> {
    let workspace_root = config.initial_workspace().to_path_buf();
    let post_initialize_notifications = config.post_initialize_notifications().to_vec();

    let resolved = match config.command_and_params().await {
        Ok(resolved) => resolved,
        Err(e) => {
            if let Some(ref logger) = logger {
                logger.log(format!("[startup error] {e}"));
            }
            return Err(e);
        }
    };

    let transport = match transport::ProcessTransport::new(
        resolved.command,
        executor.clone(),
        logger.clone(),
    ) {
        Ok(transport) => transport,
        Err(e) => {
            if let Some(ref logger) = logger {
                logger.log(format!("[startup error] {e}"));
            }
            return Err(e);
        }
    };

    let jsonrpc_service = JsonRpcService::new(
        Box::new(transport),
        executor,
        lsp_types::error_codes::REQUEST_FAILED,
    );

    let (notify_tx, notify_rx) = async_channel::unbounded::<ServerNotificationEvent>();
    let mut service = LspService::new(jsonrpc_service, notify_tx, workspace_root, logger.clone())?;

    if let Err(e) = service.initialize(resolved.params).await {
        if let Some(ref logger) = logger {
            logger.log(format!("[startup error] {e}"));
        }
        return Err(e);
    }

    for (method, params) in post_initialize_notifications {
        service
            .send_custom_notification_and_wait(method, params)
            .await?;
    }

    Ok(LspServiceInitializationResult {
        service,
        channel: notify_rx,
    })
}

#[cfg(target_arch = "wasm32")]
pub async fn spawn_lsp_service(
    _config: LspServerConfig,
    _executor: Arc<Background>,
    _logger: Option<()>,
) -> Result<LspServiceInitializationResult> {
    Err(anyhow::anyhow!("LSP is not supported in WASM environments"))
}

pub fn init(app: &mut AppContext) {
    app.add_singleton_model(|_| LspManagerModel::new());
}
