use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use ai::index::full_source_code_embedding::manager::{
    CodebaseIndexManager, CodebaseIndexManagerEvent,
};
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use ai::index::full_source_code_embedding::RetrievalID;
#[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
use ai::index::local_project_index::LocalProjectIndexManager;
use ai::index::locations::CodeContextLocation;
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use anyhow::anyhow;
use futures_util::stream::AbortHandle;
use galaxy_core::features::FeatureFlag;
use galaxyui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity};
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use instant::Instant;

#[cfg(not(target_family = "wasm"))]
use crate::ai::agent::SearchCodebaseFailureReason;
use crate::ai::agent::{AIAgentActionId, SearchCodebaseResult, MAX_SEARCH_CODEBASE_FILES};
use crate::ai::blocklist::SessionContext;
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use crate::ai::get_relevant_files::api::{FileContext as FileContextRequest, GetRelevantFiles};
#[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
use crate::ai::get_relevant_files::controller::GetRelevantFilesError::LocalIndexUnavailable;
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use crate::ai::outline::{OutlineStatus, RepoOutlines};
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use crate::report_error;
#[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
use crate::server::server_api::{AIApiError, ServerApiProvider};
use crate::{send_telemetry_from_ctx, TelemetryEvent};
#[cfg_attr(not(target_family = "wasm"), path = "remote_search/native.rs")]
#[cfg_attr(target_family = "wasm", path = "remote_search/wasm.rs")]
mod remote_search;

#[derive(Debug)]
pub enum GetRelevantFilesControllerEvent {
    Success {
        action_id: AIAgentActionId,
        result: GetRelevantFilesControllerResult,
    },
    Error {
        action_id: AIAgentActionId,
    },
}

impl GetRelevantFilesControllerEvent {
    pub fn action_id(&self) -> &AIAgentActionId {
        match self {
            GetRelevantFilesControllerEvent::Success { action_id, .. } => action_id,
            GetRelevantFilesControllerEvent::Error { action_id } => action_id,
        }
    }
}

#[derive(Debug)]
pub enum GetRelevantFilesControllerResult {
    Locations(Arc<HashSet<CodeContextLocation>>),
    SearchResult(SearchCodebaseResult),
}

pub enum GetRelevantFilesRequestTarget {
    Local {
        directory: PathBuf,
    },
    Remote {
        session_context: SessionContext,
        requested_codebase_path: Option<String>,
    },
}

/// Lifecycle state for the two-stage `get_files` relevant-file query.
///
/// The first stage discovers paths through `GetRelevantFilesController`; the second stage reads
/// current file contents in `GetFilesExecutor`. Keeping this state separate from the controller's
/// cancellation handles preserves the existing action protocol for both local and legacy search.
#[derive(Debug, Clone)]
pub enum GetRelevantFilesStatus {
    Pending { root_repo_path: PathBuf },
    InFlight { root_repo_path: PathBuf },
    Success { file_paths: Vec<PathBuf> },
    Failed { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum GetRelevantFilesError {
    #[error("Repo outline is still being computed.")]
    Pending,
    #[error("Failed to create outline.")]
    CreateFailed,
    #[error("Failed to create outline.")]
    Missing,
    #[error("Local project index is unavailable: {0}")]
    LocalIndexUnavailable(String),
}

/// This enum allows us to use both the existing structure for outline-based indexing
/// and the new full source code indexing manager/model.
enum RequestHandle {
    /// Used with outline-based indexing and remote search.
    AbortHandle(AbortHandle),

    /// Used with full source code indexing.
    #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
    RetrievalID {
        repo_path: PathBuf,
        retrieval_id: RetrievalID,
        start_time: Instant,
    },
}

impl RequestHandle {
    fn abort(&mut self, ctx: &mut AppContext) {
        match self {
            RequestHandle::AbortHandle(abort_handle) => abort_handle.abort(),
            #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
            RequestHandle::RetrievalID {
                repo_path,
                retrieval_id,
                start_time: _,
            } => {
                CodebaseIndexManager::handle(ctx).update(ctx, |index_manager, ctx| {
                    if let Err(err) =
                        index_manager.abort_retrieval_request(repo_path, retrieval_id.clone(), ctx)
                    {
                        log::error!("Failed to abort file retrieval request: {err:?}");
                    }
                });
            }
        }
    }
}

/// Controller for GetRelevantFiles action. This is scoped per terminal session.
#[derive(Default)]
pub struct GetRelevantFilesController {
    /// Search requests currently in flight, keyed by the originating action ID.
    /// This allows several SearchCodebase actions to be active at once without newer requests
    /// cancelling unrelated older ones.
    pending_requests: std::collections::HashMap<AIAgentActionId, RequestHandle>,
    /// Status for the two-stage `get_files` relevant-file query protocol.
    relevant_file_statuses: std::collections::HashMap<AIAgentActionId, GetRelevantFilesStatus>,
}

impl GetRelevantFilesController {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
        {
            let codebase_manager = CodebaseIndexManager::handle(ctx);
            ctx.subscribe_to_model(&codebase_manager, Self::handle_codebase_manager_event);
        }
        Self::default()
    }

    fn root_for_status(&self, directory: &Path, app: &AppContext) -> PathBuf {
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        {
            app.try_get_singleton_model_as_ref::<LocalProjectIndexManager>()
                .and_then(|manager| manager.root_for_path(directory))
                .unwrap_or_else(|| directory.to_path_buf())
        }
        #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
        {
            self.root_directory_for_search(directory, app)
                .unwrap_or_else(|| directory.to_path_buf())
        }
    }

    /// Queues the first stage of a relevant-file query for `GetFilesExecutor`.
    pub fn queue_request(
        &mut self,
        action_id: AIAgentActionId,
        directory: &Path,
        app: &AppContext,
    ) {
        let root_repo_path = self.root_for_status(directory, app);
        self.relevant_file_statuses.insert(
            action_id,
            GetRelevantFilesStatus::Pending { root_repo_path },
        );
    }

    /// Returns the current status of a relevant-file query.
    pub fn status(&self, action_id: &AIAgentActionId) -> Option<&GetRelevantFilesStatus> {
        self.relevant_file_statuses.get(action_id)
    }

    fn set_relevant_file_status(
        &mut self,
        action_id: &AIAgentActionId,
        status: GetRelevantFilesStatus,
    ) {
        self.relevant_file_statuses
            .insert(action_id.clone(), status);
    }

    #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
    fn pending_request_details_for_retrieval_id(
        &self,
        pending_retrieval_id: &RetrievalID,
    ) -> Option<(&AIAgentActionId, &Instant)> {
        // Full-source embedding completion events only carry the retrieval ID, so map them back to
        // the agent action that initiated the request before emitting results/telemetry.
        self.pending_requests
            .iter()
            .find_map(|(action_id, request_handle)| match request_handle {
                RequestHandle::AbortHandle(_) => None,
                RequestHandle::RetrievalID {
                    retrieval_id,
                    start_time,
                    ..
                } if retrieval_id == pending_retrieval_id => Some((action_id, start_time)),
                RequestHandle::RetrievalID { .. } => None,
            })
    }

    #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
    fn handle_codebase_manager_event(
        &mut self,
        _: ModelHandle<CodebaseIndexManager>,
        codebase_manager_event: &CodebaseIndexManagerEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match codebase_manager_event {
            CodebaseIndexManagerEvent::RetrievalRequestFailed {
                retrieval_id,
                error_message: error,
            } => {
                let Some((action_id, _search_start)) =
                    self.pending_request_details_for_retrieval_id(retrieval_id)
                else {
                    return;
                };
                send_telemetry_from_ctx!(
                    TelemetryEvent::FullEmbedCodebaseContextSearchFailed {
                        action_id: action_id.clone(),
                        error: error.to_string(),
                    },
                    ctx
                );

                self.handle_relevant_file_paths_result(
                    Err(anyhow!(error.to_owned())),
                    action_id.clone(),
                    ctx,
                );
            }
            CodebaseIndexManagerEvent::RetrievalRequestCompleted {
                retrieval_id,
                fragments,
                out_of_sync_delay,
            } => {
                let Some((action_id, search_start)) =
                    self.pending_request_details_for_retrieval_id(retrieval_id)
                else {
                    return;
                };
                send_telemetry_from_ctx!(
                    TelemetryEvent::FullEmbedCodebaseContextSearchSuccess {
                        action_id: action_id.clone(),
                        total_search_duration: search_start.elapsed(),
                        out_of_sync_delay: *out_of_sync_delay,
                    },
                    ctx
                );

                self.handle_relevant_file_paths_result(
                    Ok(fragments.clone()),
                    action_id.clone(),
                    ctx,
                );
            }
            _ => (),
        }
    }

    /// Start a new search query based on the repo outline.
    pub fn send_request(
        &mut self,
        target: GetRelevantFilesRequestTarget,
        query: String,
        partial_path_segments: Option<&Vec<String>>,
        action_id: AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), GetRelevantFilesError> {
        // Cancel any previous request for this action before dispatching to either the local or
        // remote implementation.
        self.cancel_request_for_action(&action_id, ctx);
        match target {
            GetRelevantFilesRequestTarget::Local { directory } => {
                let root_repo_path = self
                    .relevant_file_statuses
                    .get(&action_id)
                    .and_then(|status| match status {
                        GetRelevantFilesStatus::Pending { root_repo_path }
                        | GetRelevantFilesStatus::InFlight { root_repo_path } => {
                            Some(root_repo_path.clone())
                        }
                        GetRelevantFilesStatus::Success { .. }
                        | GetRelevantFilesStatus::Failed { .. } => None,
                    })
                    .unwrap_or_else(|| self.root_for_status(&directory, ctx));
                self.set_relevant_file_status(
                    &action_id,
                    GetRelevantFilesStatus::InFlight { root_repo_path },
                );
                let result = self.send_local_request(
                    &directory,
                    query,
                    partial_path_segments,
                    action_id.clone(),
                    ctx,
                );
                if let Err(error) = &result {
                    self.set_relevant_file_status(
                        &action_id,
                        GetRelevantFilesStatus::Failed {
                            message: error.to_string(),
                        },
                    );
                }
                result
            }
            GetRelevantFilesRequestTarget::Remote {
                session_context,
                requested_codebase_path,
            } => self.send_remote_request(
                session_context,
                requested_codebase_path,
                query,
                partial_path_segments.cloned(),
                action_id,
                ctx,
            ),
        }
    }

    fn send_local_request(
        &mut self,
        directory: &Path,
        query: String,
        partial_path_segments: Option<&Vec<String>>,
        action_id: AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), GetRelevantFilesError> {
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        {
            let Some(local_index) =
                ctx.try_get_singleton_model_as_ref::<LocalProjectIndexManager>()
            else {
                return Err(LocalIndexUnavailable(
                    "The local project index is not available in this session.".to_string(),
                ));
            };
            if local_index.is_searchable_for_path(directory) {
                let locations: HashSet<CodeContextLocation> = local_index
                    .search(directory, &query, partial_path_segments.map(Vec::as_slice))
                    .map_err(|error| LocalIndexUnavailable(error.to_string()))?
                    .into_iter()
                    .take(MAX_SEARCH_CODEBASE_FILES)
                    .map(|hit| CodeContextLocation::WholeFile(hit.path))
                    .collect();
                let file_paths = locations
                    .iter()
                    .map(|location| location.path().clone())
                    .collect();
                self.set_relevant_file_status(
                    &action_id,
                    GetRelevantFilesStatus::Success { file_paths },
                );
                ctx.emit(GetRelevantFilesControllerEvent::Success {
                    action_id,
                    result: GetRelevantFilesControllerResult::Locations(Arc::new(locations)),
                });
                return Ok(());
            }
            return Err(LocalIndexUnavailable(
                "The local project index is not ready. Run /init or /index first.".to_string(),
            ));
        }

        #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
        let _ = (directory, query, partial_path_segments, action_id, ctx);

        #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
        if FeatureFlag::FullSourceCodeEmbedding.is_enabled() {
            let codebase_mgr = CodebaseIndexManager::handle(ctx);
            if let Some(base_path) = codebase_mgr.as_ref(ctx).root_path_for_codebase(directory) {
                match codebase_mgr.update(ctx, |index_manager, ctx| {
                    index_manager.retrieve_relevant_files(query.clone(), base_path.as_path(), ctx)
                }) {
                    Ok(retrieval_request_id) => {
                        log::info!("Using full source code embedding for search");
                        let search_start = Instant::now();
                        self.pending_requests.insert(
                            action_id,
                            RequestHandle::RetrievalID {
                                repo_path: base_path.clone(),
                                retrieval_id: retrieval_request_id,
                                start_time: search_start,
                            },
                        );

                        return Ok(());
                    }
                    Err(e) => {
                        log::info!(
                            "Failed to initiate full source code search: {e}, falling back to outline-based search"
                        );
                    }
                }
            }
        }

        #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
        {
            match RepoOutlines::as_ref(ctx).get_outline(directory) {
                Some((OutlineStatus::Complete(outline), base_path)) => {
                    let server_api = ServerApiProvider::as_ref(ctx).get();

                    let file_outlines = outline.to_file_symbols(partial_path_segments);
                    if file_outlines.len() < MINIMUM_FILE_COUNT_FOR_API_CALL {
                        let locations: Arc<HashSet<CodeContextLocation>> = Arc::new(
                            file_outlines
                                .into_iter()
                                .map(|file| {
                                    CodeContextLocation::WholeFile(PathBuf::from(file.path))
                                })
                                .collect(),
                        );
                        let file_paths = locations
                            .iter()
                            .map(|location| location.path().clone())
                            .collect();
                        self.set_relevant_file_status(
                            &action_id,
                            GetRelevantFilesStatus::Success { file_paths },
                        );
                        ctx.emit(GetRelevantFilesControllerEvent::Success {
                            action_id,
                            result: GetRelevantFilesControllerResult::Locations(locations),
                        });
                    } else {
                        let outline_request = GetRelevantFiles {
                            query,
                            files: file_outlines
                                .into_iter()
                                .map(|outline| FileContextRequest {
                                    path: outline.path,
                                    symbols: outline.symbols,
                                })
                                .collect(),
                        };
                        let action_id_clone = action_id.clone();
                        let request_abort_handle = ctx
                            .spawn(
                                async move {
                                    let response =
                                        server_api.get_relevant_files(&outline_request).await?;
                                    Ok(Arc::new(
                                        response
                                            .relevant_file_paths
                                            .into_iter()
                                            .filter_map(|path| {
                                                let file_path = base_path.join(path);
                                                // Validate the returned file paths.
                                                if file_path.exists() {
                                                    Some(CodeContextLocation::WholeFile(file_path))
                                                } else {
                                                    None
                                                }
                                            })
                                            .collect(),
                                    ))
                                },
                                move |me,
                                      relevant_file_paths: Result<
                                    Arc<HashSet<CodeContextLocation>>,
                                    AIApiError,
                                >,
                                      ctx| {
                                    me.handle_relevant_file_paths_result(
                                        relevant_file_paths.map_err(|e| anyhow!(e)),
                                        action_id_clone,
                                        ctx,
                                    )
                                },
                            )
                            .abort_handle();
                        self.pending_requests
                            .insert(action_id, RequestHandle::AbortHandle(request_abort_handle));
                    }
                    Ok(())
                }
                Some((OutlineStatus::Pending, _)) => Err(GetRelevantFilesError::Pending),
                Some((OutlineStatus::Failed, _)) => Err(GetRelevantFilesError::CreateFailed),
                None => Err(GetRelevantFilesError::Missing),
            }
        }
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        unreachable!("native local search returns before the legacy outline fallback")
    }

    fn send_remote_request(
        &mut self,
        session_context: SessionContext,
        requested_codebase_path: Option<String>,
        query: String,
        partial_path_segments: Option<Vec<String>>,
        action_id: AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), GetRelevantFilesError> {
        match remote_search::send_request(
            query,
            partial_path_segments,
            session_context,
            requested_codebase_path,
            action_id.clone(),
            ctx,
        ) {
            #[cfg(not(target_family = "wasm"))]
            remote_search::RemoteSearchRequest::Pending(abort_handle) => {
                self.pending_requests
                    .insert(action_id, RequestHandle::AbortHandle(abort_handle));
            }
            remote_search::RemoteSearchRequest::Ready(result) => {
                ctx.emit(GetRelevantFilesControllerEvent::Success {
                    action_id,
                    result: GetRelevantFilesControllerResult::SearchResult(result),
                });
            }
        }
        Ok(())
    }

    #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
    fn handle_relevant_file_paths_result(
        &mut self,
        relevant_file_locations: anyhow::Result<Arc<HashSet<CodeContextLocation>>>,
        action_id: AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.pending_requests.remove(&action_id).is_none() {
            return;
        }
        match relevant_file_locations {
            Ok(relevant_file_locations) => {
                let file_paths = relevant_file_locations
                    .iter()
                    .map(|location| location.path().clone())
                    .collect();
                self.set_relevant_file_status(
                    &action_id,
                    GetRelevantFilesStatus::Success { file_paths },
                );
                ctx.emit(GetRelevantFilesControllerEvent::Success {
                    action_id,
                    result: GetRelevantFilesControllerResult::Locations(relevant_file_locations),
                });
            }
            Err(e) => {
                let message = e.to_string();
                self.set_relevant_file_status(
                    &action_id,
                    GetRelevantFilesStatus::Failed {
                        message: message.clone(),
                    },
                );
                report_error!(anyhow!(e).context("get_relevant_files failed"));
                ctx.emit(GetRelevantFilesControllerEvent::Error { action_id });
            }
        };
    }

    #[cfg(not(target_family = "wasm"))]
    fn handle_remote_search_result(
        &mut self,
        search_result: anyhow::Result<SearchCodebaseResult>,
        action_id: AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.pending_requests.remove(&action_id).is_none() {
            return;
        }

        let result = search_result.unwrap_or_else(|e| SearchCodebaseResult::Failed {
            reason: SearchCodebaseFailureReason::ClientError,
            message: e.to_string(),
        });
        ctx.emit(GetRelevantFilesControllerEvent::Success {
            action_id,
            result: GetRelevantFilesControllerResult::SearchResult(result),
        });
    }

    /// Returns the path to the root directory for a codebase search where pwd is `directory`.
    pub fn root_directory_for_search(&self, directory: &Path, app: &AppContext) -> Option<PathBuf> {
        #[cfg(all(feature = "local_fs", not(target_family = "wasm")))]
        {
            app.try_get_singleton_model_as_ref::<LocalProjectIndexManager>()
                .and_then(|manager| manager.root_for_path(directory))
        }

        #[cfg(any(not(feature = "local_fs"), target_family = "wasm"))]
        {
            let mut start = None;
            if FeatureFlag::FullSourceCodeEmbedding.is_enabled() {
                start = CodebaseIndexManager::as_ref(app).root_path_for_codebase(directory);
            }
            start.or_else(|| {
                RepoOutlines::as_ref(app)
                    .get_outline(directory)
                    .map(|(_, root)| root)
            })
        }
    }

    pub fn root_directory_for_remote_search(
        &self,
        session_context: &SessionContext,
        requested_codebase_path: Option<&str>,
        app: &AppContext,
    ) -> Option<PathBuf> {
        remote_search::root_directory_for_search(session_context, requested_codebase_path, app)
    }

    pub fn cancel_request_for_action(
        &mut self,
        action_id: &AIAgentActionId,
        ctx: &mut ModelContext<Self>,
    ) {
        if let Some(mut request_handle) = self.pending_requests.remove(action_id) {
            request_handle.abort(ctx);
        }
    }
}

impl Entity for GetRelevantFilesController {
    type Event = GetRelevantFilesControllerEvent;
}
