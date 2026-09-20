//! The `BlocklistAIActionModel` is responsible for managing state related to `AIAgentAction`s
//! received in AI responses.
//!
//! Notably, this model manages the "action queue", which is used to support receiving multiple
//! actions in a single AI response.
//!
//! Actions are executed, one by one, either initiated by the user or auto-executed, if the user's
//! AI permissions permit. Action execution is handled by `BlocklistAIActionExecutor`, which
//! consumes the action to be executed and emits an event when execution is complete.
//!
//! Action state also has indirect implications for various parts of the terminal UI -- for
//! example, the input should be hidden if there is a pending AI requested command that requires
//! action from the user.

mod execute;
mod preprocess;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Local;
pub(crate) use execute::{
    apply_edits, coerce_integer_args, FileReadResult, MalformedFinalLineProxyEvent,
};
#[cfg(test)]
pub(crate) use execute::{compose_run_agents_child_prompt, run_agents_to_start_agent_mode};
pub use execute::{
    read_local_file_context, EditAcceptAndContinueClickedEvent, EditAcceptClickedEvent,
    EditResolvedEvent, EditStats, NewConversationDecision, PromptSuggestionExecutor,
    ReadFileContextResult, RequestFileEditsExecutor, RequestFileEditsFormatKind,
    RequestFileEditsTelemetryEvent, RunAgentsExecutor, RunAgentsExecutorEvent,
    RunAgentsSpawningSnapshot, ShellCommandExecutor, ShellCommandExecutorEvent, StartAgentExecutor,
    StartAgentExecutorEvent, StartAgentRequest, StartAgentRequestId, StartAgentWaitPolicy,
};
use futures::future::{join_all, BoxFuture};
use galaxy_agent_core::{
    ExternalWorkId, PendingToolBatch, PermissionDecision, PermissionKind, PermissionRequest,
    ToolEvent, ToolResult, ToolResultStatus,
};
use galaxyui::{AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};
use itertools::Itertools;
use parking_lot::FairMutex;
use preprocess::{PendingPreprocessedActions, PreprocessId};

use self::execute::ask_user_question::AskUserQuestionExecutor;
use self::execute::search_codebase::SearchCodebaseExecutor;
use self::execute::{
    BlocklistAIActionExecutor, BlocklistAIActionExecutorEvent, NotExecutedReason,
    RunningActionPhase, TryExecuteResult,
};
use super::history_model::BlocklistAIHistoryEvent;
use super::BlocklistAIHistoryModel;
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResult, AIAgentActionResultType,
    AIAgentActionType, AIAgentActionTypeDiscriminants, AIAgentExchange, AIAgentInput,
    CancellationOutcome, CancellationReason, CreateDocumentsResult, EditDocumentsResult,
    RequestCommandOutputResult,
};
#[cfg(not(target_family = "wasm"))]
use crate::ai::agent::{
    AskUserQuestionResult, CallMCPToolResult, FetchConversationResult, FileGlobResult,
    FileGlobV2Result, GrepResult, InsertReviewCommentsResult, ReadDocumentsResult, ReadFilesResult,
    ReadMCPResourceResult, ReadShellCommandOutputResult, ReadSkillResult, RequestComputerUseResult,
    RequestFileEditsResult, RunAgentsResult, SearchCodebaseResult, SendMessageToAgentResult,
    StartAgentResult, TransferShellCommandControlToUserResult, UploadArtifactResult,
    UseComputerResult, WriteToLongRunningShellCommandResult,
};
use crate::ai::ai_document_view::DEFAULT_PLANNING_DOCUMENT_TITLE;
use crate::ai::blocklist::action_model::execute::suggest_new_conversation::SuggestNewConversationExecutor;
use crate::ai::document::ai_document_model::AIDocumentModel;
use crate::ai::get_relevant_files::controller::GetRelevantFilesController;
#[cfg(not(target_family = "wasm"))]
use crate::ai::remote_logging::{self, RemoteLogLevel, RemoteLogRecord};
use crate::ai::runtime::ProviderToolExecutionRef;
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model_events::ModelEventDispatcher;
use crate::terminal::TerminalModel;
use crate::{send_telemetry_from_ctx, TelemetryEvent};

/// The status of an action from an AI output.
#[derive(Clone, Debug)]
pub enum AIActionStatus {
    /// The action is preprocessing and has yet to be started.
    Preprocessing,

    /// The action is queued, but isn't yet actionable by the user (there is another action that
    /// was queued prior that the user must act on first).
    Queued,

    // The action is next up for execution, but is blocked by the completion of another action
    // and/or user confirmation.
    Blocked,

    /// The action is running asynchronously.
    ///
    /// This is never the status for actions that are executed synchronously.
    RunningAsync,

    /// The action has either been cancelled or completed.
    Finished(Arc<AIAgentActionResult>),
}

impl AIActionStatus {
    /// Returns whether the action is currently preprocessing.
    pub fn is_preprocessing(&self) -> bool {
        matches!(self, AIActionStatus::Preprocessing)
    }

    pub fn is_queued(&self) -> bool {
        matches!(self, AIActionStatus::Queued)
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, AIActionStatus::Blocked)
    }

    pub fn is_done(&self) -> bool {
        matches!(self, AIActionStatus::Finished(..))
    }

    pub fn is_running(&self) -> bool {
        matches!(self, AIActionStatus::RunningAsync)
    }

    pub fn is_success(&self) -> bool {
        let AIActionStatus::Finished(result) = self else {
            return false;
        };
        result.result.is_successful()
    }

    pub fn is_failed(&self) -> bool {
        let AIActionStatus::Finished(result) = self else {
            return false;
        };
        result.result.is_failed()
    }

    pub fn is_cancelled(&self) -> bool {
        let AIActionStatus::Finished(result) = self else {
            return false;
        };
        result.result.is_cancelled()
    }

    pub fn is_cancelled_during_requested_command_execution(&self) -> bool {
        let AIActionStatus::Finished(result) = self else {
            return false;
        };
        result
            .result
            .is_cancelled_during_requested_command_execution()
    }

    pub fn finished_result(&self) -> Option<&AIAgentActionResult> {
        let AIActionStatus::Finished(result) = self else {
            return None;
        };
        Some(result.as_ref())
    }
}

#[derive(Debug, Clone)]
struct RunningActions {
    /// The execution phase for this batch of actions.
    phase: RunningActionPhase,

    /// The specific action IDs still running within the phase.
    /// If the phase is serial, there is only at most one action in here.
    /// For parallel phases, there can be several action IDs present at once,
    /// or there can be 0 or 1 actions; actions are added and removed as
    /// they are produced and completed, respectively.
    action_ids: Vec<AIAgentActionId>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(super) enum ProviderActionQueueError {
    #[error("provider action set mismatch: expected {expected:?}, received {received:?}")]
    ActionSetMismatch {
        expected: Vec<String>,
        received: Vec<String>,
    },
    #[error("provider action '{call_id}' is already correlated to active work")]
    ExistingCorrelation { call_id: String },
}

type ProviderActionCorrelation = (
    (AIConversationId, AIAgentActionId),
    ProviderToolExecutionRef,
);

type ActionExecutionKey = (AIConversationId, AIAgentActionId);

#[derive(Default)]
struct NotReadyActionTracker {
    actions: HashSet<ActionExecutionKey>,
}

impl NotReadyActionTracker {
    fn should_retry(&self, conversation_id: AIConversationId, action_id: &AIAgentActionId) -> bool {
        self.actions.contains(&(conversation_id, action_id.clone()))
    }

    fn update_after_attempt(
        &mut self,
        conversation_id: AIConversationId,
        action_id: AIAgentActionId,
        reason: NotExecutedReason,
        initiator: ActionExecutionInitiator,
    ) {
        let key = (conversation_id, action_id);
        match (reason, initiator) {
            (NotExecutedReason::NotReady, ActionExecutionInitiator::Automatic) => {
                self.actions.insert(key);
            }
            (NotExecutedReason::NotReady, ActionExecutionInitiator::User)
            | (NotExecutedReason::NeedsConfirmation | NotExecutedReason::WaitingOnSharer, _) => {
                self.actions.remove(&key);
            }
        }
    }

    fn clear(&mut self, conversation_id: AIConversationId, action_id: &AIAgentActionId) {
        self.actions.remove(&(conversation_id, action_id.clone()));
    }
}

#[derive(Clone, Copy)]
enum ActionExecutionInitiator {
    Automatic,
    User,
}

impl ActionExecutionInitiator {
    fn is_user_initiated(self) -> bool {
        matches!(self, Self::User)
    }
}

impl RunningActions {
    fn new(phase: RunningActionPhase, action_id: AIAgentActionId) -> Self {
        Self {
            phase,
            action_ids: vec![action_id],
        }
    }

    fn add_action(&mut self, action_id: AIAgentActionId) {
        self.action_ids.push(action_id);
    }

    fn remove_action(&mut self, action_id: &AIAgentActionId) {
        self.action_ids.retain(|id| id != action_id);
    }

    fn contains(&self, action_id: &AIAgentActionId) -> bool {
        self.action_ids.iter().any(|id| id == action_id)
    }

    fn first_action_id(&self) -> Option<&AIAgentActionId> {
        self.action_ids.first()
    }

    fn is_empty(&self) -> bool {
        self.action_ids.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartedAction {
    Sync,
    Async { phase: RunningActionPhase },
}

impl StartedAction {
    fn allows_compatible_following_actions(self) -> bool {
        matches!(
            self,
            Self::Sync
                | Self::Async {
                    phase: RunningActionPhase::Parallel(_)
                }
        )
    }
}

/// Returns whether another action may join the currently running phase.
///
/// Parallel phases only admit additional actions that classify into the same group and
/// can still be auto-executed. Serial phases always act as a barrier.
fn can_start_action_with_current_phase(
    current_phase: RunningActionPhase,
    next_phase: RunningActionPhase,
    can_autoexecute: bool,
) -> bool {
    match current_phase {
        RunningActionPhase::Serial => false,
        RunningActionPhase::Parallel(group) => {
            next_phase == RunningActionPhase::Parallel(group) && can_autoexecute
        }
    }
}

fn permission_request_id(action_id: &AIAgentActionId) -> String {
    format!("permission:{action_id}")
}

fn permission_denied_tool_event(action: &AIAgentAction) -> ToolEvent {
    ToolEvent::PermissionResolved {
        request_id: permission_request_id(&action.id),
        call_id: action.id.to_string(),
        decision: PermissionDecision::Denied {
            reason: Some("Permission denied by the user.".to_string()),
        },
    }
}

fn should_emit_tool_completion(is_provider_owned: bool, permission_denied: bool) -> bool {
    !is_provider_owned || !permission_denied
}

fn is_permission_denial(reason: CancellationReason, status: Option<&AIActionStatus>) -> bool {
    matches!(reason, CancellationReason::ManuallyCancelled)
        && matches!(status, Some(AIActionStatus::Blocked))
}

fn permission_kind_for_action(action: &AIAgentActionType) -> PermissionKind {
    match action {
        AIAgentActionType::ReadFiles(_)
        | AIAgentActionType::SearchCodebase(_)
        | AIAgentActionType::Grep { .. }
        | AIAgentActionType::FileGlob { .. }
        | AIAgentActionType::FileGlobV2 { .. }
        | AIAgentActionType::ReadMCPResource { .. }
        | AIAgentActionType::ReadDocuments(_)
        | AIAgentActionType::ReadSkill(_)
        | AIAgentActionType::FetchConversation { .. }
        | AIAgentActionType::WaitForEvents { .. } => PermissionKind::Read,
        AIAgentActionType::RequestFileEdits { .. }
        | AIAgentActionType::EditDocuments(_)
        | AIAgentActionType::CreateDocuments(_)
        | AIAgentActionType::InitProject
        | AIAgentActionType::InsertCodeReviewComments { .. } => PermissionKind::Write,
        AIAgentActionType::RequestCommandOutput { .. }
        | AIAgentActionType::WriteToLongRunningShellCommand { .. }
        | AIAgentActionType::ReadShellCommandOutput { .. }
        | AIAgentActionType::UseComputer(_)
        | AIAgentActionType::RequestComputerUse(_)
        | AIAgentActionType::TransferShellCommandControlToUser { .. }
        | AIAgentActionType::OpenCodeReview => PermissionKind::Execute,
        AIAgentActionType::UploadArtifact(_) => PermissionKind::Network,
        AIAgentActionType::CallMCPTool { .. }
        | AIAgentActionType::SuggestNewConversation { .. }
        | AIAgentActionType::SuggestPrompt(_)
        | AIAgentActionType::StartAgent { .. }
        | AIAgentActionType::SendMessageToAgent { .. }
        | AIAgentActionType::AskUserQuestion { .. }
        | AIAgentActionType::RunAgents(_) => PermissionKind::ExternalTool,
    }
}

fn sort_action_results_by_order(
    results: &mut [Arc<AIAgentActionResult>],
    action_order: &HashMap<AIAgentActionId, usize>,
) {
    results.sort_by_key(|result| action_order.get(&result.id).copied().unwrap_or(usize::MAX));
}

fn action_result_for_conversation<'a>(
    finished_action_results: &'a HashMap<AIConversationId, Vec<Arc<AIAgentActionResult>>>,
    provider_finished_action_results: &'a HashMap<
        (AIConversationId, ExternalWorkId),
        Vec<Arc<AIAgentActionResult>>,
    >,
    past_action_results: &'a HashMap<(AIConversationId, AIAgentActionId), Arc<AIAgentActionResult>>,
    conversation_id: AIConversationId,
    action_id: &AIAgentActionId,
) -> Option<&'a Arc<AIAgentActionResult>> {
    finished_action_results
        .get(&conversation_id)
        .into_iter()
        .chain(provider_finished_action_results.iter().filter_map(
            |((result_conversation_id, _), results)| {
                (*result_conversation_id == conversation_id).then_some(results)
            },
        ))
        .flat_map(|results| results.iter())
        .find(|result| &result.id == action_id)
        .or_else(|| past_action_results.get(&(conversation_id, action_id.clone())))
}

fn pending_action_status(
    pending_actions: &HashMap<AIConversationId, VecDeque<AIAgentAction>>,
    running_actions: &HashMap<AIConversationId, RunningActions>,
    conversation_id: AIConversationId,
    action_id: &AIAgentActionId,
    is_view_only: bool,
) -> Option<AIActionStatus> {
    let actions = pending_actions.get(&conversation_id)?;
    let index = actions.iter().position(|action| &action.id == action_id)?;
    if index == 0 && !is_view_only && !running_actions.contains_key(&conversation_id) {
        Some(AIActionStatus::Blocked)
    } else {
        Some(AIActionStatus::Queued)
    }
}

fn domain_tool_result(action_result: &AIAgentActionResult, permission_denied: bool) -> ToolResult {
    let status = if permission_denied {
        ToolResultStatus::Denied
    } else if action_result.result.is_cancelled() {
        ToolResultStatus::Cancelled
    } else if action_result.result.is_failed() {
        ToolResultStatus::Error
    } else {
        ToolResultStatus::Success
    };
    let content = action_result.result.model_content();
    let content = if permission_denied {
        format!("Permission denied by the user. {content}")
    } else {
        content
    };
    ToolResult {
        call_id: action_result.id.to_string(),
        content,
        status,
    }
}

#[cfg(not(target_family = "wasm"))]
fn action_tool_name(action: &AIAgentAction) -> String {
    action
        .tool_name
        .clone()
        .unwrap_or_else(|| format!("{:?}", AIAgentActionTypeDiscriminants::from(&action.action)))
}

#[cfg(not(target_family = "wasm"))]
fn action_result_type_name(result: &AIAgentActionResultType) -> &'static str {
    match result {
        AIAgentActionResultType::RequestCommandOutput(_) => "RequestCommandOutput",
        AIAgentActionResultType::WriteToLongRunningShellCommand(_) => {
            "WriteToLongRunningShellCommand"
        }
        AIAgentActionResultType::RequestFileEdits(_) => "RequestFileEdits",
        AIAgentActionResultType::ReadFiles(_) => "ReadFiles",
        AIAgentActionResultType::UploadArtifact(_) => "UploadArtifact",
        AIAgentActionResultType::SearchCodebase(_) => "SearchCodebase",
        AIAgentActionResultType::Grep(_) => "Grep",
        AIAgentActionResultType::FileGlob(_) => "FileGlob",
        AIAgentActionResultType::FileGlobV2(_) => "FileGlobV2",
        AIAgentActionResultType::ReadMCPResource(_) => "ReadMCPResource",
        AIAgentActionResultType::CallMCPTool(_) => "CallMCPTool",
        AIAgentActionResultType::ReadSkill(_) => "ReadSkill",
        AIAgentActionResultType::SuggestNewConversation(_) => "SuggestNewConversation",
        AIAgentActionResultType::SuggestPrompt(_) => "SuggestPrompt",
        AIAgentActionResultType::OpenCodeReview => "OpenCodeReview",
        AIAgentActionResultType::InsertReviewComments(_) => "InsertReviewComments",
        AIAgentActionResultType::InitProject => "InitProject",
        AIAgentActionResultType::ReadDocuments(_) => "ReadDocuments",
        AIAgentActionResultType::EditDocuments(_) => "EditDocuments",
        AIAgentActionResultType::CreateDocuments(_) => "CreateDocuments",
        AIAgentActionResultType::ReadShellCommandOutput(_) => "ReadShellCommandOutput",
        AIAgentActionResultType::UseComputer(_) => "UseComputer",
        AIAgentActionResultType::RequestComputerUse(_) => "RequestComputerUse",
        AIAgentActionResultType::FetchConversation(_) => "FetchConversation",
        AIAgentActionResultType::StartAgent(_) => "StartAgent",
        AIAgentActionResultType::SendMessageToAgent(_) => "SendMessageToAgent",
        AIAgentActionResultType::TransferShellCommandControlToUser(_) => {
            "TransferShellCommandControlToUser"
        }
        AIAgentActionResultType::AskUserQuestion(_) => "AskUserQuestion",
        AIAgentActionResultType::RunAgents(_) => "RunAgents",
        AIAgentActionResultType::WaitForEvents(_) => "WaitForEvents",
    }
}

#[cfg(not(target_family = "wasm"))]
fn action_result_status(result: &AIAgentActionResultType) -> &'static str {
    if action_result_is_success_for_remote_log(result) {
        "success"
    } else if result.is_failed() || action_result_failure_summary(result).is_some() {
        "error"
    } else if result.is_cancelled() {
        "cancelled"
    } else {
        "unknown"
    }
}

#[cfg(not(target_family = "wasm"))]
fn action_result_is_success_for_remote_log(result: &AIAgentActionResultType) -> bool {
    if result.is_successful() {
        return true;
    }

    match result {
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Snapshot { .. },
        ) => true,
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::CommandFinished { exit_code, .. },
        ) => exit_code.was_successful(),
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Cancelled
            | WriteToLongRunningShellCommandResult::Error(_),
        )
        | AIAgentActionResultType::RequestCommandOutput(_)
        | AIAgentActionResultType::RequestFileEdits(_)
        | AIAgentActionResultType::ReadFiles(_)
        | AIAgentActionResultType::UploadArtifact(_)
        | AIAgentActionResultType::SearchCodebase(_)
        | AIAgentActionResultType::Grep(_)
        | AIAgentActionResultType::FileGlob(_)
        | AIAgentActionResultType::FileGlobV2(_)
        | AIAgentActionResultType::ReadMCPResource(_)
        | AIAgentActionResultType::CallMCPTool(_)
        | AIAgentActionResultType::ReadSkill(_)
        | AIAgentActionResultType::SuggestNewConversation(_)
        | AIAgentActionResultType::SuggestPrompt(_)
        | AIAgentActionResultType::OpenCodeReview
        | AIAgentActionResultType::InsertReviewComments(_)
        | AIAgentActionResultType::InitProject
        | AIAgentActionResultType::ReadDocuments(_)
        | AIAgentActionResultType::EditDocuments(_)
        | AIAgentActionResultType::CreateDocuments(_)
        | AIAgentActionResultType::ReadShellCommandOutput(_)
        | AIAgentActionResultType::UseComputer(_)
        | AIAgentActionResultType::RequestComputerUse(_)
        | AIAgentActionResultType::FetchConversation(_)
        | AIAgentActionResultType::StartAgent(_)
        | AIAgentActionResultType::SendMessageToAgent(_)
        | AIAgentActionResultType::TransferShellCommandControlToUser(_)
        | AIAgentActionResultType::AskUserQuestion(_)
        | AIAgentActionResultType::RunAgents(_)
        | AIAgentActionResultType::WaitForEvents(_) => false,
    }
}

#[cfg(not(target_family = "wasm"))]
fn action_result_log_level(result: &AIAgentActionResultType) -> RemoteLogLevel {
    if result.is_failed() || action_result_failure_summary(result).is_some() {
        RemoteLogLevel::Error
    } else if result.is_cancelled() {
        RemoteLogLevel::Warn
    } else {
        RemoteLogLevel::Info
    }
}

#[cfg(not(target_family = "wasm"))]
fn action_result_error_summary(result: &AIAgentActionResultType) -> Option<String> {
    if let Some(summary) = action_result_failure_summary(result) {
        Some(remote_logging::sanitize_error(summary))
    } else if result.is_cancelled() {
        Some(format!("{} cancelled", action_result_type_name(result)))
    } else {
        None
    }
}

#[cfg(not(target_family = "wasm"))]
fn action_result_failure_summary(result: &AIAgentActionResultType) -> Option<String> {
    match result {
        AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Completed {
            exit_code,
            ..
        }) if !exit_code.was_successful() => {
            Some(format!("command exited with code {}", exit_code.value()))
        }
        AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Denylisted {
            ..
        }) => Some("command was denylisted".to_string()),
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::CommandFinished { exit_code, .. },
        ) if !exit_code.was_successful() => Some(format!(
            "long-running shell command exited with code {}",
            exit_code.value()
        )),
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Error(error),
        ) => Some(format!("{error:?}")),
        AIAgentActionResultType::RequestFileEdits(
            RequestFileEditsResult::DiffApplicationFailed { error },
        ) => Some(error.clone()),
        AIAgentActionResultType::ReadFiles(ReadFilesResult::Error(error))
        | AIAgentActionResultType::UploadArtifact(UploadArtifactResult::Error(error))
        | AIAgentActionResultType::Grep(GrepResult::Error(error))
        | AIAgentActionResultType::FileGlob(FileGlobResult::Error(error))
        | AIAgentActionResultType::FileGlobV2(FileGlobV2Result::Error(error))
        | AIAgentActionResultType::ReadMCPResource(ReadMCPResourceResult::Error(error))
        | AIAgentActionResultType::CallMCPTool(CallMCPToolResult::Error(error))
        | AIAgentActionResultType::ReadSkill(ReadSkillResult::Error(error))
        | AIAgentActionResultType::ReadDocuments(ReadDocumentsResult::Error(error))
        | AIAgentActionResultType::EditDocuments(EditDocumentsResult::Error(error))
        | AIAgentActionResultType::CreateDocuments(CreateDocumentsResult::Error(error))
        | AIAgentActionResultType::UseComputer(UseComputerResult::Error(error))
        | AIAgentActionResultType::RequestComputerUse(RequestComputerUseResult::Error(error))
        | AIAgentActionResultType::FetchConversation(FetchConversationResult::Error(error))
        | AIAgentActionResultType::SendMessageToAgent(SendMessageToAgentResult::Error(error))
        | AIAgentActionResultType::AskUserQuestion(AskUserQuestionResult::Error(error)) => {
            Some(error.clone())
        }
        AIAgentActionResultType::SearchCodebase(SearchCodebaseResult::Failed {
            reason,
            message,
        }) => Some(format!("{reason:?}: {message}")),
        AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::CommandFinished { exit_code, .. },
        ) if !exit_code.was_successful() => Some(format!(
            "shell command output exited with code {}",
            exit_code.value()
        )),
        AIAgentActionResultType::ReadShellCommandOutput(ReadShellCommandOutputResult::Error(
            error,
        )) => Some(format!("{error:?}")),
        AIAgentActionResultType::InsertReviewComments(InsertReviewCommentsResult::Error {
            message,
            ..
        }) => Some(message.clone()),
        AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, .. }) => {
            Some(error.clone())
        }
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::CommandFinished { exit_code, .. },
        ) if !exit_code.was_successful() => Some(format!(
            "transferred shell command exited with code {}",
            exit_code.value()
        )),
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Error(error),
        ) => Some(format!("{error:?}")),
        AIAgentActionResultType::RunAgents(RunAgentsResult::Denied { reason }) => {
            Some(reason.clone())
        }
        AIAgentActionResultType::RunAgents(RunAgentsResult::Failure { error }) => {
            Some(error.clone())
        }
        AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::ExecutionError { message, .. },
        ) => Some(message.clone()),
        AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::Completed { .. }
            | RequestCommandOutputResult::CancelledBeforeExecution
            | RequestCommandOutputResult::LongRunningCommandSnapshot { .. },
        )
        | AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Cancelled
            | WriteToLongRunningShellCommandResult::CommandFinished { .. }
            | WriteToLongRunningShellCommandResult::Snapshot { .. },
        )
        | AIAgentActionResultType::RequestFileEdits(
            RequestFileEditsResult::Cancelled | RequestFileEditsResult::Success { .. },
        )
        | AIAgentActionResultType::ReadFiles(
            ReadFilesResult::Success { .. } | ReadFilesResult::Cancelled,
        )
        | AIAgentActionResultType::UploadArtifact(
            UploadArtifactResult::Success { .. } | UploadArtifactResult::Cancelled,
        )
        | AIAgentActionResultType::SearchCodebase(
            SearchCodebaseResult::Success { .. } | SearchCodebaseResult::Cancelled,
        )
        | AIAgentActionResultType::Grep(GrepResult::Success { .. } | GrepResult::Cancelled)
        | AIAgentActionResultType::FileGlob(
            FileGlobResult::Success { .. } | FileGlobResult::Cancelled,
        )
        | AIAgentActionResultType::FileGlobV2(
            FileGlobV2Result::Success { .. } | FileGlobV2Result::Cancelled,
        )
        | AIAgentActionResultType::ReadMCPResource(
            ReadMCPResourceResult::Success { .. } | ReadMCPResourceResult::Cancelled,
        )
        | AIAgentActionResultType::CallMCPTool(
            CallMCPToolResult::Success { .. } | CallMCPToolResult::Cancelled,
        )
        | AIAgentActionResultType::ReadSkill(
            ReadSkillResult::Success { .. } | ReadSkillResult::Cancelled,
        )
        | AIAgentActionResultType::SuggestNewConversation(_)
        | AIAgentActionResultType::SuggestPrompt(_)
        | AIAgentActionResultType::OpenCodeReview
        | AIAgentActionResultType::InsertReviewComments(
            InsertReviewCommentsResult::Success { .. } | InsertReviewCommentsResult::Cancelled,
        )
        | AIAgentActionResultType::InitProject
        | AIAgentActionResultType::ReadDocuments(
            ReadDocumentsResult::Success { .. } | ReadDocumentsResult::Cancelled,
        )
        | AIAgentActionResultType::EditDocuments(
            EditDocumentsResult::Success { .. } | EditDocumentsResult::Cancelled,
        )
        | AIAgentActionResultType::CreateDocuments(
            CreateDocumentsResult::Success { .. } | CreateDocumentsResult::Cancelled,
        )
        | AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::Cancelled
            | ReadShellCommandOutputResult::CommandFinished { .. }
            | ReadShellCommandOutputResult::LongRunningCommandSnapshot { .. },
        )
        | AIAgentActionResultType::UseComputer(
            UseComputerResult::Success(_) | UseComputerResult::Cancelled,
        )
        | AIAgentActionResultType::RequestComputerUse(
            RequestComputerUseResult::Approved { .. } | RequestComputerUseResult::Cancelled,
        )
        | AIAgentActionResultType::FetchConversation(
            FetchConversationResult::Success { .. } | FetchConversationResult::Cancelled,
        )
        | AIAgentActionResultType::StartAgent(
            StartAgentResult::Success { .. } | StartAgentResult::Cancelled { .. },
        )
        | AIAgentActionResultType::SendMessageToAgent(
            SendMessageToAgentResult::Success { .. } | SendMessageToAgentResult::Cancelled,
        )
        | AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Cancelled
            | TransferShellCommandControlToUserResult::CommandFinished { .. }
            | TransferShellCommandControlToUserResult::Snapshot { .. },
        )
        | AIAgentActionResultType::AskUserQuestion(
            AskUserQuestionResult::Success { .. }
            | AskUserQuestionResult::Cancelled
            | AskUserQuestionResult::SkippedByAutoApprove { .. },
        )
        | AIAgentActionResultType::RunAgents(
            RunAgentsResult::Launched { .. } | RunAgentsResult::Cancelled,
        )
        | AIAgentActionResultType::WaitForEvents(_) => None,
    }
}

#[cfg(not(target_family = "wasm"))]
fn log_tool_event(
    ctx: &mut ModelContext<BlocklistAIActionModel>,
    level: RemoteLogLevel,
    message: &str,
    context: serde_json::Value,
) {
    remote_logging::log_model_event(
        ctx,
        RemoteLogRecord {
            level,
            message: message.to_string(),
            context,
        },
    );
}

pub struct BlocklistAIActionModel {
    executor: ModelHandle<BlocklistAIActionExecutor>,

    pending_preprocessed_actions: HashMap<AIConversationId, PendingPreprocessedActions>,

    /// Map from conversation ID to queue of pending [`AIAgentAction`]s.
    pending_actions: HashMap<AIConversationId, VecDeque<AIAgentAction>>,

    /// Map from conversation ID to the currently running action phase, if any.
    running_actions: HashMap<AIConversationId, RunningActions>,

    /// Map from conversation ID to actions received in the most recent AI output that are finished.
    finished_action_results: HashMap<AIConversationId, Vec<Arc<AIAgentActionResult>>>,

    /// Provider-neutral results for the same finished actions. Rig consumes these directly rather
    /// than reconstructing them from the legacy request protobuf.
    finished_tool_results: HashMap<AIConversationId, Vec<ToolResult>>,

    /// Provider-owned action results retained until their exact tool batch is fully committed.
    provider_finished_action_results:
        HashMap<(AIConversationId, ExternalWorkId), Vec<Arc<AIAgentActionResult>>>,

    /// Original order for the current batch of actions.
    ///
    /// We maintain this so that even though we might process actions in parallel,
    /// we can still order the results consistently.
    action_order: HashMap<AIConversationId, HashMap<AIAgentActionId, usize>>,

    /// Permission-card rejections whose cancelled action result must not emit a second provider event.
    denied_permissions: HashSet<(AIConversationId, AIAgentActionId)>,
    pending_permissions: HashSet<(AIConversationId, AIAgentActionId)>,

    /// Actions parked because their executor-specific UI or state was not ready yet.
    not_ready_actions: NotReadyActionTracker,

    /// Provider preprocessing waits for execution order, since it can read file contents.
    /// The value is true while preprocessing is in flight.
    deferred_provider_preprocessing: HashMap<(AIConversationId, AIAgentActionId), bool>,

    /// Durable provider work identity for actions owned by an active provider run.
    provider_tool_executions:
        HashMap<(AIConversationId, AIAgentActionId), ProviderToolExecutionRef>,

    /// Past actions and their corresponding statuses from previous AI exchanges.
    past_action_results: HashMap<(AIConversationId, AIAgentActionId), Arc<AIAgentActionResult>>,

    /// The ID of the terminal view this controller is associated with.
    terminal_view_id: EntityId,

    /// In view-only mode, we never block on user acceptance and avoid any interactive controls.
    /// This is used for agent session sharing to avoid any tools blocking on the viewer's acceptance.
    is_view_only: bool,

    /// The ID of the ambient agent task which owns this action model, if any.
    ambient_agent_task_id: Option<crate::ai::ambient_agents::AmbientAgentTaskId>,
}

impl BlocklistAIActionModel {
    pub fn new(
        terminal_model: Arc<FairMutex<TerminalModel>>,
        active_session: ModelHandle<ActiveSession>,
        model_event_dispatcher: &ModelHandle<ModelEventDispatcher>,
        get_relevant_files_controller: ModelHandle<GetRelevantFilesController>,
        terminal_view_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        let executor = ctx.add_model(|ctx| {
            BlocklistAIActionExecutor::new(
                terminal_model,
                active_session.clone(),
                model_event_dispatcher,
                get_relevant_files_controller,
                terminal_view_id,
                ctx,
            )
        });
        ctx.subscribe_to_model(&executor, move |me, _, event, ctx| match event {
            BlocklistAIActionExecutorEvent::ExecutingAction {
                action_id,
                conversation_id,
            } => {
                let execution_ref = me.provider_tool_execution_ref(*conversation_id, action_id);
                ctx.emit(BlocklistAIActionEvent::ExecutingAction {
                    action_id: action_id.clone(),
                    conversation_id: *conversation_id,
                    execution_ref: execution_ref.clone(),
                });
                ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
                    action_id: action_id.clone(),
                    execution_ref,
                    event: ToolEvent::Started {
                        call_id: action_id.to_string(),
                    },
                });
            }
            BlocklistAIActionExecutorEvent::FinishedAction {
                result,
                conversation_id,
                cancellation_reason,
            } => {
                me.handle_action_result(*conversation_id, result.clone(), *cancellation_reason, ctx)
            }
            BlocklistAIActionExecutorEvent::InitProject(id) => {
                ctx.emit(BlocklistAIActionEvent::InitProject(id.clone()))
            }
            BlocklistAIActionExecutorEvent::OpenCodeReview(id) => {
                ctx.emit(BlocklistAIActionEvent::ToggleCodeReview(id.clone()))
            }
            BlocklistAIActionExecutorEvent::InsertCodeReviewComments {
                action_id,
                repo_path,
                comments,
                base_branch,
            } => {
                ctx.emit(BlocklistAIActionEvent::InsertCodeReviewComments {
                    action_id: action_id.clone(),
                    repo_path: repo_path.clone(),
                    comments: comments.clone(),
                    base_branch: base_branch.clone(),
                });
            }
        });

        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, |me, _, event, ctx| match event {
            BlocklistAIHistoryEvent::RemoveConversation {
                conversation_id, ..
            }
            | BlocklistAIHistoryEvent::DeletedConversation {
                conversation_id, ..
            } => {
                me.cleanup_conversation_state(*conversation_id, ctx);
            }
            BlocklistAIHistoryEvent::ClearedConversationsForTerminalSurface {
                cleared_conversation_ids,
                ..
            } => {
                for conversation_id in cleared_conversation_ids {
                    me.cleanup_conversation_state(*conversation_id, ctx);
                }
            }
            _ => {}
        });

        Self {
            pending_actions: Default::default(),
            finished_action_results: Default::default(),
            finished_tool_results: Default::default(),
            provider_finished_action_results: Default::default(),
            executor,
            past_action_results: HashMap::new(),
            running_actions: Default::default(),
            action_order: Default::default(),
            denied_permissions: Default::default(),
            pending_permissions: Default::default(),
            not_ready_actions: Default::default(),
            deferred_provider_preprocessing: Default::default(),
            provider_tool_executions: Default::default(),
            terminal_view_id,
            pending_preprocessed_actions: Default::default(),
            is_view_only: false,
            ambient_agent_task_id: None,
        }
    }

    /// Enable or disable view-only mode (for use in agent session sharing).
    pub fn set_view_only(&mut self, is_view_only: bool) {
        self.is_view_only = is_view_only;
    }

    /// Marks an action as remotely executing on the viewer side.
    /// This is called when a viewer receives a CommandExecutionStarted event from the sharer,
    /// allowing the viewer's UI to show the action as running even though it's not executing locally.
    pub fn mark_action_as_remotely_executing(
        &mut self,
        action_id: &AIAgentActionId,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        // Only applicable for viewers
        if !self.is_view_only {
            return;
        }

        // Remove the action from pending_actions for the specific conversation
        // so that we can correctly show the command as running.
        if let Some(pending_actions) = self.pending_actions.get_mut(&conversation_id) {
            pending_actions.retain(|a| &a.id != action_id);
        }

        self.add_running_action(
            conversation_id,
            action_id.clone(),
            RunningActionPhase::Serial,
        );
        ctx.emit(BlocklistAIActionEvent::ExecutingAction {
            action_id: action_id.clone(),
            conversation_id,
            execution_ref: self.provider_tool_execution_ref(conversation_id, action_id),
        });
    }

    /// Returns true if the action model is operating in view-only mode (used for shared-session viewers).
    pub fn is_view_only(&self) -> bool {
        self.is_view_only
    }

    pub fn shell_command_executor(&self, app: &AppContext) -> ModelHandle<ShellCommandExecutor> {
        self.executor.as_ref(app).shell_command_executor().clone()
    }

    pub fn suggest_new_conversation_executor(
        &self,
        app: &AppContext,
    ) -> ModelHandle<SuggestNewConversationExecutor> {
        self.executor
            .as_ref(app)
            .suggest_new_conversation_executor()
            .clone()
    }

    pub fn request_file_edits_executor(
        &self,
        app: &AppContext,
    ) -> ModelHandle<RequestFileEditsExecutor> {
        self.executor
            .as_ref(app)
            .request_file_edits_executor()
            .clone()
    }

    pub fn search_codebase_executor<'a>(
        &'a self,
        app: &'a AppContext,
    ) -> &'a ModelHandle<SearchCodebaseExecutor> {
        self.executor.as_ref(app).search_codebase_executor()
    }

    pub fn suggest_prompt_executor(
        &self,
        app: &AppContext,
    ) -> ModelHandle<PromptSuggestionExecutor> {
        self.executor.as_ref(app).suggest_prompt_executor().clone()
    }

    pub fn start_agent_executor(&self, app: &AppContext) -> ModelHandle<StartAgentExecutor> {
        self.executor.as_ref(app).start_agent_executor().clone()
    }

    pub fn run_agents_executor(&self, app: &AppContext) -> ModelHandle<RunAgentsExecutor> {
        self.executor.as_ref(app).run_agents_executor().clone()
    }

    pub fn ask_user_question_executor(
        &self,
        app: &AppContext,
    ) -> ModelHandle<AskUserQuestionExecutor> {
        self.executor
            .as_ref(app)
            .ask_user_question_executor()
            .clone()
    }

    pub fn set_ambient_agent_task_id(
        &mut self,
        id: Option<crate::ai::ambient_agents::AmbientAgentTaskId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.ambient_agent_task_id = id;
        self.executor.update(ctx, |executor, ctx| {
            executor.set_ambient_agent_task_id(id, ctx);
        });
    }

    fn blocked_action_for_conversation(
        &self,
        conversation_id: &AIConversationId,
    ) -> Option<&AIAgentAction> {
        if self.running_actions.contains_key(conversation_id) {
            return None;
        }

        self.pending_actions
            .get(conversation_id)
            .and_then(|queue| queue.front())
    }

    fn action_execution_phase(
        &self,
        conversation_id: AIConversationId,
    ) -> Option<RunningActionPhase> {
        self.running_actions
            .get(&conversation_id)
            .map(|running| running.phase)
    }

    fn add_running_action(
        &mut self,
        conversation_id: AIConversationId,
        action_id: AIAgentActionId,
        phase: RunningActionPhase,
    ) {
        match self.running_actions.entry(conversation_id) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                debug_assert_eq!(entry.get().phase, phase);
                entry.get_mut().add_action(action_id);
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(RunningActions::new(phase, action_id));
            }
        }
    }

    fn try_to_execute_available_actions(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let pending_count = self
            .pending_actions
            .get(&conversation_id)
            .map(|q| q.len())
            .unwrap_or(0);
        crate::ai::tool_diagnostics::tool_debug!(
            "try_to_execute_available_actions: conversation={:?}, pending_count={}",
            conversation_id,
            pending_count
        );
        loop {
            let Some(front_action) = self
                .pending_actions
                .get(&conversation_id)
                .and_then(|queue| queue.front())
                .cloned()
            else {
                crate::ai::tool_diagnostics::tool_debug!(
                    "try_to_execute_available_actions: no more pending actions"
                );
                return;
            };

            crate::ai::tool_diagnostics::tool_debug!(
                "try_to_execute_available_actions: trying action id={:?}, type={:?}",
                front_action.id,
                std::mem::discriminant(&front_action.action)
            );

            if let Some(current_phase) = self.action_execution_phase(conversation_id) {
                if !self.can_start_action_in_current_phase(
                    &front_action,
                    conversation_id,
                    current_phase,
                    ctx,
                ) {
                    crate::ai::tool_diagnostics::tool_debug!(
                        "try_to_execute_available_actions: cannot start in current phase {:?}",
                        current_phase
                    );
                    return;
                }
            }

            let Some(result) = self.start_pending_action_by_id(
                &front_action.id,
                conversation_id,
                ActionExecutionInitiator::Automatic,
                ctx,
            ) else {
                crate::ai::tool_diagnostics::tool_debug!("try_to_execute_available_actions: start_pending_action_by_id returned None (blocked)");
                return;
            };

            crate::ai::tool_diagnostics::tool_debug!(
                "try_to_execute_available_actions: action started, result={:?}",
                std::mem::discriminant(&result)
            );

            if matches!(
                result,
                StartedAction::Async {
                    phase: RunningActionPhase::Serial
                }
            ) {
                crate::ai::tool_diagnostics::tool_debug!(
                    "try_to_execute_available_actions: serial async action, stopping loop"
                );
                return;
            }
        }
    }

    fn sort_finished_results(&mut self, conversation_id: AIConversationId) {
        if let Some(action_order) = self.action_order.get(&conversation_id) {
            if let Some(finished_results) = self.finished_action_results.get_mut(&conversation_id) {
                sort_action_results_by_order(finished_results, action_order);
            }
            for ((finished_conversation_id, _), finished_results) in
                &mut self.provider_finished_action_results
            {
                if *finished_conversation_id == conversation_id {
                    sort_action_results_by_order(finished_results, action_order);
                }
            }
            if let Some(tool_results) = self.finished_tool_results.get_mut(&conversation_id) {
                let tool_order = action_order
                    .iter()
                    .map(|(id, index)| (id.to_string(), *index))
                    .collect::<HashMap<_, _>>();
                tool_results.sort_by_key(|result| {
                    tool_order
                        .get(&result.call_id)
                        .copied()
                        .unwrap_or(usize::MAX)
                });
            }
        }
    }

    /// Returns all pending actions for a specific conversation.
    pub fn get_pending_actions_for_conversation(
        &self,
        conversation_id: &AIConversationId,
    ) -> impl Iterator<Item = &AIAgentAction> {
        self.pending_actions
            .get(conversation_id)
            .into_iter()
            .flat_map(|queue| queue.iter())
    }

    /// Returns the next pending action
    pub fn get_pending_action(&self, app: &AppContext) -> Option<&AIAgentAction> {
        let conversation_id = self.active_conversation_id(app)?;
        self.blocked_action_for_conversation(&conversation_id)
    }

    /// Returns a pending action by its ID within the given conversation.
    pub fn get_pending_action_by_id(
        &self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> Option<&AIAgentAction> {
        self.pending_actions
            .get(&conversation_id)?
            .iter()
            .find(|action| &action.id == action_id)
    }

    /// Returns the next pending or running action ID, for the active conversation, if any.
    pub fn get_pending_or_running_action_id<'a>(
        &'a self,
        app: &'a AppContext,
    ) -> Option<&'a AIAgentActionId> {
        let conversation_id = self.active_conversation_id(app)?;
        self.blocked_action_for_conversation(&conversation_id)
            .map(|action| &action.id)
            .or_else(|| {
                self.running_actions
                    .get(&conversation_id)
                    .and_then(RunningActions::first_action_id)
            })
    }

    /// Returns one of the currently asynchronously-executing actions, if any.
    ///
    /// When multiple actions run in parallel, only the first is returned. This is
    /// sufficient for callers that need a single status indicator (e.g., "Searching
    /// codebase...") or just need to know whether *something* is running.
    pub fn get_async_running_action<'a>(
        &'a self,
        app: &'a AppContext,
    ) -> Option<&'a AIAgentAction> {
        let conversation_id = self.active_conversation_id(app)?;
        self.running_actions
            .get(&conversation_id)
            .and_then(RunningActions::first_action_id)
            .and_then(|action_id| {
                self.executor
                    .as_ref(app)
                    .async_executing_action(conversation_id, action_id)
            })
    }

    /// Returns whether there is a pending or running action for the active conversation.
    pub fn has_unfinished_actions(&self, app: &AppContext) -> bool {
        let Some(conversation_id) = self.active_conversation_id(app) else {
            return false;
        };
        self.has_unfinished_actions_for_conversation(conversation_id)
    }

    pub fn has_unfinished_actions_for_conversation(
        &self,
        conversation_id: AIConversationId,
    ) -> bool {
        let has_pending = self
            .pending_actions
            .get(&conversation_id)
            .is_some_and(|queue| !queue.is_empty());
        let has_running = self
            .running_actions
            .get(&conversation_id)
            .is_some_and(|running| !running.is_empty());
        has_pending || has_running
    }

    pub fn has_unresolved_ask_user_question_for_conversation(
        &self,
        conversation_id: AIConversationId,
        app: &AppContext,
    ) -> bool {
        self.pending_actions
            .get(&conversation_id)
            .is_some_and(|queue| {
                queue.iter().any(|action| {
                    matches!(action.action, AIAgentActionType::AskUserQuestion { .. })
                })
            })
            || self
                .executor
                .as_ref(app)
                .has_running_ask_user_question(conversation_id)
    }

    /// Returns finished action results received from the most recent AI output for the active conversation.
    pub fn get_finished_action_results(
        &self,
        conversation_id: AIConversationId,
    ) -> Option<&Vec<Arc<AIAgentActionResult>>> {
        self.finished_action_results.get(&conversation_id)
    }

    /// Returns the status for an action within the given conversation.
    pub fn get_action_status(
        &self,
        conversation_id: AIConversationId,
        id: &AIAgentActionId,
    ) -> Option<AIActionStatus> {
        if self
            .deferred_provider_preprocessing
            .get(&(conversation_id, id.clone()))
            == Some(&true)
        {
            return Some(AIActionStatus::Preprocessing);
        }
        if let Some(status) = pending_action_status(
            &self.pending_actions,
            &self.running_actions,
            conversation_id,
            id,
            self.is_view_only,
        ) {
            return Some(status);
        }

        self.running_actions
            .get(&conversation_id)
            .filter(|running| running.contains(id))
            .map(|_| AIActionStatus::RunningAsync)
            .or_else(|| {
                self.get_action_result(conversation_id, id)
                    .map(|result| AIActionStatus::Finished(result.clone()))
            })
            .or_else(|| {
                self.pending_preprocessed_actions
                    .get(&conversation_id)
                    .is_some_and(|preprocessing| preprocessing.contains(id))
                    .then_some(AIActionStatus::Preprocessing)
            })
    }

    pub fn get_action_result(
        &self,
        conversation_id: AIConversationId,
        id: &AIAgentActionId,
    ) -> Option<&Arc<AIAgentActionResult>> {
        action_result_for_conversation(
            &self.finished_action_results,
            &self.provider_finished_action_results,
            &self.past_action_results,
            conversation_id,
            id,
        )
    }

    /// Bulk restore action results from a list of exchanges (used when loading conversations from tasks)
    pub fn restore_action_results_from_exchanges(
        &mut self,
        conversation_id: AIConversationId,
        exchanges: Vec<&AIAgentExchange>,
    ) {
        for exchange in exchanges.iter() {
            for input in &exchange.input {
                if let AIAgentInput::ActionResult { result, .. } = input {
                    let result_id = result.id.clone();
                    let mut result_to_insert = result.clone();
                    if let AIAgentActionResultType::RequestCommandOutput(
                        RequestCommandOutputResult::LongRunningCommandSnapshot { .. },
                    ) = &result.result
                    {
                        // On restoration we set long running command snapshot results to cancelled,
                        // since this means the command was incomplete when the app was closed.
                        result_to_insert.result = AIAgentActionResultType::RequestCommandOutput(
                            RequestCommandOutputResult::CancelledBeforeExecution,
                        );
                    }
                    self.past_action_results
                        .insert((conversation_id, result_id), Arc::new(result_to_insert));
                }
            }
        }
    }

    /// Dispatches a `RunAgents` action with the user-edited request
    /// from the confirmation card.
    pub fn execute_run_agents(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
        request: ai::agent::action::RunAgentsRequest,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(action) = self
            .pending_actions
            .get_mut(&conversation_id)
            .and_then(|queue| queue.iter_mut().find(|action| &action.id == action_id))
        else {
            log::warn!(
                "BlocklistAIActionModel::execute_run_agents: no pending action for {action_id:?}"
            );
            return;
        };
        if !matches!(action.action, AIAgentActionType::RunAgents(_)) {
            log::warn!(
                "BlocklistAIActionModel::execute_run_agents: pending action {action_id:?} is not RunAgents"
            );
            return;
        }
        action.action = AIAgentActionType::RunAgents(request);
        self.execute_action(action_id, conversation_id, ctx);
    }

    /// Removes a pending `RunAgents` action and records a `Denied`
    /// result. Used when the orchestration config is disapproved at
    /// the time the action becomes blocked on user confirmation.
    pub fn deny_run_agents(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
        reason: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(action) = self
            .pending_actions
            .get_mut(&conversation_id)
            .and_then(|queue| {
                let index = queue.iter().position(|action| &action.id == action_id)?;
                queue.remove(index)
            })
        else {
            log::warn!(
                "BlocklistAIActionModel::deny_run_agents: no pending action for {action_id:?}"
            );
            return;
        };
        self.resolve_permission_denial(conversation_id, &action, ctx);
        let result = Arc::new(AIAgentActionResult {
            id: action.id,
            task_id: action.task_id,
            result: AIAgentActionResultType::RunAgents(
                ai::agent::action_result::RunAgentsResult::Denied { reason },
            ),
        });
        self.handle_action_result(conversation_id, result, None, ctx);
    }

    /// Attempts to execute the next pending action for the active conversation.
    pub fn execute_next_action_for_user(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(pending_action_id) = self
            .pending_actions
            .get(&conversation_id)
            .and_then(|queue| queue.front())
            .map(|action| action.id.clone())
        else {
            return;
        };

        if self
            .start_pending_action_by_id(
                &pending_action_id,
                conversation_id,
                ActionExecutionInitiator::User,
                ctx,
            )
            .is_some_and(StartedAction::allows_compatible_following_actions)
        {
            self.try_to_execute_available_actions(conversation_id, ctx);
        }
    }

    /// Attempts to execute the pending action with the given `action_id` for the given conversation.
    pub fn execute_action(
        &mut self,
        action_id: &AIAgentActionId,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .start_pending_action_by_id(
                action_id,
                conversation_id,
                ActionExecutionInitiator::User,
                ctx,
            )
            .is_some_and(StartedAction::allows_compatible_following_actions)
        {
            self.try_to_execute_available_actions(conversation_id, ctx);
        }
    }

    /// Retries an action only when a prior automatic attempt found its executor not ready.
    pub(super) fn retry_not_ready_action(
        &mut self,
        action_id: &AIAgentActionId,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self
            .not_ready_actions
            .should_retry(conversation_id, action_id)
        {
            return;
        }

        if self
            .start_pending_action_by_id(
                action_id,
                conversation_id,
                ActionExecutionInitiator::Automatic,
                ctx,
            )
            .is_some_and(|result| matches!(result, StartedAction::Sync))
        {
            self.try_to_execute_available_actions(conversation_id, ctx);
        }
    }

    /// Gets the active conversation ID for this terminal view.
    fn active_conversation_id(&self, app: &AppContext) -> Option<AIConversationId> {
        BlocklistAIHistoryModel::as_ref(app).active_conversation_id(self.terminal_view_id)
    }

    fn update_conversation_in_progress_status(
        &self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
            history_model.update_conversation_status(
                self.terminal_view_id,
                conversation_id,
                ConversationStatus::InProgress,
                ctx,
            );
        });
    }

    fn handle_not_executed_action(
        &mut self,
        action: &AIAgentAction,
        reason: NotExecutedReason,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        match reason {
            NotExecutedReason::NeedsConfirmation => {
                if !self
                    .pending_permissions
                    .insert((conversation_id, action.id.clone()))
                {
                    return;
                }
                #[cfg(not(target_family = "wasm"))]
                log_tool_event(
                    ctx,
                    RemoteLogLevel::Info,
                    "Tool permission requested",
                    serde_json::json!({
                        "event": "tool_permission_requested",
                        "conversation_id": conversation_id.to_string(),
                        "action_id": action.id.to_string(),
                        "task_id": action.task_id.to_string(),
                        "tool_name": action_tool_name(action),
                        "permission_kind": format!("{:?}", permission_kind_for_action(&action.action)),
                        "reason": format!("{reason:?}"),
                    }),
                );
                let execution_ref = self.provider_tool_execution_ref(conversation_id, &action.id);
                ctx.emit(BlocklistAIActionEvent::ActionBlockedOnUserConfirmation {
                    action_id: action.id.clone(),
                    conversation_id,
                    execution_ref: execution_ref.clone(),
                });
                ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
                    action_id: action.id.clone(),
                    execution_ref,
                    event: ToolEvent::PermissionRequested {
                        request: PermissionRequest {
                            id: permission_request_id(&action.id),
                            call_id: action.id.to_string(),
                            kind: permission_kind_for_action(&action.action),
                            reason: Some(action.action.user_friendly_name()),
                        },
                    },
                });
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    let blocked_action_user_friendly_str = action.action.user_friendly_name();
                    history_model.update_conversation_status(
                        self.terminal_view_id,
                        conversation_id,
                        ConversationStatus::Blocked {
                            blocked_action: format!("{blocked_action_user_friendly_str:?}"),
                        },
                        ctx,
                    );
                });
            }
            NotExecutedReason::NotReady => {
                #[cfg(not(target_family = "wasm"))]
                log_tool_event(
                    ctx,
                    RemoteLogLevel::Warn,
                    "Tool execution deferred",
                    serde_json::json!({
                        "event": "tool_execution_not_ready",
                        "conversation_id": conversation_id.to_string(),
                        "action_id": action.id.to_string(),
                        "task_id": action.task_id.to_string(),
                        "tool_name": action_tool_name(action),
                        "permission_kind": format!("{:?}", permission_kind_for_action(&action.action)),
                    }),
                );
            }
            NotExecutedReason::WaitingOnSharer => {}
        }
    }

    fn action_phase_for_action(
        &self,
        action: &AIAgentAction,
        ctx: &ModelContext<Self>,
    ) -> RunningActionPhase {
        self.executor.as_ref(ctx).action_phase(action, ctx)
    }

    fn can_start_action_in_current_phase(
        &self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        current_phase: RunningActionPhase,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        // Recompute the candidate action's phase on demand so executor-side capability checks
        // (for example, whether the active session can run shell commands in parallel) are applied
        // using the latest runtime state.
        let next_phase = self.action_phase_for_action(action, ctx);
        let can_autoexecute = self.executor.update(ctx, |executor, ctx| {
            executor.can_autoexecute_action(action, conversation_id, ctx)
        });
        can_start_action_with_current_phase(current_phase, next_phase, can_autoexecute)
    }

    fn start_pending_action_by_id(
        &mut self,
        action_id: &AIAgentActionId,
        conversation_id: AIConversationId,
        initiator: ActionExecutionInitiator,
        ctx: &mut ModelContext<Self>,
    ) -> Option<StartedAction> {
        let is_user_initiated = initiator.is_user_initiated();
        if is_user_initiated && self.running_actions.contains_key(&conversation_id) {
            // User-driven approvals still execute one action at a time so that interactive
            // confirmations do not overlap in the UI.
            return None;
        }

        let idx = self
            .pending_actions
            .get(&conversation_id)
            .and_then(|queue| queue.iter().position(|action| &action.id == action_id))?;

        let key = (conversation_id, action_id.clone());
        if self.provider_tool_executions.contains_key(&key) && idx != 0 {
            return None;
        }
        // Readiness callbacks also enter here directly, bypassing the normal queue loop.
        // They must obey the same serial barriers before preprocessing or executing.
        if let Some(current_phase) = self.action_execution_phase(conversation_id) {
            let action = self.pending_actions[&conversation_id][idx].clone();
            if !self.can_start_action_in_current_phase(&action, conversation_id, current_phase, ctx)
            {
                return None;
            }
        }
        if let Some(in_flight) = self.deferred_provider_preprocessing.get_mut(&key) {
            if *in_flight {
                return None;
            }
            *in_flight = true;
            let action = self.pending_actions[&conversation_id][idx].clone();
            let execution_ref = self.provider_tool_execution_ref(conversation_id, action_id);
            let future = self.preprocess_action(&action, conversation_id, ctx);
            ctx.spawn(future, move |me, (), ctx| {
                // A cancelled or replaced generation must never restart this action.
                if me.provider_tool_execution_ref(conversation_id, &action.id) != execution_ref {
                    return;
                }
                me.deferred_provider_preprocessing.remove(&key);
                if is_user_initiated {
                    me.execute_action(&action.id, conversation_id, ctx);
                } else {
                    me.try_to_execute_available_actions(conversation_id, ctx);
                }
            });
            return None;
        }

        let action = self
            .pending_actions
            .get_mut(&conversation_id)?
            .remove(idx)?;

        let action_id = action.id.clone();
        let phase = self.action_phase_for_action(&action, ctx);
        #[cfg(not(target_family = "wasm"))]
        let remote_log_action_context = serde_json::json!({
            "conversation_id": conversation_id.to_string(),
            "action_id": action_id.to_string(),
            "task_id": action.task_id.to_string(),
            "tool_name": action_tool_name(&action),
            "permission_kind": format!("{:?}", permission_kind_for_action(&action.action)),
            "phase": format!("{phase:?}"),
        });
        let resolves_permission = self.pending_permissions.contains(&key)
            && (is_user_initiated
                || self.executor.update(ctx, |executor, ctx| {
                    executor.can_autoexecute_action(&action, conversation_id, ctx)
                }));
        if resolves_permission {
            self.pending_permissions.remove(&key);
            #[cfg(not(target_family = "wasm"))]
            log_tool_event(
                ctx,
                RemoteLogLevel::Info,
                "Tool permission resolved",
                serde_json::json!({
                    "event": "tool_permission_resolved",
                    "decision": "allow_once",
                    "tool": remote_log_action_context,
                }),
            );
            ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
                action_id: action_id.clone(),
                execution_ref: self.provider_tool_execution_ref(conversation_id, &action_id),
                event: ToolEvent::PermissionResolved {
                    request_id: permission_request_id(&action_id),
                    call_id: action_id.to_string(),
                    decision: PermissionDecision::AllowOnce,
                },
            });
        }
        // WaitForEvents owns its own status transition; skip the default
        // in-progress update.
        let is_wait_for_events = matches!(action.action, AIAgentActionType::WaitForEvents { .. });
        let execute_result = self.executor.update(ctx, |executor, ctx| {
            executor.try_to_execute_action(action, conversation_id, is_user_initiated, ctx)
        });

        match execute_result {
            TryExecuteResult::ExecutedAsync => {
                self.not_ready_actions.clear(conversation_id, &action_id);
                #[cfg(not(target_family = "wasm"))]
                log_tool_event(
                    ctx,
                    RemoteLogLevel::Info,
                    "Tool execution started",
                    serde_json::json!({
                        "event": "tool_execution_started",
                        "initiated_by": if is_user_initiated { "user" } else { "auto" },
                        "tool": remote_log_action_context,
                    }),
                );
                if !is_wait_for_events {
                    self.update_conversation_in_progress_status(conversation_id, ctx);
                }
                self.add_running_action(conversation_id, action_id, phase);
                Some(StartedAction::Async { phase })
            }
            TryExecuteResult::ExecutedSync => {
                self.not_ready_actions.clear(conversation_id, &action_id);
                #[cfg(not(target_family = "wasm"))]
                log_tool_event(
                    ctx,
                    RemoteLogLevel::Info,
                    "Tool execution started",
                    serde_json::json!({
                        "event": "tool_execution_started",
                        "initiated_by": if is_user_initiated { "user" } else { "auto" },
                        "tool": remote_log_action_context,
                    }),
                );
                if !is_wait_for_events {
                    self.update_conversation_in_progress_status(conversation_id, ctx);
                }
                Some(StartedAction::Sync)
            }
            TryExecuteResult::NotExecuted { reason, action } => {
                self.not_ready_actions.update_after_attempt(
                    conversation_id,
                    action_id,
                    reason,
                    initiator,
                );
                self.pending_actions
                    .entry(conversation_id)
                    .or_default()
                    .insert(idx, (*action).clone());
                self.handle_not_executed_action(action.as_ref(), reason, conversation_id, ctx);
                None
            }
        }
    }

    fn preprocess_action(
        &mut self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) -> BoxFuture<'static, ()> {
        self.executor.update(ctx, |executor, ctx| {
            executor.preprocess_action(action, conversation_id, ctx)
        })
    }

    fn provider_tool_execution_ref(
        &self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> Option<ProviderToolExecutionRef> {
        self.provider_tool_executions
            .get(&(conversation_id, action_id.clone()))
            .cloned()
    }

    #[allow(dead_code)]
    pub(super) fn queue_provider_actions(
        &mut self,
        actions: Vec<AIAgentAction>,
        recovery_action_ids: HashSet<AIAgentActionId>,
        conversation_id: AIConversationId,
        batch: &PendingToolBatch,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), ProviderActionQueueError> {
        let action_ids = self.provider_action_ids_to_enqueue(&actions, conversation_id, batch)?;
        let refs = provider_action_correlations(&actions, conversation_id, batch)?
            .into_iter()
            .filter(|((_, action_id), _)| action_ids.contains(action_id));
        self.provider_tool_executions.extend(refs);
        self.executor.update(ctx, |executor, ctx| {
            executor.mark_restored_actions(conversation_id, &recovery_action_ids, ctx);
        });
        let actions: Vec<AIAgentAction> = actions
            .into_iter()
            .filter(|action| action_ids.contains(&action.id))
            .collect();
        if !actions.is_empty() {
            self.queue_actions(actions, conversation_id, ctx);
        }
        Ok(())
    }

    pub(super) fn provider_action_ids_to_enqueue(
        &self,
        actions: &[AIAgentAction],
        conversation_id: AIConversationId,
        batch: &PendingToolBatch,
    ) -> Result<HashSet<AIAgentActionId>, ProviderActionQueueError> {
        Self::provider_action_ids_to_enqueue_from(
            &self.provider_tool_executions,
            actions,
            conversation_id,
            batch,
        )
    }

    fn provider_action_ids_to_enqueue_from(
        existing_correlations: &HashMap<
            (AIConversationId, AIAgentActionId),
            ProviderToolExecutionRef,
        >,
        actions: &[AIAgentAction],
        conversation_id: AIConversationId,
        batch: &PendingToolBatch,
    ) -> Result<HashSet<AIAgentActionId>, ProviderActionQueueError> {
        let refs = provider_action_correlations(actions, conversation_id, batch)?;
        let mut action_ids = HashSet::with_capacity(refs.len());
        let mut seen_refs = HashSet::with_capacity(refs.len());
        for ((_, action_id), execution_ref) in &refs {
            if !seen_refs.insert(execution_ref.clone()) {
                return Err(ProviderActionQueueError::ExistingCorrelation {
                    call_id: action_id.to_string(),
                });
            }
            if let Some(existing_ref) =
                existing_correlations.get(&(conversation_id, action_id.clone()))
            {
                if existing_ref == execution_ref {
                    continue;
                }
                return Err(ProviderActionQueueError::ExistingCorrelation {
                    call_id: action_id.to_string(),
                });
            }
            if existing_correlations
                .values()
                .any(|existing_ref| existing_ref == execution_ref)
            {
                return Err(ProviderActionQueueError::ExistingCorrelation {
                    call_id: action_id.to_string(),
                });
            }
            action_ids.insert(action_id.clone());
        }
        Ok(action_ids)
    }

    /// Queues the `actions` in the given iterator for the given conversation,
    /// to be dispatched in the order in which they appear in the iterator.
    pub(super) fn queue_actions(
        &mut self,
        actions: Vec<AIAgentAction>,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        crate::ai::tool_diagnostics::tool_debug!(
            "queue_actions: queuing {} actions for conversation {:?}",
            actions.len(),
            conversation_id
        );
        for (i, action) in actions.iter().enumerate() {
            crate::ai::tool_diagnostics::tool_debug!(
                "queue_actions:   [{}] id={:?}, type={:?}",
                i,
                action.id,
                std::mem::discriminant(&action.action)
            );
        }
        #[cfg(not(target_family = "wasm"))]
        log_tool_event(
            ctx,
            RemoteLogLevel::Info,
            "Tools queued",
            serde_json::json!({
                "event": "tools_queued",
                "conversation_id": conversation_id.to_string(),
                "tool_count": actions.len(),
                "tools": actions
                    .iter()
                    .map(|action| {
                        serde_json::json!({
                            "action_id": action.id.to_string(),
                            "task_id": action.task_id.to_string(),
                            "tool_name": action_tool_name(action),
                            "requires_result": action.requires_result,
                            "permission_kind": format!("{:?}", permission_kind_for_action(&action.action)),
                        })
                    })
                    .collect::<Vec<_>>(),
            }),
        );
        self.action_order.insert(
            conversation_id,
            actions
                .iter()
                .enumerate()
                .map(|(index, action)| (action.id.clone(), index))
                .collect(),
        );
        let mut preprocess_future = Vec::with_capacity(actions.len());
        let mut action_ids = HashSet::with_capacity(actions.len());

        for action in actions.iter() {
            action_ids.insert(action.id.clone());
            if self
                .provider_tool_executions
                .contains_key(&(conversation_id, action.id.clone()))
            {
                self.deferred_provider_preprocessing
                    .insert((conversation_id, action.id.clone()), false);
            } else {
                preprocess_future.push(self.preprocess_action(action, conversation_id, ctx));
            }
        }

        let preprocess_id = self
            .pending_preprocessed_actions
            .entry(conversation_id)
            .or_default()
            .insert_preprocess_action_batch(action_ids);

        ctx.spawn(join_all(preprocess_future), move |me, _, ctx| {
            me.handle_preprocess_actions_results(conversation_id, preprocess_id, actions, ctx);
        });
    }

    fn handle_preprocess_actions_results(
        &mut self,
        conversation_id: AIConversationId,
        preprocess_id: PreprocessId,
        actions: Vec<AIAgentAction>,
        ctx: &mut ModelContext<Self>,
    ) {
        let actions_to_enqueue = self
            .pending_preprocessed_actions
            .entry(conversation_id)
            .or_default()
            .handle_preprocess_actions_result(preprocess_id, actions);

        for action in actions_to_enqueue {
            let action_id = action.id.clone();
            // Some actions may already have results. This can happen in session sharing when
            // the sharer finishes and sends a result while preprocessing is still running on the viewer.
            // This is an edge case that only happens with fast tool calls, but we still need to guard against it,
            // as otherwise tools get stuck in a pending state on the viewer's side of things. This check
            // must be scoped to the current conversation as some providers generate tool call IDs that
            // only unique within a conversation.
            let has_finished_result = self
                .finished_action_results
                .get(&conversation_id)
                .is_some_and(|results| results.iter().any(|result| result.id == action_id))
                || self
                    .provider_finished_action_results
                    .iter()
                    .filter(|((finished_conversation_id, _), _)| {
                        *finished_conversation_id == conversation_id
                    })
                    .any(|(_, results)| results.iter().any(|result| result.id == action_id));
            if has_finished_result {
                continue;
            }

            // In view-only mode, if an action is already marked as running
            // (which can happen if we receive a CommandExecutionStarted event
            // before the action is queued), don't add it to the pending queue to avoid an inconsistent state.
            if self.is_view_only
                && self
                    .running_actions
                    .get(&conversation_id)
                    .is_some_and(|running| running.contains(&action_id))
            {
                continue;
            }

            self.pending_actions
                .entry(conversation_id)
                .or_default()
                .push_back(action);
            ctx.emit(BlocklistAIActionEvent::QueuedAction {
                execution_ref: self.provider_tool_execution_ref(conversation_id, &action_id),
                action_id,
            });
        }
        self.try_to_execute_available_actions(conversation_id, ctx);
    }

    /// Apply a finished action result to the conversation.
    /// This is used in agent session sharing to apply finished action results
    /// received from the action stream.
    pub fn apply_finished_action_result(
        &mut self,
        conversation_id: AIConversationId,
        mut action_result: AIAgentActionResult,
        ctx: &mut ModelContext<Self>,
    ) {
        let action_id = action_result.id.clone();
        if let Some(queue) = self.pending_actions.get_mut(&conversation_id) {
            if let Some(idx) = queue.iter().position(|a| a.id == action_id) {
                queue.remove(idx);
            }
        }

        // For shared session viewers, take in any document action results
        // and apply the associated actions to the local document version
        // (or create a new document if the given doc does not exist).
        self.maybe_sync_view_only_documents_with_local_model(
            conversation_id,
            &mut action_result,
            ctx,
        );

        self.handle_action_result(conversation_id, Arc::new(action_result), None, ctx);
    }

    pub(super) fn cancel_action_with_id(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
        reason: CancellationReason,
        ctx: &mut ModelContext<Self>,
    ) {
        let status = self.get_action_status(conversation_id, action_id);
        let permission_denied = is_permission_denial(reason, status.as_ref());
        if self
            .running_actions
            .get(&conversation_id)
            .is_some_and(|running| running.contains(action_id))
        {
            self.executor.update(ctx, |executor, ctx| {
                executor.cancel_running_async_action(conversation_id, action_id, Some(reason), ctx)
            });
        } else {
            let Some(pending_actions_for_conversation) =
                self.pending_actions.get_mut(&conversation_id)
            else {
                return;
            };
            if let Some((idx, _)) = pending_actions_for_conversation
                .iter()
                .find_position(|action| action.id == *action_id)
            {
                if let Some(action) = pending_actions_for_conversation.remove(idx) {
                    self.cancel_pending_action(
                        conversation_id,
                        action,
                        Some(reason),
                        permission_denied,
                        ctx,
                    );
                }
            }
        }
    }

    /// Cancels any in-flight WaitForEvents action for the given conversation.
    pub fn cancel_wait_for_events_for_conversation(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let action_id = self.executor.update(ctx, |executor, _| {
            executor.find_running_wait_for_events(conversation_id)
        });
        if let Some(action_id) = action_id {
            self.cancel_action_with_id(
                conversation_id,
                &action_id,
                CancellationReason::FollowUpSubmitted {
                    is_for_same_conversation: true,
                },
                ctx,
            );
        }
    }

    pub(super) fn cancel_all_pending_actions(
        &mut self,
        conversation_id: AIConversationId,
        reason: Option<CancellationReason>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.deferred_provider_preprocessing
            .retain(|(conv_id, _), _| *conv_id != conversation_id);
        self.executor.update(ctx, |executor, ctx| {
            executor.cancel_all_running_async_actions_for_conversation(conversation_id, reason, ctx)
        });

        if let Some(preprocessing) = self.pending_preprocessed_actions.remove(&conversation_id) {
            self.provider_tool_executions
                .retain(|(correlated_conversation_id, action_id), _| {
                    *correlated_conversation_id != conversation_id
                        || !preprocessing.contains(action_id)
                });
        }

        let Some(actions_to_cancel) = self.pending_actions.get_mut(&conversation_id) else {
            return;
        };
        for action in actions_to_cancel.drain(..).collect_vec() {
            crate::ai::tool_diagnostics::tool_debug!(
                "Canceling pending action of type {:?} conversation_id={conversation_id:?} action_id={:?}, reason={:?}",
                AIAgentActionTypeDiscriminants::from(&action.action),
                action.id,
                reason
            );
            if let Some(backtrace) = crate::ai::tool_diagnostics::capture_backtrace() {
                log::debug!("Pending action cancellation backtrace:\n{backtrace}");
            }
            self.cancel_pending_action(conversation_id, action, reason, false, ctx);
        }
    }

    /// Removes and returns all pending RequestCommandOutput actions for a conversation.
    fn drain_pending_request_command_actions(
        &mut self,
        conversation_id: AIConversationId,
    ) -> Vec<AIAgentAction> {
        let Some(pending_actions) = self.pending_actions.get_mut(&conversation_id) else {
            return Vec::new();
        };

        let mut to_drain = Vec::new();
        let mut i = 0;
        while i < pending_actions.len() {
            if matches!(
                pending_actions[i].action,
                AIAgentActionType::RequestCommandOutput { .. }
            ) {
                to_drain.push(
                    pending_actions
                        .remove(i)
                        .expect("index is valid because i < pending_actions.len()"),
                );
            } else {
                i += 1;
            }
        }
        to_drain
    }

    fn resolve_permission_denial(
        &mut self,
        conversation_id: AIConversationId,
        pending_action: &AIAgentAction,
        ctx: &mut ModelContext<Self>,
    ) {
        self.denied_permissions
            .insert((conversation_id, pending_action.id.clone()));
        #[cfg(not(target_family = "wasm"))]
        log_tool_event(
            ctx,
            RemoteLogLevel::Warn,
            "Tool permission resolved",
            serde_json::json!({
                "event": "tool_permission_resolved",
                "decision": "denied",
                "conversation_id": conversation_id.to_string(),
                "action_id": pending_action.id.to_string(),
                "task_id": pending_action.task_id.to_string(),
                "tool_name": action_tool_name(pending_action),
                "permission_kind": format!("{:?}", permission_kind_for_action(&pending_action.action)),
            }),
        );
        ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
            action_id: pending_action.id.clone(),
            execution_ref: self.provider_tool_execution_ref(conversation_id, &pending_action.id),
            event: permission_denied_tool_event(pending_action),
        });
    }

    fn cancel_pending_action(
        &mut self,
        conversation_id: AIConversationId,
        pending_action: AIAgentAction,
        reason: Option<CancellationReason>,
        permission_denied: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        if permission_denied {
            self.resolve_permission_denial(conversation_id, &pending_action, ctx);
        }

        if matches!(
            pending_action.action,
            AIAgentActionType::RequestComputerUse(_)
        ) {
            let server_conversation_id = BlocklistAIHistoryModel::as_ref(ctx)
                .conversation(&conversation_id)
                .and_then(|c| c.server_conversation_token())
                .map(|t| t.as_str().to_string());
            send_telemetry_from_ctx!(
                TelemetryEvent::ComputerUseCancelled {
                    client_conversation_id: conversation_id,
                    server_conversation_id,
                    ambient_agent_task_id: self.ambient_agent_task_id,
                },
                ctx
            );
        }

        let result = Arc::new(AIAgentActionResult {
            id: pending_action.id,
            task_id: pending_action.task_id,
            result: pending_action.action.cancelled_result(),
        });
        self.handle_action_result(conversation_id, result, reason, ctx);
    }

    /// Returns all finished action results from the given conversation, moving them to the
    /// `past_action_results` in the process.
    pub(super) fn drain_finished_action_results(
        &mut self,
        conversation_id: AIConversationId,
    ) -> Vec<AIAgentActionResult> {
        self.action_order.remove(&conversation_id);
        let finished_action_results = self
            .finished_action_results
            .remove(&conversation_id)
            .unwrap_or_default();

        for result in finished_action_results.iter() {
            self.past_action_results
                .insert((conversation_id, result.id.clone()), result.clone());
        }
        finished_action_results
            .into_iter()
            .map(|result| (*result).clone())
            .collect_vec()
    }

    pub(super) fn drain_finished_tool_results(
        &mut self,
        conversation_id: AIConversationId,
    ) -> Vec<ToolResult> {
        self.finished_tool_results
            .remove(&conversation_id)
            .unwrap_or_default()
    }

    pub(super) fn provider_finished_action_results(
        &self,
        conversation_id: AIConversationId,
        work_id: &ExternalWorkId,
    ) -> Vec<Arc<AIAgentActionResult>> {
        self.provider_finished_action_results
            .get(&(conversation_id, work_id.clone()))
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn archive_provider_finished_action_results(
        &mut self,
        conversation_id: AIConversationId,
        work_id: &ExternalWorkId,
    ) {
        let results = self
            .provider_finished_action_results
            .remove(&(conversation_id, work_id.clone()))
            .unwrap_or_default();
        for result in results {
            self.past_action_results
                .insert((conversation_id, result.id.clone()), result);
        }
    }

    /// Clears finished action results for a conversation. Used when reverting.
    pub(super) fn clear_finished_action_results(&mut self, conversation_id: AIConversationId) {
        self.action_order.remove(&conversation_id);
        self.finished_action_results.remove(&conversation_id);
        self.finished_tool_results.remove(&conversation_id);
        self.provider_finished_action_results
            .retain(|(finished_conversation_id, _), _| {
                *finished_conversation_id != conversation_id
            });
    }

    /// Removes all per-conversation state for a conversation that has been removed or cleared.
    /// This prevents unbounded growth of `past_action_results` and other maps over long sessions.
    fn cleanup_conversation_state(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.deferred_provider_preprocessing
            .retain(|(conv_id, _), _| *conv_id != conversation_id);
        self.past_action_results
            .retain(|(conv_id, _), _| *conv_id != conversation_id);
        self.provider_tool_executions
            .retain(|(conv_id, _), _| *conv_id != conversation_id);
        self.denied_permissions
            .retain(|(conv_id, _)| *conv_id != conversation_id);
        self.pending_permissions
            .retain(|(conv_id, _)| *conv_id != conversation_id);
        self.pending_actions.remove(&conversation_id);
        self.running_actions.remove(&conversation_id);
        self.finished_action_results.remove(&conversation_id);
        self.finished_tool_results.remove(&conversation_id);
        self.provider_finished_action_results
            .retain(|(conv_id, _), _| *conv_id != conversation_id);
        self.action_order.remove(&conversation_id);
        self.pending_preprocessed_actions.remove(&conversation_id);
        self.not_ready_actions
            .actions
            .retain(|(conv_id, _)| *conv_id != conversation_id);
        self.executor.update(ctx, |executor, ctx| {
            executor.cleanup_conversation(conversation_id, ctx);
        });
    }

    #[cfg(test)]
    pub(super) fn push_pending_action_for_test(
        &mut self,
        conversation_id: AIConversationId,
        action: AIAgentAction,
    ) {
        self.pending_actions
            .entry(conversation_id)
            .or_default()
            .push_back(action);
    }

    /// The control flow for initiating cancellations across suggested plans, requested commands,
    /// and code diff views are identical, and thus should be handled directly by the [`AIBlock`]'s
    /// respective functions.
    pub fn handle_requested_command_accepted(
        &mut self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
        command: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(action) = self
            .pending_actions
            .get_mut(&conversation_id)
            .and_then(|actions| actions.iter_mut().find(|action| action.id == *action_id))
        else {
            log::warn!("Ignoring acceptance for non-pending requested command: {action_id:?}");
            return;
        };
        let AIAgentActionType::RequestCommandOutput {
            command: original_command,
            ..
        } = &mut action.action
        else {
            log::warn!("Ignoring acceptance for non-command action: {action_id:?}");
            return;
        };
        *original_command = command;

        self.execute_action(action_id, conversation_id, ctx);
    }

    fn handle_action_result(
        &mut self,
        conversation_id: AIConversationId,
        action_result: Arc<AIAgentActionResult>,
        cancellation_reason: Option<CancellationReason>,
        ctx: &mut ModelContext<Self>,
    ) {
        crate::ai::tool_diagnostics::tool_debug!(
            "handle_action_result: action_id={:?}, result_type={:?}, cancellation={:?}",
            action_result.id,
            std::mem::discriminant(&action_result.result),
            cancellation_reason
        );
        self.not_ready_actions
            .clear(conversation_id, &action_result.id);
        let should_remove_entry =
            self.running_actions
                .get_mut(&conversation_id)
                .is_some_and(|running| {
                    running.remove_action(&action_result.id);
                    running.is_empty()
                });

        if should_remove_entry {
            self.running_actions.remove(&conversation_id);
        }

        let action_id = action_result.id.clone();

        // If a command action entered long-running mode (returned a snapshot), cancel all other
        // pending RequestCommandOutput actions. Only one command can be active at a time, and the
        // server can only spawn one CLI subagent. We don't cancel other actions because those
        // actions will complete before we send any response to the server. NOTE: this does allow
        // the long-running command to execute in parallel with the other actions.
        if matches!(
            &action_result.result,
            AIAgentActionResultType::RequestCommandOutput(
                RequestCommandOutputResult::LongRunningCommandSnapshot { .. }
            )
        ) {
            for action in self.drain_pending_request_command_actions(conversation_id) {
                self.cancel_pending_action(
                    conversation_id,
                    action,
                    cancellation_reason,
                    false,
                    ctx,
                );
            }
        }

        let execution_ref = self
            .provider_tool_executions
            .remove(&(conversation_id, action_result.id.clone()));
        self.deferred_provider_preprocessing
            .remove(&(conversation_id, action_result.id.clone()));
        self.pending_permissions
            .remove(&(conversation_id, action_result.id.clone()));
        let permission_denied = self
            .denied_permissions
            .remove(&(conversation_id, action_result.id.clone()));
        if execution_ref.is_some() {
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                if let Some(conversation) = history.conversation_mut(&conversation_id) {
                    if let Err(error) = conversation.save_provider_tool_result(&action_result, ctx)
                    {
                        log::error!("Failed to save provider tool result: {error}");
                    }
                }
            });
        }
        let tool_result = domain_tool_result(&action_result, permission_denied);
        if execution_ref.is_none() {
            self.finished_tool_results
                .entry(conversation_id)
                .or_default()
                .push(tool_result.clone());
        }
        #[cfg(not(target_family = "wasm"))]
        log_tool_event(
            ctx,
            if permission_denied {
                RemoteLogLevel::Warn
            } else {
                action_result_log_level(&action_result.result)
            },
            "Tool execution completed",
            serde_json::json!({
                "event": "tool_execution_completed",
                "conversation_id": conversation_id.to_string(),
                "action_id": action_result.id.to_string(),
                "task_id": action_result.task_id.to_string(),
                "result_type": action_result_type_name(&action_result.result),
                "status": if permission_denied {
                    "denied"
                } else {
                    action_result_status(&action_result.result)
                },
                "tool_result_status": format!("{:?}", tool_result.status),
                "permission_denied": permission_denied,
                "cancellation_reason": cancellation_reason.map(|reason| format!("{reason:?}")),
                "error": action_result_error_summary(&action_result.result),
            }),
        );
        // Permission denial completes provider-owned calls when the permission decision is
        // applied, so emitting a second correlated completion would violate exactly-once delivery.
        if should_emit_tool_completion(execution_ref.is_some(), permission_denied) {
            ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
                action_id: action_result.id.clone(),
                execution_ref: execution_ref.clone(),
                event: ToolEvent::Completed {
                    result: tool_result,
                },
            });
        }

        if let Some(execution_ref) = &execution_ref {
            self.provider_finished_action_results
                .entry((conversation_id, execution_ref.work_id()))
                .or_default()
                .push(action_result);
        } else {
            self.finished_action_results
                .entry(conversation_id)
                .or_default()
                .push(action_result);
        }

        if self
            .running_actions
            .get(&conversation_id)
            .is_some_and(|running| !running.is_empty())
        {
            // Wait until the entire phase drains before scheduling subsequent actions or deciding
            // whether to send a follow-up request. In particular, don't emit `FinishedAction` yet:
            // the controller treats that event as the phase-complete signal and could otherwise
            // drain only a prefix of parallel tool results into the next LLM request.
            return;
        }

        // A serial phase may have more queued actions. Start those before notifying the controller;
        // otherwise the controller can drain this phase's result while the next action is still
        // pending, and that later tool result will lose its follow-up trigger.
        if self
            .pending_actions
            .get(&conversation_id)
            .is_some_and(|actions| !actions.is_empty())
        {
            self.try_to_execute_available_actions(conversation_id, ctx);
            return;
        }

        // The entire action batch is fully drained — sort results back into original tool-call
        // order before notifying the controller that it may send one complete follow-up request.
        self.sort_finished_results(conversation_id);
        ctx.emit(BlocklistAIActionEvent::FinishedAction {
            action_id,
            conversation_id,
            cancellation_reason,
            execution_ref: execution_ref.clone(),
        });

        if self
            .pending_actions
            .get(&conversation_id)
            .is_none_or(|actions| actions.is_empty())
        {
            // Only a `Cancelled` outcome stamps a status here. The other outcomes are
            // owned elsewhere: `KeepInProgress` / `Succeeded` and `FinalizedExternally`
            // are finalized by the controller or a dedicated path, and a normal
            // completion (no cancellation reason) is resolved by the controller's
            // follow-up handling. Stamping here for any of those would clobber the real
            // status and message.
            if execution_ref.is_none()
                && cancellation_reason.is_some_and(|r| {
                    matches!(r.conversation_outcome(), CancellationOutcome::Cancelled)
                })
            {
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    // Treat action result as authoritative for determining status.
                    let status = if self
                        .finished_action_results
                        .get(&conversation_id)
                        .is_some_and(|finished_results| {
                            finished_results
                                .iter()
                                .all(|result| result.result.is_cancelled())
                        }) {
                        ConversationStatus::Cancelled
                    } else {
                        ConversationStatus::InProgress
                    };
                    history_model.update_conversation_status(
                        self.terminal_view_id,
                        conversation_id,
                        status,
                        ctx,
                    );
                });
            }
        }
    }

    /// In shared-session viewer (view-only) mode, ensure document-related action results
    /// are backed by documents in the local `AIDocumentModel` and that their
    /// `DocumentContext` versions match. For CreateDocuments, restore missing documents
    /// (using titles from the original action); for EditDocuments, apply edits to local
    /// documents and align versions, so headers and "View" buttons stay accurate.
    fn maybe_sync_view_only_documents_with_local_model(
        &self,
        conversation_id: AIConversationId,
        result: &mut AIAgentActionResult,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.is_view_only {
            return;
        }

        match &mut result.result {
            AIAgentActionResultType::CreateDocuments(CreateDocumentsResult::Success {
                created_documents,
            }) => {
                let history = BlocklistAIHistoryModel::handle(ctx);
                let Some(conversation) = history.as_ref(ctx).conversation(&conversation_id) else {
                    return;
                };
                let titles = conversation.get_document_titles_for_action(&result.id);

                let doc_model = AIDocumentModel::handle(ctx);
                doc_model.update(ctx, |doc_model, doc_ctx| {
                    for (index, doc_context) in created_documents.iter_mut().enumerate() {
                        // If a user is re-opening a shared session that they previously closed in the current warp session,
                        // we should delete the previously created document so that the verseion history doesn't get messed up.
                        doc_model.delete_document(&doc_context.document_id);

                        let title = titles
                            .as_ref()
                            .and_then(|t| t.get(index))
                            .cloned()
                            .unwrap_or_else(|| DEFAULT_PLANNING_DOCUMENT_TITLE.to_string());

                        doc_model.restore_document(
                            doc_context.document_id,
                            conversation_id,
                            &title,
                            doc_context.content.clone(),
                            Local::now(),
                            doc_ctx,
                        );
                    }
                });
            }
            AIAgentActionResultType::EditDocuments(EditDocumentsResult::Success {
                updated_documents,
            }) => {
                let doc_model = AIDocumentModel::handle(ctx);
                doc_model.update(ctx, |doc_model, doc_ctx| {
                    for doc_context in updated_documents.iter_mut() {
                        if doc_model
                            .get_current_document(&doc_context.document_id)
                            .is_none()
                        {
                            // You can't make edits to a doc that does not exist.
                            continue;
                        }

                        if let Some(new_version) = doc_model.restore_document_edit(
                            &doc_context.document_id,
                            doc_context.content.clone(),
                            Local::now(),
                            doc_ctx,
                        ) {
                            // Align the header's version with the locally restored doc
                            // so the viewer sees the correct bumped version.
                            doc_context.document_version = new_version;
                        }
                    }
                });
            }
            _ => {}
        }
    }
}

fn provider_action_correlations(
    actions: &[AIAgentAction],
    conversation_id: AIConversationId,
    batch: &PendingToolBatch,
) -> Result<Vec<ProviderActionCorrelation>, ProviderActionQueueError> {
    let expected = batch.unresolved_call_ids();
    let received = actions
        .iter()
        .map(|action| action.id.to_string())
        .collect::<Vec<_>>();
    if expected != received {
        return Err(ProviderActionQueueError::ActionSetMismatch { expected, received });
    }

    Ok(actions
        .iter()
        .map(|action| {
            (
                (conversation_id, action.id.clone()),
                ProviderToolExecutionRef::new(
                    conversation_id,
                    &batch.work_id,
                    action.id.to_string(),
                ),
            )
        })
        .collect())
}

#[derive(Debug, Clone)]
pub enum BlocklistAIActionEvent {
    /// Emitted when the action with the given ID is enqueued for execution.
    QueuedAction {
        action_id: AIAgentActionId,
        execution_ref: Option<ProviderToolExecutionRef>,
    },
    /// Emitted when the action with the given ID requires user confirmation to execute.
    ActionBlockedOnUserConfirmation {
        action_id: AIAgentActionId,
        conversation_id: AIConversationId,
        execution_ref: Option<ProviderToolExecutionRef>,
    },
    /// Emitted when the action with the given ID begins execution.
    ExecutingAction {
        action_id: AIAgentActionId,
        conversation_id: AIConversationId,
        execution_ref: Option<ProviderToolExecutionRef>,
    },
    /// Emitted when the action with the given ID has finished.
    FinishedAction {
        action_id: AIAgentActionId,
        conversation_id: AIConversationId,
        cancellation_reason: Option<CancellationReason>,
        execution_ref: Option<ProviderToolExecutionRef>,
    },
    /// Provider-neutral permission and execution lifecycle event for runtime consumers.
    ToolLifecycle {
        action_id: AIAgentActionId,
        execution_ref: Option<ProviderToolExecutionRef>,
        event: ToolEvent,
    },
    InitProject(AIAgentActionId),
    ToggleCodeReview(AIAgentActionId),
    InsertCodeReviewComments {
        action_id: AIAgentActionId,
        repo_path: PathBuf,
        comments: Vec<ai::agent::action::InsertReviewComment>,
        base_branch: Option<String>,
    },
}

impl BlocklistAIActionEvent {
    pub fn action_id(&self) -> &AIAgentActionId {
        match self {
            BlocklistAIActionEvent::QueuedAction { action_id, .. }
            | BlocklistAIActionEvent::ActionBlockedOnUserConfirmation { action_id, .. }
            | BlocklistAIActionEvent::ExecutingAction { action_id, .. }
            | BlocklistAIActionEvent::FinishedAction { action_id, .. } => action_id,
            BlocklistAIActionEvent::ToolLifecycle { action_id, .. } => action_id,
            BlocklistAIActionEvent::InitProject(action_id) => action_id,
            BlocklistAIActionEvent::ToggleCodeReview(action_id) => action_id,
            BlocklistAIActionEvent::InsertCodeReviewComments { action_id, .. } => action_id,
        }
    }

    pub fn conversation_id(&self) -> Option<AIConversationId> {
        match self {
            BlocklistAIActionEvent::ActionBlockedOnUserConfirmation {
                conversation_id, ..
            }
            | BlocklistAIActionEvent::ExecutingAction {
                conversation_id, ..
            }
            | BlocklistAIActionEvent::FinishedAction {
                conversation_id, ..
            } => Some(*conversation_id),
            BlocklistAIActionEvent::QueuedAction { .. }
            | BlocklistAIActionEvent::ToolLifecycle { .. }
            | BlocklistAIActionEvent::InitProject(_)
            | BlocklistAIActionEvent::ToggleCodeReview(_)
            | BlocklistAIActionEvent::InsertCodeReviewComments { .. } => None,
        }
    }
}

impl Entity for BlocklistAIActionModel {
    type Event = BlocklistAIActionEvent;
}

#[cfg(test)]
#[path = "action_model_tests.rs"]
mod tests;
