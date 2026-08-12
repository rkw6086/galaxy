#![allow(dead_code)]

//! This module contains core business logic for Agent Mode, primarily sending input to an AI
//! model and receiving output.
//!
//! The `BlocklistAIController` orchestrates state updates and service calls to power the
//! Agent Mode UI.
pub mod input_context;
mod pending_response_streams;
pub mod response_stream;
pub(super) mod shared_session;
mod slash_command;
use std::collections::{HashMap, HashSet};
#[cfg(not(target_family = "wasm"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ai::skills::SkillPathOrigin;
use anyhow::anyhow;
use chrono::{DateTime, Local};
use galaxy_agent_core::ToolLoopGuard;
use galaxy_core::assertions::safe_assert;
use input_context::{input_context_for_request, parse_context_attachments};
use itertools::Itertools;
use parking_lot::FairMutex;
use pending_response_streams::PendingResponseStreams;
use session_sharing_protocol::common::ParticipantId;
use settings::Setting;
pub use slash_command::*;
use warp_multi_agent_api::{message, Task, ToolType};
use warpui::r#async::{SpawnedFutureHandle, Timer};
use warpui::{AppContext, Entity, EntityId, ModelContext, ModelHandle, SingletonEntity};

use self::response_stream::{ResponseStream, ResponseStreamEvent};
use super::action_model::{BlocklistAIActionEvent, BlocklistAIActionModel};
use super::context_model::{BlocklistAIContextModel, PendingAttachment, PendingFile};
use super::conversation_selection::{ConversationSelectionEvent, ConversationSelectionHandle};
use super::history_model::{BlocklistAIHistoryEvent, BlocklistAIHistoryModel};
use super::orchestration_event_streamer::{
    OrchestrationEventStreamer, OrchestrationEventStreamerEvent,
};
use super::orchestration_events::{OrchestrationEventService, OrchestrationEventServiceEvent};
use super::orchestration_topology::descendant_conversation_ids_in_spawn_order;
use super::queued_query::{QueuedQueryId, QueuedQueryModel};
use super::{BlocklistAIInputModel, ResponseStreamId};
use crate::ai::agent::api::{self, ServerConversationToken};
use crate::ai::agent::conversation::{AIConversation, AIConversationId, ConversationStatus};
use crate::ai::agent::task::TaskId;
#[cfg(not(target_family = "wasm"))]
use crate::ai::agent::AIAgentActionTypeDiscriminants;
use crate::ai::agent::{
    extract_user_query_mode, AIAgentAction, AIAgentActionResult, AIAgentActionResultType,
    AIAgentAttachment, AIAgentContext, AIAgentExchangeId, AIAgentInput, AIAgentOutputStatus,
    AIIdentifiers, CancellationOutcome, CancellationReason, DocumentContentAttachmentSource,
    EntrypointType, FileContext, FinishedAIAgentOutput, PassiveSuggestionResultType,
    PassiveSuggestionTrigger, PassiveSuggestionTriggerType, RenderableAIError,
    RequestCommandOutputResult, RequestCost, RequestMetadata, RunningCommand, StaticQueryType,
    TransientNetworkErrorKind, UserQueryMode,
};
use crate::ai::agent_events::AgentMessageEventMetadata;
#[cfg(not(target_family = "wasm"))]
use crate::ai::agent_sdk::ClaudeHarness;
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::document::ai_document_model::{
    AIDocumentId, AIDocumentModel, AIDocumentUserEditStatus,
};
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::provider::types::ContentPart;
#[cfg(not(target_family = "wasm"))]
use crate::ai::remote_logging::{self, RemoteLogLevel, RemoteLogRecord};
use crate::ai::AIRequestUsageModel;
use crate::cloud_object::model::persistence::CloudModel;
use crate::features::FeatureFlag;
use crate::global_resource_handles::GlobalResourceHandlesProvider;
use crate::network::NetworkStatus;
use crate::notebooks::editor::model::FileLinkResolutionContext;
use crate::persistence::model::AgentBackend;
use crate::persistence::ModelEvent;
use crate::send_telemetry_from_ctx;
use crate::server::server_api::AIApiError;
#[cfg(not(target_family = "wasm"))]
use crate::server::server_api::ServerApiProvider;
use crate::server::telemetry::TelemetryEvent;
use crate::terminal::model::block::{
    formatted_terminal_contents_for_input, BlockId, CURSOR_MARKER,
};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model::session::SessionType;
use crate::terminal::model::terminal_model::TerminalModel;
use crate::terminal::view::inline_banner::ZeroStatePromptSuggestionType;
use crate::terminal::ShellLaunchData;
use crate::workspace::OneTimeModalModel;
use crate::workspaces::update_manager::TeamUpdateManager;
use crate::workspaces::user_workspaces::UserWorkspaces;

#[derive(Debug, Clone)]
pub struct SessionContext {
    session_type: Option<SessionType>,
    shell: Option<ShellLaunchData>,
    current_working_directory: Option<String>,
}

impl SessionContext {
    pub fn from_session(session: &ActiveSession, app: &AppContext) -> Self {
        SessionContext {
            session_type: session.session_type(app),
            shell: session.shell_launch_data(app),
            current_working_directory: session.current_working_directory().cloned(),
        }
    }

    pub fn session_type(&self) -> &Option<SessionType> {
        &self.session_type
    }

    pub fn shell(&self) -> &Option<ShellLaunchData> {
        &self.shell
    }

    pub fn current_working_directory(&self) -> &Option<String> {
        &self.current_working_directory
    }

    /// Returns the remote host ID if this is a `WarpifiedRemote` session with
    /// a connected `RemoteServerClient`.
    pub fn host_id(&self) -> Option<&galaxy_core::HostId> {
        match &self.session_type {
            Some(SessionType::WarpifiedRemote { host_id }) => host_id.as_ref(),
            Some(SessionType::Local) | None => None,
        }
    }

    /// Returns `true` if this is a remote session (regardless of whether
    /// the remote server client is connected).
    pub fn is_remote(&self) -> bool {
        matches!(self.session_type, Some(SessionType::WarpifiedRemote { .. }))
    }

    pub fn skill_path_origin(&self) -> SkillPathOrigin {
        match &self.session_type {
            Some(SessionType::WarpifiedRemote {
                host_id: Some(host_id),
            }) => SkillPathOrigin::Remote {
                host_id: host_id.clone(),
            },
            Some(SessionType::WarpifiedRemote { host_id: None }) => SkillPathOrigin::Unavailable,
            Some(SessionType::Local) | None => SkillPathOrigin::Local,
        }
    }

    #[cfg(test)]
    pub fn new_for_test() -> Self {
        SessionContext {
            session_type: None,
            shell: None,
            current_working_directory: None,
        }
    }

    #[cfg(test)]
    pub fn new_with_session_type_for_test(session_type: Option<SessionType>) -> Self {
        SessionContext {
            session_type,
            shell: None,
            current_working_directory: None,
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn remote_action_tool_name(action: &AIAgentAction) -> String {
    action
        .tool_name
        .clone()
        .unwrap_or_else(|| format!("{:?}", AIAgentActionTypeDiscriminants::from(&action.action)))
}

#[cfg(not(target_family = "wasm"))]
fn remote_action_summaries(actions: &[AIAgentAction]) -> Vec<serde_json::Value> {
    actions
        .iter()
        .map(|action| {
            serde_json::json!({
                "action_id": action.id.to_string(),
                "task_id": action.task_id.to_string(),
                "tool_name": remote_action_tool_name(action),
                "requires_result": action.requires_result,
            })
        })
        .collect()
}

#[derive(Debug, Clone)]
struct FailedToolProposal {
    action_id: String,
    task_id: String,
    tool_name: String,
    requires_result: bool,
    error: String,
}

impl FailedToolProposal {
    fn new(action: &AIAgentAction, error: impl Into<String>) -> Self {
        Self {
            action_id: action.id.to_string(),
            task_id: action.task_id.to_string(),
            tool_name: failed_proposal_tool_name(action),
            requires_result: action.requires_result,
            error: error.into(),
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn to_remote_log_value(&self) -> serde_json::Value {
        serde_json::json!({
            "action_id": self.action_id,
            "task_id": self.task_id,
            "tool_name": self.tool_name,
            "requires_result": self.requires_result,
            "error": remote_logging::sanitize_error(&self.error),
        })
    }
}

fn failed_proposal_tool_name(action: &AIAgentAction) -> String {
    if let Some(tool_name) = action.tool_name.clone() {
        return tool_name;
    }
    #[cfg(not(target_family = "wasm"))]
    {
        format!("{:?}", AIAgentActionTypeDiscriminants::from(&action.action))
    }
    #[cfg(target_family = "wasm")]
    {
        "unknown".to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolQueueDecision {
    Cancelled,
    UnfinishedExchange,
    BlockedFailedToolProposal,
    BlockedActiveChildAgents,
    NoActions,
    QueueActionsWithStreamSnapshotFallback,
    QueueActions,
}

impl ToolQueueDecision {
    fn label(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::UnfinishedExchange => "unfinished_exchange",
            Self::BlockedFailedToolProposal => "blocked_failed_tool_proposal",
            Self::BlockedActiveChildAgents => "blocked_active_child_agents",
            Self::NoActions => "no_actions",
            Self::QueueActionsWithStreamSnapshotFallback => {
                "queue_actions_with_stream_snapshot_fallback"
            }
            Self::QueueActions => "queue_actions",
        }
    }

    fn will_queue_actions(self) -> bool {
        matches!(
            self,
            Self::QueueActions | Self::QueueActionsWithStreamSnapshotFallback
        )
    }

    #[cfg(not(target_family = "wasm"))]
    fn remote_log_level(self) -> RemoteLogLevel {
        match self {
            Self::BlockedFailedToolProposal
            | Self::BlockedActiveChildAgents
            | Self::QueueActionsWithStreamSnapshotFallback => RemoteLogLevel::Warn,
            Self::Cancelled | Self::UnfinishedExchange | Self::NoActions | Self::QueueActions => {
                RemoteLogLevel::Info
            }
        }
    }
}

fn tool_queue_decision(
    has_cancellation: bool,
    has_unfinished_exchange: bool,
    has_failed_tool_proposal: bool,
    has_active_child_agents: bool,
    candidate_action_count: usize,
    queued_from_stream_snapshot_count: usize,
) -> ToolQueueDecision {
    if has_cancellation {
        ToolQueueDecision::Cancelled
    } else if has_unfinished_exchange {
        ToolQueueDecision::UnfinishedExchange
    } else if has_failed_tool_proposal {
        ToolQueueDecision::BlockedFailedToolProposal
    } else if has_active_child_agents {
        ToolQueueDecision::BlockedActiveChildAgents
    } else if candidate_action_count == 0 {
        ToolQueueDecision::NoActions
    } else if queued_from_stream_snapshot_count > 0 {
        ToolQueueDecision::QueueActionsWithStreamSnapshotFallback
    } else {
        ToolQueueDecision::QueueActions
    }
}

fn active_descendant_conversation_ids(
    history: &BlocklistAIHistoryModel,
    conversation_id: AIConversationId,
) -> Vec<AIConversationId> {
    descendant_conversation_ids_in_spawn_order(history, conversation_id)
        .into_iter()
        .filter(|descendant_id| {
            history
                .conversation(descendant_id)
                .is_some_and(|conversation| !conversation.status().is_done())
        })
        .collect()
}

fn query_targets_existing_conversation(input_query: &InputQuery) -> Option<AIConversationId> {
    match &input_query.which_task {
        WhichTask::Task {
            conversation_id, ..
        } => Some(*conversation_id),
        WhichTask::NewConversation => None,
    }
}

pub enum BlocklistAIControllerEvent {
    /// Emitted when a request is sent to the AI agent API.
    SentRequest {
        contains_user_query: bool,
        /// True when this request is the first send of a previously queued prompt (e.g.
        /// via `/queue` or the auto-queue toggle) rather than a direct user submission.
        /// Subscribers that perform user-submission side effects (e.g. clearing the input
        /// buffer) should skip those effects when this is true — the user may have typed
        /// new input while the agent was busy and we don't want to wipe it.
        is_queued_prompt: bool,
        /// The model ID used for this request. None for slash commands that don't
        /// send a model request (e.g., /fork).
        model_id: LLMId,
        /// The ID of the response stream for this request.
        stream_id: ResponseStreamId,
    },

    /// Emitted when an AI output response is fully received, particularly relevant when output is
    /// being streamed.
    FinishedReceivingOutput {
        stream_id: ResponseStreamId,
        conversation_id: AIConversationId,
    },

    /// Emitted when the export-to-file slash command is executed.
    ExportConversationToFile {
        filename: Option<String>,
    },

    ExecuteLocalHarnessCommand {
        command: String,
    },

    FreeTierLimitCheckTriggered,
}

#[derive(Debug)]
pub struct RequestInput {
    pub conversation_id: AIConversationId,
    pub input_messages: HashMap<TaskId, Vec<AIAgentInput>>,
    pub working_directory: Option<String>,
    pub model_id: LLMId,
    pub coding_model_id: LLMId,
    pub cli_agent_model_id: LLMId,
    pub computer_use_model_id: LLMId,
    pub shared_session_response_initiator: Option<ParticipantId>,
    pub request_start_ts: DateTime<Local>,
    pub supported_tools_override: Option<Vec<ToolType>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunningCommandDetection {
    Detect,
    Skip,
}

fn no_action_tool_error_recovery_reason(
    had_failed_tool_result: bool,
    agent_output: &str,
) -> Option<&'static str> {
    if !had_failed_tool_result {
        return None;
    }

    let output = agent_output.trim();
    if output.is_empty() {
        return None;
    }

    let lower = output.to_ascii_lowercase();
    let has_tool_intent = [
        "check", "find", "grep", "inspect", "look", "open", "read", "recall", "search", "verify",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !has_tool_intent {
        return None;
    }

    let promise_prefixes = [
        "i'll ",
        "i will ",
        "i’m going to ",
        "i'm going to ",
        "i need to ",
        "i should ",
        "let me ",
        "now let me ",
        "next let me ",
        "next, let me ",
    ];
    let starts_with_unfulfilled_intent = promise_prefixes
        .iter()
        .any(|prefix| lower.starts_with(prefix));
    let ends_with_incomplete_intent = lower.ends_with(':') && lower.chars().count() < 800;
    if starts_with_unfulfilled_intent || ends_with_incomplete_intent {
        return Some("unfulfilled_tool_intent");
    }

    let repeated_intent_lines = lower
        .lines()
        .map(str::trim)
        .filter(|line| {
            promise_prefixes
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .filter(|line| {
            [
                "check", "find", "grep", "inspect", "look", "open", "read", "recall", "search",
                "verify",
            ]
            .iter()
            .any(|needle| line.contains(needle))
        })
        .take(2)
        .count();
    if repeated_intent_lines >= 2 {
        return Some("repeated_unfulfilled_tool_intent");
    }

    None
}

fn acp_backend_model_id(backend: &AgentBackend) -> Option<LLMId> {
    match backend {
        AgentBackend::Provider => None,
        AgentBackend::Acp(acp) => Some(
            if acp.provider_id.is_empty() {
                crate::ai::acp::acp_selection_identity(&acp.agent_id, &acp.config_values)
            } else {
                crate::ai::acp::acp_provider_selection_identity(
                    &acp.provider_id,
                    &acp.agent_id,
                    &acp.config_values,
                )
            }
            .into(),
        ),
    }
}

impl RequestInput {
    fn for_task(
        inputs: Vec<AIAgentInput>,
        task_id: TaskId,
        active_session: &ModelHandle<ActiveSession>,
        shared_session_response_initiator: Option<ParticipantId>,
        conversation_id: AIConversationId,
        terminal_surface_id: EntityId,
        app: &AppContext,
    ) -> Self {
        let mut me = Self::new_with_common_fields(
            conversation_id,
            active_session,
            shared_session_response_initiator,
            terminal_surface_id,
            app,
        );
        me.input_messages.insert(task_id, inputs);
        me
    }

    fn for_actions_results(
        action_results: Vec<AIAgentActionResult>,
        context: Arc<[AIAgentContext]>,
        active_session: &ModelHandle<ActiveSession>,
        shared_session_response_initiator: Option<ParticipantId>,
        conversation_id: AIConversationId,
        terminal_surface_id: EntityId,
        app: &AppContext,
    ) -> Self {
        let mut me = Self::new_with_common_fields(
            conversation_id,
            active_session,
            shared_session_response_initiator,
            terminal_surface_id,
            app,
        );
        for result in action_results.into_iter() {
            me.input_messages
                .entry(result.task_id.clone())
                .or_default()
                .push(AIAgentInput::ActionResult {
                    result,
                    context: context.clone(),
                });
        }
        me
    }

    pub fn all_inputs(&self) -> impl Iterator<Item = &AIAgentInput> {
        self.input_messages.values().flatten()
    }

    pub fn with_supported_tools(mut self, tools: Vec<ToolType>) -> Self {
        self.supported_tools_override = Some(tools);
        self
    }

    fn new_with_common_fields(
        conversation_id: AIConversationId,
        active_session: &ModelHandle<ActiveSession>,
        shared_session_response_initiator: Option<ParticipantId>,
        terminal_surface_id: EntityId,
        app: &AppContext,
    ) -> Self {
        let llm_prefs = LLMPreferences::as_ref(app);
        let model_id = llm_prefs
            .get_active_base_model(app, Some(terminal_surface_id))
            .id
            .clone();
        let coding_model_id = llm_prefs
            .get_active_coding_model(app, Some(terminal_surface_id))
            .id
            .clone();
        let cli_agent_model_id = llm_prefs
            .get_active_cli_agent_model(app, Some(terminal_surface_id))
            .id
            .clone();
        let computer_use_model_id = llm_prefs
            .get_active_computer_use_model(app, Some(terminal_surface_id))
            .id
            .clone();
        let working_directory = active_session
            .as_ref(app)
            .current_working_directory()
            .cloned();

        Self {
            conversation_id,
            input_messages: Default::default(),
            working_directory,
            model_id,
            coding_model_id,
            cli_agent_model_id,
            computer_use_model_id,
            shared_session_response_initiator,
            request_start_ts: Local::now(),
            supported_tools_override: None,
        }
    }
}

/// Controller for Blocklist AI.
///
/// This is responsible for managing and updating blocklist AI state for a single terminal surface.
pub struct BlocklistAIController {
    active_session: ModelHandle<ActiveSession>,
    input_model: ModelHandle<BlocklistAIInputModel>,
    context_model: ModelHandle<BlocklistAIContextModel>,
    action_model: ModelHandle<BlocklistAIActionModel>,
    terminal_model: Arc<FairMutex<TerminalModel>>,

    in_flight_response_streams: PendingResponseStreams,

    /// The ID of the terminal surface this controller is associated with.
    terminal_surface_id: EntityId,

    should_refresh_available_llms_on_stream_finish: bool,

    shared_session_state: shared_session::SharedSessionState,

    /// Ambient agent task ID attached to this controller. This is a property of the controller, and not an individual
    /// conversation, because the ambient agent task driver owns the entire Warp window working on a task, and any
    /// sessions within it. In the future, one task may span several sessions with background processes.
    ambient_agent_task_id: Option<AmbientAgentTaskId>,

    /// Per-session directory for downloading file attachments.
    /// Set by the agent driver based on the workspace directory (e.g. `{working_dir}/.warp-core/attachments`).
    attachments_download_dir: Option<std::path::PathBuf>,

    /// Pending auto-resume tasks that are waiting for network connectivity.
    /// These should be cancelled when a new request is sent for the same conversation.
    pending_auto_resume_handles: HashMap<AIConversationId, SpawnedFutureHandle>,
    /// Pending dormant Claude wake preparations for success-idle child conversations.
    #[cfg_attr(target_family = "wasm", allow(dead_code))]
    pending_local_claude_wakes: HashMap<AIConversationId, SpawnedFutureHandle>,
    /// Passive conversations explicitly requested to follow up after actions complete.
    pending_passive_follow_ups: HashSet<AIConversationId>,
    /// Conversations with finished action results that should not be drained
    /// until active child agents in their orchestration subtree finish.
    pending_child_blocked_follow_ups: HashSet<AIConversationId>,
    /// Tool proposals that arrived in a provider stream but failed to attach to
    /// conversation history. If a proposal cannot be attached, executing it via
    /// the stream snapshot fallback would create orphaned tool history.
    failed_tool_proposals_by_stream: HashMap<ResponseStreamId, Vec<FailedToolProposal>>,

    /// Per-conversation loop detection state for preventing recursive tool failures.
    loop_detection: HashMap<AIConversationId, ToolLoopGuard>,
    /// Per-conversation error retry count for injecting corrective messages on failure.
    error_retry_counts: HashMap<AIConversationId, usize>,
    /// Passive suggestion results that should be included with the next request
    /// for a given conversation (e.g. accepted/iterated code diffs that weren't
    /// auto-resumed).
    pending_passive_suggestion_results: HashMap<
        AIConversationId,
        Vec<(
            PassiveSuggestionResultType,
            Option<PassiveSuggestionTrigger>,
        )>,
    >,
    /// The crosscheck reviewer model for the "Crosscheck Work" experiment.
    crosscheck_reviewer: ModelHandle<crate::ai::crosscheck::CrosscheckReviewer>,
}

enum InputQueryType {
    /// The user submitted query from the input. This may map to [`AIAgentInput::UserQuery`] but may
    /// map to other `AIAgentInput` types depending on various factors.
    UserSubmittedQueryFromInput {
        query: String,
        static_query_type: Option<StaticQueryType>,
        running_command: Option<RunningCommand>,
    },
    /// A custom [`AIInputType`].
    AIInputType { ai_input: AIAgentInput },
}

enum WhichTask {
    NewConversation,
    Task {
        conversation_id: AIConversationId,
        task_id: TaskId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalClaudeWakeTrigger {
    PendingEvents,
    WakeOnlyStream {
        wake_message: AgentMessageEventMetadata,
    },
}

impl LocalClaudeWakeTrigger {
    #[cfg(not(target_family = "wasm"))]
    fn requires_pending_events(&self) -> bool {
        match self {
            Self::PendingEvents => true,
            Self::WakeOnlyStream { .. } => false,
        }
    }
}

struct InputQuery {
    which_task: WhichTask,
    input_query: InputQueryType,
    /// Additional referenced attachments to include in the query
    /// (e.g. file path references from shared session file uploads).
    additional_attachments: HashMap<String, AIAgentAttachment>,
    /// When `Some`, this submission is a fired queued-prompt row; the send path resolves the
    /// row's stored attachments by this id instead of the live input staging.
    queued_query_id: Option<QueuedQueryId>,
}

#[derive(Clone, Copy)]
struct LiveSteeringEligibility {
    is_user_initiated: bool,
    has_shared_session_participant: bool,
    is_queued_prompt: bool,
    has_queued_query_id: bool,
    has_additional_attachments: bool,
    is_existing_task: bool,
    is_active_conversation: bool,
    has_plain_user_input: bool,
    has_pending_context: bool,
    has_action_context: bool,
    has_pending_passive_results: bool,
}

impl LiveSteeringEligibility {
    fn can_attempt(self) -> bool {
        self.is_user_initiated
            && !self.has_shared_session_participant
            && !self.is_queued_prompt
            && !self.has_queued_query_id
            && !self.has_additional_attachments
            && self.is_existing_task
            && self.is_active_conversation
            && self.has_plain_user_input
            && !self.has_pending_context
            && !self.has_action_context
            && !self.has_pending_passive_results
    }
}

fn is_plain_live_steering_input(
    input_query: &InputQueryType,
    is_same_conversation_running_command_monitor: bool,
) -> bool {
    let InputQueryType::UserSubmittedQueryFromInput {
        query,
        static_query_type,
        running_command,
    } = input_query
    else {
        return false;
    };
    let (_, user_query_mode) = extract_user_query_mode(query.clone());
    !query.trim().is_empty()
        && !query.trim_start().starts_with('/')
        && SlashCommandRequest::from_query(query).is_none()
        && static_query_type.is_none()
        && (running_command.is_none() || is_same_conversation_running_command_monitor)
        && matches!(user_query_mode, UserQueryMode::Normal)
}

impl InputQuery {
    fn query(&self) -> String {
        match &self.input_query {
            InputQueryType::UserSubmittedQueryFromInput { query, .. } => query.clone(),
            InputQueryType::AIInputType { ai_input } => {
                ai_input.display_query().unwrap_or_default()
            }
        }
    }
}

impl BlocklistAIController {
    fn has_unresolved_ask_user_question(
        &self,
        conversation_id: AIConversationId,
        app: &AppContext,
    ) -> bool {
        self.action_model
            .as_ref(app)
            .has_unresolved_ask_user_question_for_conversation(conversation_id, app)
    }

    fn should_block_follow_up_for_unresolved_ask_user_question(
        &self,
        input_query: &InputQuery,
        active_conversation_id: Option<AIConversationId>,
        app: &AppContext,
    ) -> bool {
        self.should_block_submission_for_unresolved_ask_user_question(
            query_targets_existing_conversation(input_query),
            active_conversation_id,
            app,
        )
    }

    pub(super) fn should_block_submission_for_unresolved_ask_user_question(
        &self,
        target_conversation_id: Option<AIConversationId>,
        active_conversation_id: Option<AIConversationId>,
        app: &AppContext,
    ) -> bool {
        if target_conversation_id
            .is_some_and(|target_id| self.has_unresolved_ask_user_question(target_id, app))
        {
            return true;
        }

        active_conversation_id.is_some_and(|active_id| {
            Some(active_id) != target_conversation_id
                && self.has_unresolved_ask_user_question(active_id, app)
        })
    }

    pub(super) fn log_blocked_submission_for_unresolved_ask_user_question(
        &self,
        target_conversation_id: Option<AIConversationId>,
        active_conversation_id: Option<AIConversationId>,
        is_queued_prompt: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        log::warn!(
            "Ignoring user follow-up while AskUserQuestion is unresolved: target_conversation_id={target_conversation_id:?}, active_conversation_id={active_conversation_id:?}"
        );
        #[cfg(not(target_family = "wasm"))]
        remote_logging::log_model_event(
            ctx,
            RemoteLogRecord {
                level: RemoteLogLevel::Warn,
                message: "User follow-up blocked for unresolved AskUserQuestion".to_string(),
                context: serde_json::json!({
                    "event": "user_follow_up_blocked_unresolved_ask_user_question",
                    "target_conversation_id": target_conversation_id.map(|id| id.to_string()),
                    "active_conversation_id": active_conversation_id.map(|id| id.to_string()),
                    "is_queued_prompt": is_queued_prompt,
                }),
            },
        );
    }

    /// Returns the bundled-skill catalog origin for this controller's active session.
    pub fn skill_path_origin(&self, ctx: &AppContext) -> SkillPathOrigin {
        SessionContext::from_session(self.active_session.as_ref(ctx), ctx).skill_path_origin()
    }

    /// Creates a controller for a terminal surface.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_model: ModelHandle<BlocklistAIInputModel>,
        context_model: ModelHandle<BlocklistAIContextModel>,
        conversation_selection: ConversationSelectionHandle,
        action_model: ModelHandle<BlocklistAIActionModel>,
        active_session: ModelHandle<ActiveSession>,
        terminal_model: Arc<FairMutex<TerminalModel>>,
        terminal_surface_id: EntityId,
        ctx: &mut ModelContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&action_model, move |me, _, event, ctx| {
            let BlocklistAIActionEvent::FinishedAction {
                conversation_id,
                cancellation_reason,
                ..
            } = event
            else {
                return;
            };
            // `FinalizedExternally` (e.g. shell exit) means the conversation status and message
            // is set elsewhere through a dedicated path, so we must not trigger a follow-up or update conversation status here.
            let cancellation_outcome =
                cancellation_reason.map(|reason| reason.conversation_outcome());
            if matches!(
                cancellation_outcome,
                Some(CancellationOutcome::FinalizedExternally)
            ) {
                return;
            }
            let action_model = me.action_model.as_ref(ctx);
            if action_model.has_unfinished_actions_for_conversation(*conversation_id) {
                return;
            }

            let history_model = BlocklistAIHistoryModel::handle(ctx);
            let Some((is_viewing_shared_session, is_entirely_passive_code_diff)) = history_model
                .as_ref(ctx)
                .conversation(conversation_id)
                .map(|conversation| {
                    (
                        conversation.is_viewing_shared_session(),
                        conversation.is_entirely_passive_code_diff(),
                    )
                })
            else {
                return;
            };

            // Viewer sessions should not send follow-ups.
            // They only act as passive viewers of the action stream.
            if is_viewing_shared_session {
                return;
            }

            let Some(finished_action_results) =
                action_model.get_finished_action_results(*conversation_id)
            else {
                return;
            };
            let is_passive_code_diff = is_entirely_passive_code_diff
                && finished_action_results.last().is_some_and(|result| {
                    matches!(result.result, AIAgentActionResultType::RequestFileEdits(_))
                });
            let has_manual_follow_up = me.pending_passive_follow_ups.contains(conversation_id);

            // A `Succeeded` cancellation (e.g. an optimistic long-running-command
            // completion or a revert) is itself the terminal result, so it neither
            // triggers a follow-up nor counts as a cancellation.
            let treat_as_success =
                matches!(cancellation_outcome, Some(CancellationOutcome::Succeeded));
            let should_trigger_follow_up_request = (!is_passive_code_diff
                && !treat_as_success
                && finished_action_results
                    .iter()
                    .any(|result| result.result.should_trigger_request_upon_completion()))
                || has_manual_follow_up;
            if !should_trigger_follow_up_request {
                if matches!(
                    cancellation_outcome,
                    Some(CancellationOutcome::KeepInProgress)
                ) {
                    return;
                }
                // We also check if there's an in-flight req, because it's possible that this
                // subscription callback was queued in response to auto-cancelling pending actions
                // in the process of constructing a request. In such cases, we don't want to update
                // conversation status to Cancelled/Success.
                if !me
                    .in_flight_response_streams
                    .has_active_stream_for_conversation(*conversation_id, ctx)
                {
                    // If the completed actions do not trigger a follow-up request, update conversation
                    // status based on the outcome of the actions.
                    //
                    // (It would otherwise remain `InProgress`, which would be correct, since we'd be
                    // immediately triggering a follow-up request).
                    //
                    // In practice, the only time where this codepath gets triggered is upon completion
                    // of a passive code diff action, where we don't autosend the next request.
                    //
                    // With passive code diffs, its most appropriate to mark the conversation
                    // successful if the passive diff was accepted. In practice, there's only ever
                    // one RequestFileEdits action, so `finished_action_results` at this point
                    // should only have a single element.
                    //
                    // If the user does end up following up on the passive diff-originated conversation,
                    // the status will once again be updated to `InProgress`.
                    let updated_conversation_status = if finished_action_results
                        .iter()
                        .all(|result| result.result.is_successful())
                        || treat_as_success
                    {
                        ConversationStatus::Success
                    } else {
                        // This is an imperfect heuristic that practically speaking should have no effect.
                        //
                        // If we actually need to differentiate between the state of a conversation
                        // where actions completed with mixed result statuses (e.g. a mix of
                        // cancelled, error, and success) _and_ we don't automatically send back action
                        // results to the agent, then it'd be worth considering adding a new status
                        // variant.
                        ConversationStatus::Cancelled
                    };
                    history_model.update(ctx, |history_model, ctx| {
                        history_model.update_conversation_status(
                            me.terminal_surface_id,
                            *conversation_id,
                            updated_conversation_status,
                            ctx,
                        );
                    });
                }
                return;
            }
            me.send_follow_up_for_conversation(*conversation_id, ctx);
        });

        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, |me, _, event, ctx| {
            let BlocklistAIHistoryEvent::UpdatedConversationStatus {
                terminal_surface_id,
                new_status,
                ..
            } = event
            else {
                return;
            };
            if *terminal_surface_id != me.terminal_surface_id || !new_status.is_done() {
                return;
            }

            let pending_parents = me
                .pending_child_blocked_follow_ups
                .iter()
                .copied()
                .collect::<Vec<_>>();
            for parent_id in pending_parents {
                me.maybe_resume_child_blocked_follow_up(parent_id, ctx);
            }
        });

        ctx.subscribe_to_model(&conversation_selection, |me, _, event, ctx| {
            let ConversationSelectionEvent::Deactivated {
                conversation_id,
                final_exchange_count,
                is_exit_before_new_entrance,
            } = event
            else {
                return;
            };
            if *is_exit_before_new_entrance || *final_exchange_count == 0 {
                return;
            }
            let history = BlocklistAIHistoryModel::handle(ctx);
            let Some(conversation) = history.as_ref(ctx).conversation(conversation_id) else {
                return;
            };
            if conversation.is_viewing_shared_session() {
                return;
            }
            if conversation.status().is_in_progress() {
                me.cancel_conversation_progress(
                    *conversation_id,
                    CancellationReason::ManuallyCancelled,
                    ctx,
                );
            }
        });
        // Subscribe to the orchestration event service to inject events
        // (e.g. MessagesReceivedFromAgents) into conversations that receive inter-agent messages.
        let svc = OrchestrationEventService::handle(ctx);
        ctx.subscribe_to_model(&svc, move |me, _, event, ctx| {
            let OrchestrationEventServiceEvent::EventsReady { conversation_id } = event;
            me.handle_pending_events_ready(*conversation_id, ctx);
        });
        let streamer = OrchestrationEventStreamer::handle(ctx);
        ctx.subscribe_to_model(&streamer, move |me, _, event, ctx| match event {
            OrchestrationEventStreamerEvent::DormantClaudeWakeReady {
                conversation_id,
                wake_message,
            } => {
                me.handle_dormant_claude_wake_ready(*conversation_id, wake_message.clone(), ctx);
            }
            // Viewer-mode placeholder materialization is handled by
            // `OrchestrationViewerModel`; the owner-side controller only
            // mirrors status changes for already-known child conversations.
            OrchestrationEventStreamerEvent::ChildSpawned { .. } => {}
            OrchestrationEventStreamerEvent::ChildStatusChanged { run_id, status, .. } => {
                me.handle_orchestrated_child_status_changed(run_id, status.clone(), ctx);
            }
        });
        let crosscheck_reviewer = ctx.add_model(crate::ai::crosscheck::CrosscheckReviewer::new);
        ctx.subscribe_to_model(&crosscheck_reviewer, move |me, _, event, ctx| {
            use crate::ai::crosscheck::{CrosscheckReviewerEvent, ReviewOutcome};
            let CrosscheckReviewerEvent::ReviewCompleted {
                conversation_id,
                outcome,
            } = event;
            me.handle_crosscheck_review_completed(*conversation_id, outcome.clone(), ctx);
        });
        Self {
            input_model,
            context_model,
            action_model,
            active_session,
            terminal_model,
            in_flight_response_streams: PendingResponseStreams::new(),
            terminal_surface_id,
            should_refresh_available_llms_on_stream_finish: false,
            shared_session_state: shared_session::SharedSessionState::default(),
            ambient_agent_task_id: None,
            attachments_download_dir: None,
            pending_auto_resume_handles: HashMap::new(),
            pending_local_claude_wakes: HashMap::new(),
            pending_passive_follow_ups: HashSet::new(),
            pending_child_blocked_follow_ups: HashSet::new(),
            failed_tool_proposals_by_stream: HashMap::new(),
            pending_passive_suggestion_results: HashMap::new(),
            loop_detection: HashMap::new(),
            error_retry_counts: HashMap::new(),
            crosscheck_reviewer,
        }
    }

    /// Internal method to send a query to the AI model. External callers should use either
    /// `send_user_query_in_conversation`, `send_user_in_conversation`, or
    /// `send_custom_ai_input_query` instead.
    ///
    /// When the request is sent, a `BlocklistAIEvent::SentRequest` event is emitted containing the
    /// query itself as well as a oneshot `Receiver` that can be `await`-ed to receive the response
    /// from the AI.
    fn send_query(
        &mut self,
        input_query: InputQuery,
        entrypoint_type: EntrypointType,
        // The shared session participant who initiated this query
        // (None if this is not a shared session).
        shared_session_participant_id: Option<ParticipantId>,
        is_queued_prompt: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let has_shared_session_participant = shared_session_participant_id.is_some();
        // Store the participant who initiated this query before sending
        // so that send_query can use it when creating the exchange.
        if let Some(participant_id) = shared_session_participant_id {
            self.set_current_response_initiator(participant_id);
        }

        let query = input_query.query().to_owned();
        let is_existing_task = matches!(&input_query.which_task, WhichTask::Task { .. });
        let active_conversation_id =
            BlocklistAIHistoryModel::as_ref(ctx).active_conversation_id(self.terminal_surface_id);
        if self.should_block_follow_up_for_unresolved_ask_user_question(
            &input_query,
            active_conversation_id,
            ctx,
        ) {
            self.log_blocked_submission_for_unresolved_ask_user_question(
                query_targets_existing_conversation(&input_query),
                active_conversation_id,
                input_query.queued_query_id.is_some(),
                ctx,
            );
            return;
        }
        let (conversation_id, task_id) = match &input_query.which_task {
            WhichTask::NewConversation => {
                let conversation = self.start_new_conversation_for_request(ctx);
                (conversation.id(), conversation.get_root_task_id().clone())
            }
            WhichTask::Task {
                conversation_id,
                task_id,
            } => (*conversation_id, task_id.clone()),
        };

        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
            history.refresh_conversation_backend_without_output(conversation_id, ctx);
        });

        let is_same_conversation_running_command_monitor = match &input_query.input_query {
            InputQueryType::UserSubmittedQueryFromInput {
                running_command: Some(running_command),
                ..
            } => {
                let terminal_model = self.terminal_model.lock();
                running_command_belongs_to_monitor(
                    &terminal_model,
                    conversation_id,
                    running_command,
                )
            }
            InputQueryType::UserSubmittedQueryFromInput {
                running_command: None,
                ..
            }
            | InputQueryType::AIInputType { .. } => false,
        };
        let has_simple_user_input = is_plain_live_steering_input(
            &input_query.input_query,
            is_same_conversation_running_command_monitor,
        );
        let has_pending_context = {
            let context_model = self.context_model.as_ref(ctx);
            !context_model.pending_context_block_ids().is_empty()
                || context_model.pending_context_selected_text().is_some()
                || !context_model.pending_attachments().is_empty()
                || context_model.pending_document_id().is_some()
        };
        let has_action_context = {
            let action_model = self.action_model.as_ref(ctx);
            action_model.has_unfinished_actions_for_conversation(conversation_id)
                || action_model
                    .get_finished_action_results(conversation_id)
                    .is_some_and(|results| !results.is_empty())
        };
        let can_attempt_live_steering = LiveSteeringEligibility {
            is_user_initiated: matches!(entrypoint_type, EntrypointType::UserInitiated),
            has_shared_session_participant,
            is_queued_prompt,
            has_queued_query_id: input_query.queued_query_id.is_some(),
            has_additional_attachments: !input_query.additional_attachments.is_empty(),
            is_existing_task,
            is_active_conversation: active_conversation_id
                .as_ref()
                .is_some_and(|id| *id == conversation_id),
            has_plain_user_input: has_simple_user_input,
            has_pending_context,
            has_action_context,
            has_pending_passive_results: self
                .pending_passive_suggestion_results
                .get(&conversation_id)
                .is_some_and(|results| !results.is_empty()),
        }
        .can_attempt();
        if can_attempt_live_steering {
            if let Some((stream_id, model_id)) = self
                .in_flight_response_streams
                .try_steer_runtime_for_conversation(conversation_id, query.clone(), ctx)
            {
                ctx.emit(BlocklistAIControllerEvent::SentRequest {
                    contains_user_query: true,
                    is_queued_prompt: false,
                    model_id,
                    stream_id,
                });
                ctx.dispatch_global_action("workspace:save_app", ());
                return;
            }
        }

        // Drain any queued passive suggestion results for this conversation
        // *before* cancelling progress, since cancel_conversation_progress
        // clears the pending map.
        let pending_passive_results = self
            .pending_passive_suggestion_results
            .remove(&conversation_id)
            .unwrap_or_default();

        let cancellation_reason = CancellationReason::FollowUpSubmitted {
            is_for_same_conversation: active_conversation_id
                .is_some_and(|id| id == conversation_id),
        };
        if let Some(active_conversation_id) = active_conversation_id {
            self.cancel_conversation_progress(active_conversation_id, cancellation_reason, ctx);
        }

        if let Some(slash_command_request) = SlashCommandRequest::from_query(query.as_str()) {
            // Only fired queued rows carry `queued_query_id`. For those rows, keep slash commands
            // (e.g. queued `/compact`) on the conversation they were queued on; direct slash
            // submissions still re-derive their target from the current UI selection.
            let conversation_id_override = input_query
                .queued_query_id
                .is_some()
                .then_some(conversation_id);
            slash_command_request.send_request(
                self,
                input_query.queued_query_id,
                conversation_id_override,
                ctx,
            );
            return;
        }

        let (query, user_query_mode) = extract_user_query_mode(query);

        // Attribute /orchestrate queries to the slash-command entry surface.
        if matches!(user_query_mode, UserQueryMode::Orchestrate) {
            send_telemetry_from_ctx!(
                super::telemetry::BlocklistOrchestrationTelemetryEvent::OrchestrationEntered(
                    super::telemetry::OrchestrationEnteredEvent {
                        conversation_id,
                        plan_id: None,
                        entry_source:
                            super::telemetry::OrchestrationEntrySource::SlashCommandOrchestrate,
                    }
                ),
                ctx
            );
        }

        let should_prepend_finished_action_results = matches!(
            input_query.input_query,
            InputQueryType::UserSubmittedQueryFromInput { .. }
        );

        let completed_action_results = self.action_model.update(ctx, |action_model, ctx| {
            action_model.cancel_all_pending_actions(
                conversation_id,
                Some(cancellation_reason),
                ctx,
            );
            action_model.drain_finished_action_results(conversation_id)
        });

        let context = input_context_for_request(
            false,
            self.context_model.as_ref(ctx),
            self.active_session.as_ref(ctx),
            vec![],
            ctx,
        );
        let mut inputs = if should_prepend_finished_action_results {
            completed_action_results
                .into_iter()
                .map(|result| AIAgentInput::ActionResult {
                    result,
                    context: context.clone(),
                })
                .collect_vec()
        } else {
            // Custom AI inputs like CodeReview and FetchReviewComments are encoded as
            // top-level request variants (`request::input::Type::CodeReview`,
            // `request::input::Type::FetchReviewComments`, etc.), and `convert_input`
            // only emits those variants in the single-input path.
            //
            // Tool call results are encoded differently: they only exist inside
            // `request::input::Type::UserInputs` as `user_input::Input::ToolCallResult`.
            // There is no proto request shape that can represent both a top-level
            // CodeReview-style input and a ToolCallResult in the same request.
            //
            // So if we prepend an ActionResult here, `convert_input` has to fall back
            // to the multi-input `UserInputs` path, where CodeReview / FetchReviewComments
            // are ignored entirely. The stale tool result is preserved, but the custom
            // AI input disappears from the request.
            vec![]
        };

        // Append any queued passive suggestion results that were drained
        // earlier (before cancel_conversation_progress).
        for (suggestion, trigger) in pending_passive_results {
            inputs.push(AIAgentInput::PassiveSuggestionResult {
                trigger,
                suggestion,
                context: context.clone(),
            });
        }

        let additional_attachments = input_query.additional_attachments;
        let queued_query_id = input_query.queued_query_id;
        let ai_input = match input_query.input_query {
            InputQueryType::UserSubmittedQueryFromInput {
                static_query_type,
                running_command,
                ..
            } => {
                // Resolve the attachment set for this submission. The direct-send branch
                // preserves existing behavior: live input staging is still consumed by regular
                // submissions, but fired queued rows read from their row-owned attachment set.
                let prompt_attachments = match queued_query_id {
                    Some(query_id) => QueuedQueryModel::as_ref(ctx)
                        .attachments_for(conversation_id, query_id)
                        .to_vec(),
                    None => self
                        .context_model
                        .as_ref(ctx)
                        .pending_attachments()
                        .to_vec(),
                };

                input_for_query(
                    query,
                    &task_id,
                    conversation_id,
                    static_query_type,
                    user_query_mode,
                    running_command,
                    additional_attachments,
                    prompt_attachments,
                    self.context_model.as_ref(ctx),
                    self.active_session.as_ref(ctx),
                    ctx,
                )
            }
            InputQueryType::AIInputType { ai_input } => ai_input,
        };
        inputs.push(ai_input);

        // Piggyback any pending orchestration config updates for this conversation.
        let taken_dirty_events = AIDocumentModel::handle(ctx).update(ctx, |model, _| {
            model.take_dirty_orchestration_events(&conversation_id)
        });
        for dirty_event in &taken_dirty_events {
            inputs.push(AIAgentInput::OrchestrationConfigUpdate {
                plan_id: dirty_event.plan_id.clone(),
                config: dirty_event.config.clone(),
                status: dirty_event.status,
            });
        }

        let send_result = self.send_request_input(
            RequestInput::for_task(
                inputs,
                task_id,
                &self.active_session,
                self.get_current_response_initiator(),
                conversation_id,
                self.terminal_surface_id,
                ctx,
            ),
            Some(RequestMetadata {
                is_autodetected_user_query: !self.input_model.as_ref(ctx).is_input_type_locked(),
                entrypoint: entrypoint_type,
                is_auto_resume_after_error: false,
            }),
            /*can_attempt_resume_on_error*/ true,
            is_queued_prompt,
            ctx,
        );

        // If the request failed, re-insert the dirty events so they aren't
        // silently lost.
        if let Err(e) = &send_result {
            log::error!("Failed to send agent request: {e:?}");
            if !taken_dirty_events.is_empty() {
                AIDocumentModel::handle(ctx).update(ctx, |model, _| {
                    model.set_dirty_orchestration_events(conversation_id, taken_dirty_events);
                });
            }
        }
    }

    /// Populates plan documents from user query to AIDocumentModel if not already present.
    /// Parses attachments from query and creates AI documents for any user-attached plans.
    /// This is split from parse_context_attachments to run later in the pipeline when new conversations are created.
    fn maybe_populate_plans_for_ai_document_model(
        &self,
        referenced_attachments: &HashMap<String, AIAgentAttachment>,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        // Get file link resolution context from active session
        let session = self.active_session.as_ref(ctx);
        let file_link_resolution_context =
            session
                .current_working_directory()
                .cloned()
                .map(|working_directory| FileLinkResolutionContext {
                    working_directory,
                    shell_launch_data: session.shell_launch_data(ctx),
                });

        for attachment in referenced_attachments.values() {
            let AIAgentAttachment::DocumentContent {
                document_id,
                content,
                source,
                ..
            } = attachment
            else {
                continue;
            };
            if !matches!(*source, DocumentContentAttachmentSource::UserAttached) {
                continue;
            }
            let document_id = match AIDocumentId::try_from(document_id.as_str()) {
                Ok(id) => id,
                Err(_) => {
                    log::warn!("Invalid ai_document_id in document content: {document_id}");
                    continue;
                }
            };

            // Skip if document already exists in the model
            let ai_document_model = AIDocumentModel::as_ref(ctx);
            if ai_document_model
                .get_current_document(&document_id)
                .is_some()
            {
                continue;
            }

            // Look up notebook to get title and sync_id
            let cloud_model = CloudModel::as_ref(ctx);
            let notebook_data = cloud_model
                .get_all_active_notebooks()
                .find(|nb| nb.model().ai_document_id.as_ref() == Some(&document_id))
                .map(|nb| (nb.model().title.clone(), nb.id));

            if let Some((title, sync_id)) = notebook_data {
                AIDocumentModel::handle(ctx).update(ctx, |model, model_ctx| {
                    model.create_document_from_notebook(
                        document_id,
                        sync_id,
                        title,
                        content,
                        conversation_id,
                        file_link_resolution_context.clone(),
                        model_ctx,
                    );
                });
            } else {
                log::warn!("Notebook not found for ai_document_id: {document_id}");
            }
        }
    }

    pub fn send_user_query_in_new_conversation(
        &mut self,
        query: String,
        static_query_type: Option<StaticQueryType>,
        entrypoint_type: EntrypointType,
        participant_id: Option<ParticipantId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_new_conversation_internal(
            query,
            static_query_type,
            entrypoint_type,
            participant_id,
            /*is_queued_prompt*/ false,
            /*queued_query_id*/ None,
            ctx,
        );
    }

    /// Sends the first submission of a previously queued user prompt into a new conversation.
    /// Same as [`Self::send_user_query_in_new_conversation`] but marks the emitted
    /// `SentRequest` event so UI subscribers (e.g. the input editor) know not to treat
    /// this as a direct user submission and therefore not clear the input buffer.
    pub fn send_queued_user_query_in_new_conversation(
        &mut self,
        query: String,
        static_query_type: Option<StaticQueryType>,
        entrypoint_type: EntrypointType,
        participant_id: Option<ParticipantId>,
        queued_query_id: QueuedQueryId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_new_conversation_internal(
            query,
            static_query_type,
            entrypoint_type,
            participant_id,
            /*is_queued_prompt*/ true,
            Some(queued_query_id),
            ctx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn send_user_query_in_new_conversation_internal(
        &mut self,
        query: String,
        static_query_type: Option<StaticQueryType>,
        entrypoint_type: EntrypointType,
        participant_id: Option<ParticipantId>,
        is_queued_prompt: bool,
        queued_query_id: Option<QueuedQueryId>,
        ctx: &mut ModelContext<Self>,
    ) {
        let participant_id = participant_id.or_else(|| self.get_sharer_participant_id());
        let running_command = {
            let terminal_model = self.terminal_model.lock();
            get_running_command(&terminal_model)
        };
        if let Some(running_command) = running_command {
            let conversation_id = self.start_new_conversation_for_request(ctx).id();
            let history_model = BlocklistAIHistoryModel::handle(ctx);
            let task_id = match history_model.update(ctx, |history_model, ctx| {
                history_model.create_cli_subagent_task_for_conversation(
                    running_command.block_id.clone(),
                    conversation_id,
                    self.terminal_surface_id,
                    ctx,
                )
            }) {
                Ok(task_id) => task_id,
                Err(e) => {
                    log::error!("Could not create CLI subagent task optimistically: {e:?}");
                    return;
                }
            };
            self.send_query(
                InputQuery {
                    which_task: WhichTask::Task {
                        conversation_id,
                        task_id,
                    },
                    input_query: InputQueryType::UserSubmittedQueryFromInput {
                        query,
                        static_query_type,
                        running_command: Some(running_command),
                    },
                    additional_attachments: HashMap::new(),
                    queued_query_id,
                },
                entrypoint_type,
                participant_id,
                is_queued_prompt,
                ctx,
            );
        } else {
            self.send_query(
                InputQuery {
                    which_task: WhichTask::NewConversation,
                    input_query: InputQueryType::UserSubmittedQueryFromInput {
                        query,
                        static_query_type,
                        running_command: None,
                    },
                    additional_attachments: HashMap::new(),
                    queued_query_id,
                },
                entrypoint_type,
                participant_id,
                is_queued_prompt,
                ctx,
            );
        }
    }

    /// Sends a query into an existing conversation as an agent-initiated request.
    /// This is the agent-initiated counterpart to `send_user_query_in_conversation`.
    pub fn send_agent_query_in_conversation(
        &mut self,
        query: String,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_conversation_internal(
            query,
            conversation_id,
            None,
            RunningCommandDetection::Detect,
            HashMap::new(),
            EntrypointType::AgentInitiated,
            /*is_queued_prompt*/ false,
            /*queued_query_id*/ None,
            ctx,
        );
    }

    /// Sends one non-preemptive final assessment to a completed CLI-monitor task.
    ///
    /// This deliberately bypasses `send_query`: command completion must not cancel
    /// another conversation, drain unrelated action results, or replace a request
    /// that is still delivering the command's final tool result.
    pub fn send_command_completion_assessment(
        &mut self,
        conversation_id: AIConversationId,
        task_id: TaskId,
        query: String,
        completed_command: RunningCommand,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        if self
            .in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, ctx)
            || self
                .action_model
                .as_ref(ctx)
                .has_unfinished_actions_for_conversation(conversation_id)
        {
            return false;
        }

        let context = input_context_for_request(
            false,
            self.context_model.as_ref(ctx),
            self.active_session.as_ref(ctx),
            vec![],
            ctx,
        );
        let request_input = RequestInput::for_task(
            vec![AIAgentInput::UserQuery {
                query,
                context,
                static_query_type: None,
                referenced_attachments: HashMap::new(),
                user_query_mode: UserQueryMode::Normal,
                running_command: Some(completed_command),
                intended_agent: None,
            }],
            task_id,
            &self.active_session,
            self.get_current_response_initiator(),
            conversation_id,
            self.terminal_surface_id,
            ctx,
        )
        .with_supported_tools(vec![]);

        self.send_request_input(
            request_input,
            Some(RequestMetadata {
                is_autodetected_user_query: false,
                entrypoint: EntrypointType::AgentInitiated,
                is_auto_resume_after_error: false,
            }),
            /*can_attempt_resume_on_error*/ false,
            /*is_queued_prompt*/ false,
            ctx,
        )
        .is_ok()
    }

    /// Sends the given user query to the AI model.
    pub fn send_user_query_in_conversation(
        &mut self,
        query: String,
        conversation_id: AIConversationId,
        participant_id: Option<ParticipantId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_conversation_internal(
            query,
            conversation_id,
            participant_id,
            RunningCommandDetection::Detect,
            HashMap::new(),
            EntrypointType::UserInitiated,
            /*is_queued_prompt*/ false,
            /*queued_query_id*/ None,
            ctx,
        );
    }

    /// Sends the first submission of a previously queued user prompt into an existing conversation.
    /// Same as [`Self::send_user_query_in_conversation`] but marks the emitted `SentRequest`
    /// event so UI subscribers (e.g. the input editor) know not to treat this as a direct
    /// user submission and therefore not clear the input buffer.
    pub fn send_queued_user_query_in_conversation(
        &mut self,
        query: String,
        conversation_id: AIConversationId,
        participant_id: Option<ParticipantId>,
        queued_query_id: QueuedQueryId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_conversation_internal(
            query,
            conversation_id,
            participant_id,
            RunningCommandDetection::Detect,
            HashMap::new(),
            EntrypointType::UserInitiated,
            /*is_queued_prompt*/ true,
            Some(queued_query_id),
            ctx,
        );
    }

    /// Sends the given user query to the AI model, with additional referenced attachments.
    pub fn send_user_query_in_conversation_with_attachments(
        &mut self,
        query: String,
        conversation_id: AIConversationId,
        participant_id: Option<ParticipantId>,
        additional_attachments: HashMap<String, AIAgentAttachment>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_conversation_internal(
            query,
            conversation_id,
            participant_id,
            RunningCommandDetection::Detect,
            additional_attachments,
            EntrypointType::UserInitiated,
            /*is_queued_prompt*/ false,
            /*queued_query_id*/ None,
            ctx,
        );
    }

    /// Sends the given user query to the AI model, skipping long running command detection.
    /// We use this when we fork a conversation and immediately send an initial query, to avoid
    /// a race condition where restored command blocks may appear long running when the initial query is sent,
    /// causing the query to go to the lrc subagent.
    pub fn send_user_query_in_conversation_no_lrc_subagent(
        &mut self,
        query: String,
        conversation_id: AIConversationId,
        participant_id: Option<ParticipantId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_conversation_internal(
            query,
            conversation_id,
            participant_id,
            RunningCommandDetection::Skip,
            HashMap::new(),
            EntrypointType::UserInitiated,
            /*is_queued_prompt*/ false,
            /*queued_query_id*/ None,
            ctx,
        );
    }

    /// Nudges a CLI monitor that ended a turn without proposing a polling action. The running
    /// command is attached through normal long-running-command detection so Rig and the legacy
    /// provider path both receive the monitor-specific prompt and tool set.
    pub fn send_cli_monitor_nudge(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.send_user_query_in_conversation_internal(
            "The command is still running. Continue monitoring now: call `read_shell_command_output` \
             with the existing command ID instead of replying with a status message. If the user's \
             explicit stop condition is met, call `interrupt_shell_command` immediately."
                .to_owned(),
            conversation_id,
            None,
            RunningCommandDetection::Detect,
            HashMap::new(),
            EntrypointType::AgentInitiated,
            /*is_queued_prompt*/ false,
            /*queued_query_id*/ None,
            ctx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn send_user_query_in_conversation_internal(
        &mut self,
        query: String,
        conversation_id: AIConversationId,
        participant_id: Option<ParticipantId>,
        running_command_detection: RunningCommandDetection,
        additional_attachments: HashMap<String, AIAgentAttachment>,
        entrypoint_type: EntrypointType,
        is_queued_prompt: bool,
        queued_query_id: Option<QueuedQueryId>,
        ctx: &mut ModelContext<Self>,
    ) {
        // User sending a new query resets loop detection and error retry state — fresh context.
        self.loop_detection.remove(&conversation_id);
        self.error_retry_counts.remove(&conversation_id);
        // Reset any in-flight crosscheck review for this conversation.
        self.crosscheck_reviewer.update(ctx, |reviewer, _| {
            reviewer.reset_review(conversation_id);
        });

        let is_viewer = self
            .terminal_model
            .lock()
            .shared_session_status()
            .is_viewer();
        if is_viewer {
            log::error!("Viewers should never attempt to send queries directly");
        }

        // Ensure we capture all pending context blocks before promoting and attaching them to the conversation.
        let context_block_ids = self
            .context_model
            .as_ref(ctx)
            .pending_context_block_ids()
            .clone();

        let (promoted_blocks, task_id, running_command) = {
            let mut terminal_model = self.terminal_model.lock();

            let running_command_opt = match running_command_detection {
                RunningCommandDetection::Detect => {
                    get_running_command_for_conversation(&terminal_model, conversation_id)
                }
                RunningCommandDetection::Skip => None,
            };

            terminal_model
                .block_list_mut()
                .associate_blocks_with_conversation(context_block_ids.iter(), conversation_id);

            // Promote all blocks that are pending for this conversation to attached.
            // This happens at query submission time, making blocks permanently associated with the conversation.
            let promoted_blocks = terminal_model
                .block_list_mut()
                .promote_blocks_to_attached_from_conversation(conversation_id);

            let active_block = terminal_model.block_list().active_block();
            let existing_cli_task_id = active_block
                .is_agent_monitoring()
                .then(|| active_block.agent_interaction_metadata())
                .flatten()
                .filter(|metadata| metadata.conversation_id() == &conversation_id)
                .and_then(|metadata| metadata.subagent_task_id().cloned());

            // Steering for a command that already has a monitor must remain on
            // that monitor's task. Creating another optimistic CLI task here
            // replaces the active task ID and strands the previous exchange.
            // Keep attaching the current running-command snapshot so the
            // direct provider continues selecting the CLI-agent model.
            let (task_id, running_command) = if let Some(task_id) = existing_cli_task_id {
                (task_id, running_command_opt)
            } else if let Some(running_command) = running_command_opt {
                let history_model = BlocklistAIHistoryModel::handle(ctx);
                match history_model.update(ctx, |history_model, ctx| {
                    history_model.create_cli_subagent_task_for_conversation(
                        running_command.block_id.clone(),
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    )
                }) {
                    Ok(task_id) => (task_id, Some(running_command)),
                    Err(e) => {
                        log::error!("Could not create CLI subagent task optimistically: {e:?}");
                        return;
                    }
                }
            } else {
                let history_model = BlocklistAIHistoryModel::as_ref(ctx);
                let Some(conversation) = history_model.conversation(&conversation_id) else {
                    log::error!(
                        "Tried to send follow-up query for non-existent conversation: {conversation_id:?}"
                    );
                    return;
                };

                (conversation.get_root_task_id().clone(), None)
            };

            (promoted_blocks, task_id, running_command)
        };

        // Persist the updated visibility for each promoted block
        if !promoted_blocks.is_empty() {
            if let Some(sender) = GlobalResourceHandlesProvider::as_ref(ctx)
                .get()
                .model_event_sender
                .as_ref()
            {
                for (block_id, agent_view_visibility) in promoted_blocks {
                    if let Err(e) = sender.send(ModelEvent::UpdateBlockAgentViewVisibility {
                        block_id: block_id.to_string(),
                        agent_view_visibility: agent_view_visibility.into(),
                    }) {
                        log::error!("Error sending UpdateBlockAgentViewVisibility event: {e:?}");
                    }
                }
            }
        }

        let participant_id = participant_id.or_else(|| self.get_sharer_participant_id());
        self.send_query(
            InputQuery {
                which_task: WhichTask::Task {
                    conversation_id,
                    task_id,
                },
                input_query: InputQueryType::UserSubmittedQueryFromInput {
                    query,
                    static_query_type: None,
                    running_command,
                },
                additional_attachments,
                queued_query_id,
            },
            entrypoint_type,
            participant_id,
            is_queued_prompt,
            ctx,
        );
    }

    /// Sends a request triggered by a zero-state prompt suggestion.
    pub fn send_zero_state_prompt_suggestion(
        &mut self,
        query_type: ZeroStatePromptSuggestionType,
        ctx: &mut ModelContext<Self>,
    ) {
        let participant_id = self.get_sharer_participant_id();
        self.send_query(
            InputQuery {
                which_task: WhichTask::NewConversation,
                input_query: InputQueryType::UserSubmittedQueryFromInput {
                    query: query_type.query().to_string(),
                    static_query_type: query_type.static_query_type(),
                    running_command: None,
                },
                additional_attachments: HashMap::new(),
                queued_query_id: None,
            },
            EntrypointType::ZeroStateAgentModePromptSuggestion,
            participant_id,
            /*is_queued_prompt*/ false,
            ctx,
        );
    }

    /// Sends a custom [`AIAgentInput`] query.
    pub fn send_custom_ai_input_query(
        &mut self,
        ai_input: AIAgentInput,
        ctx: &mut ModelContext<Self>,
    ) {
        let participant_id = self.get_sharer_participant_id();
        let which_task = match self.context_model.as_ref(ctx).selected_conversation_id(ctx) {
            Some(id) => {
                let Some(conversation) = BlocklistAIHistoryModel::as_ref(ctx).conversation(&id)
                else {
                    log::error!(
                        "Tried to send custom AI input query as follow-up in non-existent conversation"
                    );
                    return;
                };
                WhichTask::Task {
                    conversation_id: conversation.id(),
                    task_id: conversation.get_root_task_id().clone(),
                }
            }
            None => WhichTask::NewConversation,
        };
        self.send_query(
            InputQuery {
                which_task,
                input_query: InputQueryType::AIInputType { ai_input },
                additional_attachments: HashMap::new(),
                queued_query_id: None,
            },
            EntrypointType::UserInitiated,
            participant_id,
            /*is_queued_prompt*/ false,
            ctx,
        )
    }

    pub fn send_slash_command_request(
        &mut self,
        slash_command: SlashCommandRequest,
        ctx: &mut ModelContext<Self>,
    ) {
        slash_command.send_request(self, None, None, ctx);
    }

    /// Same as [`Self::send_slash_command_request`] but marks the emitted `SentRequest`
    /// event as a queued prompt submission so UI subscribers (e.g. the input editor)
    /// don't clear the input buffer on the auto-send.
    pub fn send_queued_slash_command_request(
        &mut self,
        slash_command: SlashCommandRequest,
        queued_query_id: QueuedQueryId,
        conversation_id: Option<AIConversationId>,
        ctx: &mut ModelContext<Self>,
    ) {
        slash_command.send_request(self, Some(queued_query_id), conversation_id, ctx);
    }

    /// Mark a conversation to follow up after its actions complete and attempt to send immediately
    /// if results are already available.
    pub fn request_follow_up_after_actions(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.pending_passive_follow_ups.insert(conversation_id);

        if self
            .in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, ctx)
        {
            return;
        }

        let has_pending_actions = self
            .action_model
            .as_ref(ctx)
            .get_pending_actions_for_conversation(&conversation_id)
            .next()
            .is_some();
        if has_pending_actions {
            return;
        }

        let finished_action_results = self
            .action_model
            .as_ref(ctx)
            .get_finished_action_results(conversation_id);
        if finished_action_results.is_some_and(|results| !results.is_empty()) {
            self.send_follow_up_for_conversation(conversation_id, ctx);
        }
    }

    /// Sends a custom AI input, building context from the current session.
    pub fn send_ai_input_with_context(
        &mut self,
        build_input: impl FnOnce(Arc<[AIAgentContext]>) -> AIAgentInput,
        ctx: &mut ModelContext<Self>,
    ) {
        let context = input_context_for_request(
            false,
            self.context_model.as_ref(ctx),
            self.active_session.as_ref(ctx),
            vec![],
            ctx,
        );
        self.send_custom_ai_input_query(build_input(context), ctx);
    }

    /// Sends the result of a passive suggestion (accepted/rejected code diff or
    /// prompt) back to the model so it can continue with accurate context.
    pub fn send_passive_suggestion_result(
        &mut self,
        conversation_id: Option<AIConversationId>,
        suggestion: PassiveSuggestionResultType,
        trigger: Option<PassiveSuggestionTrigger>,
        ctx: &mut ModelContext<Self>,
    ) {
        let which_task = match conversation_id {
            Some(id) => {
                let Some(conversation) = BlocklistAIHistoryModel::as_ref(ctx).conversation(&id)
                else {
                    log::error!("[passive-suggestion-result] conversation not found for id {id:?}");
                    return;
                };
                WhichTask::Task {
                    conversation_id: conversation.id(),
                    task_id: conversation.get_root_task_id().clone(),
                }
            }
            None => WhichTask::NewConversation,
        };

        let context = input_context_for_request(
            false,
            self.context_model.as_ref(ctx),
            self.active_session.as_ref(ctx),
            vec![],
            ctx,
        );

        let participant_id = self.get_sharer_participant_id();
        let trigger_type = trigger.as_ref().map(PassiveSuggestionTriggerType::from);
        log::debug!(
            "[passive-suggestions] sending result: trigger={}, trigger_type={:?}",
            if trigger.is_some() { "Some" } else { "None" },
            trigger_type,
        );
        self.send_query(
            InputQuery {
                which_task,
                input_query: InputQueryType::AIInputType {
                    ai_input: AIAgentInput::PassiveSuggestionResult {
                        trigger,
                        suggestion,
                        context,
                    },
                },
                additional_attachments: HashMap::new(),
                queued_query_id: None,
            },
            EntrypointType::TriggerPassiveSuggestion {
                trigger: trigger_type,
            },
            participant_id,
            /*is_queued_prompt*/ false,
            ctx,
        );
    }

    /// Queues a passive suggestion result to be included with the next request
    /// for the given conversation. Use this instead of `send_passive_suggestion_result`
    /// when the result should not trigger an immediate server request (e.g. the user
    /// accepted a code diff without auto-resuming).
    pub fn queue_passive_suggestion_result(
        &mut self,
        conversation_id: AIConversationId,
        suggestion: PassiveSuggestionResultType,
        trigger: Option<PassiveSuggestionTrigger>,
    ) {
        self.pending_passive_suggestion_results
            .entry(conversation_id)
            .or_default()
            .push((suggestion, trigger));
    }

    fn send_follow_up_for_conversation(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        if self
            .in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, ctx)
        {
            return;
        }

        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
            history.mark_active_conversation_id(conversation_id, self.terminal_surface_id, ctx);
        });

        let active_child_conversation_ids = active_descendant_conversation_ids(
            BlocklistAIHistoryModel::as_ref(ctx),
            conversation_id,
        );
        if !active_child_conversation_ids.is_empty() {
            self.pending_child_blocked_follow_ups
                .insert(conversation_id);
            log::info!(
                "Deferring agent follow-up for conversation {conversation_id:?}: active child conversations remain: {:?}",
                active_child_conversation_ids
            );
            #[cfg(not(target_family = "wasm"))]
            remote_logging::log_model_event(
                ctx,
                RemoteLogRecord {
                    level: RemoteLogLevel::Warn,
                    message: "Agent follow-up deferred for active child agents".to_string(),
                    context: serde_json::json!({
                        "event": "agent_follow_up_deferred_active_child_agents",
                        "conversation_id": conversation_id.to_string(),
                        "active_descendant_conversation_ids": active_child_conversation_ids
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>(),
                    }),
                },
            );
            return;
        }
        self.pending_child_blocked_follow_ups
            .remove(&conversation_id);

        let mut finished_results = self.action_model.update(ctx, |action_model, _| {
            action_model.drain_finished_action_results(conversation_id)
        });
        if finished_results.is_empty() {
            return;
        }

        // Direct providers do not rely on a hosted orchestrator to create a CLI
        // subtask after the initial long-running-command snapshot. Create that
        // task locally at the action-result boundary, then route the snapshot
        // and every resulting monitor response through it. This is non-preemptive:
        // the original model stream has already finished and the action result is
        // ready for its normal follow-up.
        let initial_cli_block_id =
            finished_results
                .iter()
                .find_map(|result| match &result.result {
                    AIAgentActionResultType::RequestCommandOutput(
                        RequestCommandOutputResult::LongRunningCommandSnapshot { block_id, .. },
                    ) => Some(block_id.clone()),
                    _ => None,
                });
        if let Some(block_id) = initial_cli_block_id {
            let cli_task_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    history_model.create_cli_subagent_task_for_conversation(
                        block_id.clone(),
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    )
                });
            match cli_task_id {
                Ok(cli_task_id) => {
                    for result in &mut finished_results {
                        if matches!(
                            &result.result,
                            AIAgentActionResultType::RequestCommandOutput(
                                RequestCommandOutputResult::LongRunningCommandSnapshot {
                                    block_id: result_block_id,
                                    ..
                                }
                            ) if result_block_id == &block_id
                        ) {
                            result.task_id = cli_task_id.clone();
                        }
                    }
                }
                Err(error) => {
                    log::error!(
                        "Could not create direct-provider CLI monitor task for block \
                         {block_id:?}: {error:?}"
                    );
                }
            }
        }

        // Loop detection: record failures and check for repeated patterns
        let loop_warning = self.check_and_record_loop_detection(conversation_id, &finished_results);

        // Check whether any result will trigger a server-side subagent (e.g. CLI
        // subagent for LRC), or if one is already active. If so, we must not
        // piggyback orchestration events because the subagent cannot interpret
        // them and inserting events breaks tool_use/tool_result ordering.
        let will_trigger_server_subagent = finished_results
            .iter()
            .any(|r| r.result.triggers_server_subagent());
        let has_active_subagent = BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&conversation_id)
            .is_some_and(|c| c.has_active_subagent());

        let context = input_context_for_request(
            false,
            self.context_model.as_ref(ctx),
            self.active_session.as_ref(ctx),
            vec![],
            ctx,
        );
        let mut request_input = RequestInput::for_actions_results(
            finished_results,
            context,
            &self.active_session,
            self.get_current_response_initiator(),
            conversation_id,
            self.terminal_surface_id,
            ctx,
        );

        // If a loop was detected, inject a corrective instruction alongside
        // the action results so the model avoids repeating the same failure.
        if let Some(warning_msg) = loop_warning {
            log::warn!(
                "[loop-detection] Injecting corrective instruction for conversation {:?}: {}",
                conversation_id,
                warning_msg
            );
            if let Some(conversation) =
                BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
            {
                let root_task_id = conversation.get_root_task_id().clone();
                request_input
                    .input_messages
                    .entry(root_task_id)
                    .or_default()
                    .push(AIAgentInput::UserQuery {
                        query: warning_msg,
                        context: Arc::from([]),
                        static_query_type: None,
                        referenced_attachments: HashMap::new(),
                        user_query_mode: UserQueryMode::Normal,
                        running_command: None,
                        intended_agent: None,
                    });
            }
        }

        // Include any pending orchestration events in this follow-up rather
        // than waiting for a separate idle injection turn. Skip when a server
        // subagent is or will be active — events will be delivered via the idle
        // path once the subagent session ends.
        let mut has_piggybacked_events = false;
        if will_trigger_server_subagent || has_active_subagent {
            log::debug!(
                "Skipping event piggyback for conversation {conversation_id:?}: \
                 {}",
                if will_trigger_server_subagent {
                    "results will trigger a server-side subagent"
                } else {
                    "a subagent is currently active"
                }
            );
        } else if let Some((event_inputs, task_id)) = OrchestrationEventService::handle(ctx)
            .update(ctx, |svc, ctx| {
                svc.drain_events_for_request(conversation_id, ctx)
            })
        {
            has_piggybacked_events = true;
            request_input
                .input_messages
                .entry(task_id)
                .or_default()
                .extend(event_inputs);
        }

        let result = self.send_request_input(
            request_input,
            None,
            /*can_attempt_resume_on_error*/ true,
            /*is_queued_prompt*/ false,
            ctx,
        );

        if has_piggybacked_events && result.is_err() {
            OrchestrationEventService::handle(ctx).update(ctx, |svc, ctx| {
                svc.requeue_awaiting_events(conversation_id, ctx);
            });
        }

        self.pending_passive_follow_ups.remove(&conversation_id);
    }

    fn maybe_resume_child_blocked_follow_up(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self
            .pending_child_blocked_follow_ups
            .contains(&conversation_id)
        {
            return;
        }
        if self
            .in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, ctx)
        {
            return;
        }
        if self
            .action_model
            .as_ref(ctx)
            .has_unfinished_actions_for_conversation(conversation_id)
        {
            return;
        }
        if !active_descendant_conversation_ids(
            BlocklistAIHistoryModel::as_ref(ctx),
            conversation_id,
        )
        .is_empty()
        {
            return;
        }
        self.send_follow_up_for_conversation(conversation_id, ctx);
    }

    fn handle_orchestrated_child_status_changed(
        &mut self,
        run_id: &str,
        status: ConversationStatus,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation_id) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation_id_for_agent_id(run_id)
        else {
            return;
        };
        let owns_conversation = BlocklistAIHistoryModel::as_ref(ctx)
            .all_live_conversations_for_terminal_surface(self.terminal_surface_id)
            .any(|conversation| conversation.id() == conversation_id);
        if !owns_conversation {
            return;
        }

        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
            history_model.update_conversation_status(
                self.terminal_surface_id,
                conversation_id,
                status,
                ctx,
            );
        });
    }

    fn check_and_record_loop_detection(
        &mut self,
        conversation_id: AIConversationId,
        results: &[AIAgentActionResult],
    ) -> Option<String> {
        use std::hash::{Hash, Hasher};

        let state = self.loop_detection.entry(conversation_id).or_default();
        let mut has_success = false;

        for result in results {
            if result.result.is_failed() {
                let discriminant = std::mem::discriminant(&result.result);
                // Use a stable description that includes the tool type and the *input*
                // (command, file paths, etc.) but NOT the variable output, so the same
                // failing command with different output is still recognized as a loop.
                let description = result.result.loop_description();

                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                discriminant.hash(&mut hasher);
                description.hash(&mut hasher);
                let input_hash = hasher.finish();

                state.record_failure(input_hash, description);
            } else if result.result.is_successful() {
                has_success = true;
            }
        }

        // If we had at least one success in this batch, clear loop state —
        // the agent is making progress.
        if has_success {
            state.record_success();
            return None;
        }

        // Check for loops
        if let Some(looping_entry) = state.detect_and_reset() {
            let warning = format!(
                "[SYSTEM] Loop detected: the same action has failed {} or more times consecutively. \
                 Do NOT repeat this action or any similar approach.\n\n\
                 Failing action: {}\n\n\
                 Take a completely different approach to accomplish the goal. \
                 If you cannot find an alternative, explain to the user what is failing and why.",
                looping_entry.threshold,
                looping_entry.description
            );
            Some(warning)
        } else {
            None
        }
    }

    fn conversation_ready_for_pending_events(
        &self,
        conversation_id: AIConversationId,
        ctx: &ModelContext<Self>,
    ) -> bool {
        let owns = BlocklistAIHistoryModel::as_ref(ctx)
            .all_live_conversations_for_terminal_surface(self.terminal_surface_id)
            .any(|conversation| conversation.id() == conversation_id);
        let has_active_stream = self
            .in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, ctx);
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            log::info!(
                "Pending events are not ready: conversation_id={conversation_id:?} reason=conversation_missing owns_conversation={owns} has_active_stream={has_active_stream}"
            );
            return false;
        };
        // WaitingForEvents is treated as Success here: pending events
        // drain via the next outbound request and the server-side
        // supersede emits the resume signal.
        let is_ready_status = matches!(
            conversation.status(),
            ConversationStatus::Success | ConversationStatus::WaitingForEvents,
        );
        if !owns || has_active_stream || !is_ready_status {
            log::info!(
                "Pending events are not ready: conversation_id={conversation_id:?} owns_conversation={owns} has_active_stream={has_active_stream} status={:?}",
                conversation.status()
            );
            return false;
        }

        true
    }

    #[cfg(target_family = "wasm")]
    fn maybe_prepare_local_claude_wake(
        &mut self,
        _conversation_id: AIConversationId,
        _trigger: LocalClaudeWakeTrigger,
        _ctx: &mut ModelContext<Self>,
    ) -> bool {
        false
    }

    #[cfg(not(target_family = "wasm"))]
    fn maybe_prepare_local_claude_wake(
        &mut self,
        conversation_id: AIConversationId,
        trigger: LocalClaudeWakeTrigger,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        if self
            .pending_local_claude_wakes
            .contains_key(&conversation_id)
        {
            log::info!("Dormant Claude wake already pending: conversation_id={conversation_id:?}");
            return true;
        }
        if trigger.requires_pending_events() {
            let has_pending_events = OrchestrationEventService::handle(ctx)
                .update(ctx, |svc, _| svc.has_pending_events(conversation_id));
            if !has_pending_events {
                return false;
            }
        }

        if !self.conversation_ready_for_pending_events(conversation_id, ctx) {
            return false;
        }

        let history_model = BlocklistAIHistoryModel::as_ref(ctx);
        let Some(conversation) = history_model.conversation(&conversation_id).cloned() else {
            log::info!(
                "Skipping dormant Claude wake preparation: conversation_id={conversation_id:?} reason=conversation_missing"
            );
            return false;
        };
        let parent_conversation = conversation
            .parent_conversation_id()
            .and_then(|parent_conversation_id| history_model.conversation(&parent_conversation_id))
            .cloned();
        let working_dir = self
            .active_session
            .as_ref(ctx)
            .current_working_directory()
            .cloned()
            .map(PathBuf::from);
        let task_id = conversation.task_id();
        let wake_message_for_prepare = match &trigger {
            LocalClaudeWakeTrigger::PendingEvents => None,
            LocalClaudeWakeTrigger::WakeOnlyStream { wake_message } => Some(wake_message.clone()),
        };
        let trigger_for_callback = trigger.clone();

        let server_api = ServerApiProvider::as_ref(ctx).get();
        let handle = ctx.spawn(
            async move {
                log::info!(
                    "Preparing dormant Claude wake command: conversation_id={conversation_id:?} task_id={task_id:?}"
                );
                ClaudeHarness::wake_dormant_session(
                    server_api.clone(),
                    conversation,
                    parent_conversation,
                    working_dir,
                    wake_message_for_prepare,
                )
                .await
            },
            move |me, result, ctx| {
                me.pending_local_claude_wakes.remove(&conversation_id);
                match result {
                    Ok(Some(command)) => {
                        if let LocalClaudeWakeTrigger::WakeOnlyStream { wake_message } =
                            &trigger_for_callback
                        {
                            OrchestrationEventStreamer::handle(ctx).update(
                                ctx,
                                |streamer, ctx| {
                                    streamer.persist_dormant_claude_wake_cursor(
                                        conversation_id,
                                        wake_message,
                                        ctx,
                                    );
                                },
                            );
                        }
                        log::info!(
                            "Executing dormant Claude wake command: conversation_id={conversation_id:?} task_id={task_id:?}"
                        );
                        BlocklistAIHistoryModel::handle(ctx).update(
                            ctx,
                            |history_model, ctx| {
                                history_model.update_conversation_status(
                                    me.terminal_surface_id,
                                    conversation_id,
                                    ConversationStatus::InProgress,
                                    ctx,
                                );
                            },
                        );
                        ctx.emit(BlocklistAIControllerEvent::ExecuteLocalHarnessCommand {
                            command,
                        });
                    }
                    Ok(None) => {
                        match &trigger_for_callback {
                            LocalClaudeWakeTrigger::PendingEvents => {
                                log::info!(
                                    "Falling back to generic pending-event injection after dormant Claude wake eligibility check: conversation_id={conversation_id:?} task_id={task_id:?}"
                                );
                                me.inject_pending_events_for_request(conversation_id, ctx);
                            }
                            LocalClaudeWakeTrigger::WakeOnlyStream { wake_message } => {
                                log::info!(
                                    "Retrying wake-only dormant Claude eligibility check: conversation_id={conversation_id:?} task_id={task_id:?}"
                                );
                                me.schedule_dormant_claude_wake_ready_retry(
                                    conversation_id,
                                    wake_message.clone(),
                                    ctx,
                                );
                            }
                        }
                    }
                    Err(err) => {
                        log::warn!(
                            "Failed to prepare dormant Claude wake command for {conversation_id:?} task_id={task_id:?}: {err:#}"
                        );
                        match &trigger_for_callback {
                            LocalClaudeWakeTrigger::PendingEvents => {
                                me.schedule_pending_events_ready_retry(conversation_id, ctx);
                            }
                            LocalClaudeWakeTrigger::WakeOnlyStream { wake_message } => {
                                me.schedule_dormant_claude_wake_ready_retry(
                                    conversation_id,
                                    wake_message.clone(),
                                    ctx,
                                );
                            }
                        }
                    }
                }
            },
        );
        self.pending_local_claude_wakes
            .insert(conversation_id, handle);
        true
    }

    #[cfg(not(target_family = "wasm"))]
    fn schedule_pending_events_ready_retry(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        ctx.spawn(
            async move { Timer::after(Duration::from_secs(2)).await },
            move |me, _, ctx| {
                me.handle_pending_events_ready(conversation_id, ctx);
            },
        );
    }

    #[cfg(not(target_family = "wasm"))]
    fn schedule_dormant_claude_wake_ready_retry(
        &mut self,
        conversation_id: AIConversationId,
        wake_message: AgentMessageEventMetadata,
        ctx: &mut ModelContext<Self>,
    ) {
        ctx.spawn(
            async move { Timer::after(Duration::from_secs(2)).await },
            move |me, _, ctx| {
                me.handle_dormant_claude_wake_ready(conversation_id, wake_message.clone(), ctx);
            },
        );
    }

    fn inject_pending_events_for_request(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.conversation_ready_for_pending_events(conversation_id, ctx) {
            return;
        }

        let Some((inputs, task_id)) = OrchestrationEventService::handle(ctx)
            .update(ctx, |svc, ctx| {
                svc.drain_events_for_request(conversation_id, ctx)
            })
        else {
            return;
        };

        // The resume request supersedes any in-flight wait_for_events.
        self.action_model.update(ctx, |action_model, ctx| {
            action_model.cancel_wait_for_events_for_conversation(conversation_id, ctx);
        });

        if self
            .send_request_input(
                RequestInput::for_task(
                    inputs,
                    task_id,
                    &self.active_session,
                    self.get_current_response_initiator(),
                    conversation_id,
                    self.terminal_surface_id,
                    ctx,
                ),
                None,
                /*can_attempt_resume_on_error*/ true,
                /*is_queued_prompt*/ false,
                ctx,
            )
            .is_err()
        {
            // TODO: surface retry exhaustion. The existing requeue
            // re-emits `EventsReady` until `MAX_RETRY_ATTEMPTS` is hit,
            // after which events are dropped silently and the wait has
            // already been cancelled — the conversation can end up stuck
            // with no executor pending entry, no watchdog, and no
            // in-flight stream. Follow-up: park-on-exhaust the events
            // and transition the conversation to `Error` so the next
            // user resume can carry them along.
            OrchestrationEventService::handle(ctx).update(ctx, |svc, ctx| {
                svc.requeue_awaiting_events(conversation_id, ctx);
            });
        }
    }

    /// Handles the EventsReady signal. Checks readiness, drains
    /// pending events from the service, and injects them into the conversation.
    fn handle_pending_events_ready(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.conversation_ready_for_pending_events(conversation_id, ctx) {
            return;
        }

        if self.maybe_prepare_local_claude_wake(
            conversation_id,
            LocalClaudeWakeTrigger::PendingEvents,
            ctx,
        ) {
            return;
        }

        self.inject_pending_events_for_request(conversation_id, ctx);
    }

    fn handle_dormant_claude_wake_ready(
        &mut self,
        conversation_id: AIConversationId,
        wake_message: AgentMessageEventMetadata,
        ctx: &mut ModelContext<Self>,
    ) {
        if !self.maybe_prepare_local_claude_wake(
            conversation_id,
            LocalClaudeWakeTrigger::WakeOnlyStream { wake_message },
            ctx,
        ) {
            log::info!(
                "Ignoring dormant Claude wake-ready signal: conversation_id={conversation_id:?}"
            );
        }
    }

    pub fn resume_conversation(
        &mut self,
        conversation_id: AIConversationId,
        can_attempt_resume_on_error: bool,
        is_auto_resume_after_error: bool,
        additional_context: Vec<AIAgentContext>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            log::error!("Tried to resume non-existent conversation: {conversation_id:?}");
            return;
        };
        let task_id = {
            let terminal_model = self.terminal_model.lock();
            let active_block = terminal_model.block_list().active_block();
            if let Some(agent_interaction_metadata) = active_block
                .agent_interaction_metadata()
                .filter(|metadata| {
                    metadata.conversation_id() == &conversation_id && metadata.is_agent_in_control()
                })
            {
                agent_interaction_metadata
                    .subagent_task_id()
                    .cloned()
                    .unwrap_or_else(|| conversation.get_root_task_id().clone())
            } else {
                conversation.get_root_task_id().clone()
            }
        };

        let context = input_context_for_request(
            false,
            self.context_model.as_ref(ctx),
            self.active_session.as_ref(ctx),
            additional_context,
            ctx,
        );

        let inputs = vec![AIAgentInput::ResumeConversation { context }];
        let metadata = if is_auto_resume_after_error {
            Some(RequestMetadata {
                is_autodetected_user_query: false,
                entrypoint: EntrypointType::ResumeConversation,
                is_auto_resume_after_error: true,
            })
        } else {
            None
        };
        let _ = self.send_request_input(
            RequestInput::for_task(
                inputs,
                task_id,
                &self.active_session,
                self.get_current_response_initiator(),
                conversation_id,
                self.terminal_surface_id,
                ctx,
            ),
            metadata,
            can_attempt_resume_on_error,
            /*is_queued_prompt*/ false,
            ctx,
        );
    }

    /// Schedules an auto-resume-after-error for the conversation once the network is online
    /// and the auto-handoff sleep modal is closed, so the resume doesn't race the user's
    /// enable/dismiss decision on wake.
    /// Handles the completion of a crosscheck review cycle.
    ///
    /// If the reviewer provided feedback, it is injected as a synthetic user
    /// query to the main agent. If approved or max iterations reached, the
    /// conversation is allowed to complete normally.
    fn handle_crosscheck_review_completed(
        &mut self,
        conversation_id: AIConversationId,
        outcome: crate::ai::crosscheck::ReviewOutcome,
        ctx: &mut ModelContext<Self>,
    ) {
        use crate::ai::crosscheck::ReviewOutcome;

        match outcome {
            ReviewOutcome::Approved => {
                log::info!("[crosscheck] Work approved for conversation {conversation_id:?}");
                // Nothing to do — the conversation completes normally.
            }
            ReviewOutcome::MaxIterationsReached { last_feedback } => {
                log::warn!(
                    "[crosscheck] Max iterations reached for {conversation_id:?}; showing last feedback to user"
                );
                // Inject the last feedback as a visible message so the user is aware.
                self.inject_crosscheck_feedback(conversation_id, last_feedback, true, ctx);
            }
            ReviewOutcome::Feedback { message } => {
                log::info!(
                    "[crosscheck] Injecting reviewer feedback into conversation {conversation_id:?}"
                );
                self.inject_crosscheck_feedback(conversation_id, message, false, ctx);
            }
            ReviewOutcome::Error { error } => {
                log::error!("[crosscheck] Reviewer failed for {conversation_id:?}: {error}");
                // Don't block the conversation on reviewer errors; just log it.
            }
        }
    }

    /// Injects crosscheck reviewer feedback as a synthetic user query to the
    /// main agent, prompting it to address the feedback.
    fn inject_crosscheck_feedback(
        &mut self,
        conversation_id: AIConversationId,
        feedback: String,
        is_final: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            return;
        };
        let root_task_id = conversation.get_root_task_id().clone();

        let prefix = if is_final {
            "[CROSSCHECK REVIEWER - FINAL NOTE] The reviewer reached the maximum number of \
             review cycles. Below is the last feedback. Please address what you can, but you \
             may proceed even if not all items are resolved:"
        } else {
            "[CROSSCHECK REVIEWER] The following feedback was provided by an automated \
             reviewer. Please address ALL items below and then present your updated work:"
        };

        let corrective_msg = format!("{prefix}\n\n{feedback}");

        let inputs = vec![AIAgentInput::UserQuery {
            query: corrective_msg,
            context: Arc::from([]),
            static_query_type: None,
            referenced_attachments: HashMap::new(),
            user_query_mode: UserQueryMode::Normal,
            running_command: None,
            intended_agent: None,
        }];

        let _ = self.send_request_input(
            RequestInput::for_task(
                inputs,
                root_task_id,
                &self.active_session,
                self.get_current_response_initiator(),
                conversation_id,
                self.terminal_surface_id,
                ctx,
            ),
            None,
            /*can_attempt_resume_on_error*/ false,
            /*is_queued_prompt*/ false,
            ctx,
        );
    }

    fn send_tool_error_no_action_recovery(
        &mut self,
        conversation_id: AIConversationId,
        reason: &'static str,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            return;
        };
        let root_task_id = conversation.get_root_task_id().clone();
        let corrective_msg = format!(
            "[SYSTEM] The previous tool result failed, and your last response stopped with \
             an unfulfilled inspection/search intent ({reason}) without calling another tool \
             or answering. Continue now. Either retry with a narrower available tool call, \
             or answer from the evidence already available and explicitly state what could \
             not be verified. Do not end this turn with another promise to inspect."
        );

        let inputs = vec![AIAgentInput::UserQuery {
            query: corrective_msg,
            context: Arc::from([]),
            static_query_type: None,
            referenced_attachments: HashMap::new(),
            user_query_mode: UserQueryMode::Normal,
            running_command: None,
            intended_agent: None,
        }];

        if let Err(error) = self.send_request_input(
            RequestInput::for_task(
                inputs,
                root_task_id,
                &self.active_session,
                self.get_current_response_initiator(),
                conversation_id,
                self.terminal_surface_id,
                ctx,
            ),
            Some(RequestMetadata {
                is_autodetected_user_query: false,
                entrypoint: EntrypointType::AgentInitiated,
                is_auto_resume_after_error: false,
            }),
            /*can_attempt_resume_on_error*/ false,
            /*is_queued_prompt*/ false,
            ctx,
        ) {
            log::warn!(
                "Failed to send tool-error no-action recovery for conversation \
                 {conversation_id:?}: {error:?}"
            );
        }
    }

    /// Checks whether a crosscheck review should be triggered for a conversation
    /// that just finished with no actions to queue (i.e., the agent is "done").
    ///
    /// If eligible, extracts the agent's last output text and kicks off the
    /// reviewer sub-agent.
    fn maybe_trigger_crosscheck(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        use settings::Setting;

        let ai_settings = crate::AISettings::as_ref(ctx);
        if !ai_settings.is_crosscheck_enabled(ctx) {
            return;
        }

        // Don't trigger crosscheck for child conversations
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            return;
        };
        if conversation.parent_conversation_id().is_some() {
            return;
        }

        // Extract the agent's last output text
        let agent_output = self.extract_last_agent_output(conversation_id, ctx);
        if agent_output.is_empty() {
            return;
        }

        // Determine which model to use for the reviewer
        let model_id = {
            let configured = ai_settings.crosscheck_model_id().to_string();
            if configured.is_empty() {
                // Fall back to the conversation's active model
                LLMPreferences::as_ref(ctx)
                    .get_active_base_model(ctx, Some(self.terminal_surface_id))
                    .id
                    .to_string()
            } else {
                configured
            }
        };

        let max_iterations = ai_settings.crosscheck_max_iterations();

        self.crosscheck_reviewer.update(ctx, |reviewer, ctx| {
            reviewer.start_review(conversation_id, max_iterations, agent_output, model_id, ctx);
        });
    }

    /// Extracts the text content of the main agent's most recent output for a
    /// conversation (used as input to the crosscheck reviewer).
    fn extract_last_agent_output(
        &self,
        conversation_id: AIConversationId,
        ctx: &AppContext,
    ) -> String {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            return String::new();
        };

        // Get the last exchange's output messages
        let exchanges = conversation.all_exchanges();
        let Some(last_exchange) = exchanges.last() else {
            return String::new();
        };

        let AIAgentOutputStatus::Finished {
            finished_output: FinishedAIAgentOutput::Success { output },
            ..
        } = &last_exchange.output_status
        else {
            return String::new();
        };

        // Collect text messages from the output
        output
            .get()
            .messages
            .iter()
            .filter_map(|msg| {
                use crate::ai::agent::{AIAgentOutputMessageType, AIAgentTextSection};
                match &msg.message {
                    AIAgentOutputMessageType::Text(text) => {
                        let plain_text: String = text
                            .sections
                            .iter()
                            .filter_map(|section| match section {
                                AIAgentTextSection::PlainText { text } => {
                                    Some(text.text().to_string())
                                }
                                AIAgentTextSection::Code { code, .. } => Some(code.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        if plain_text.is_empty() {
                            None
                        } else {
                            Some(plain_text)
                        }
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn schedule_auto_resume_after_error(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let wait_for_online = NetworkStatus::as_ref(ctx).wait_until_online();
        let wait_for_modal_closed =
            OneTimeModalModel::as_ref(ctx).wait_until_auto_handoff_sleep_modal_closed();
        let wait = async move {
            wait_for_online.await;
            // Await the modal second: the future reads live modal state at
            // poll time, so a modal surfaced on wake (after connectivity
            // returns) is still observed.
            wait_for_modal_closed.await;
        };
        let handle = ctx.spawn(wait, move |me, _, ctx| {
            // Clean up the pending handle now that the resume is executing.
            me.pending_auto_resume_handles.remove(&conversation_id);
            me.resume_conversation(
                conversation_id,
                // Don't allow a second resume-on-error to prevent a persistent loop.
                /*can_attempt_resume_on_error*/
                false,
                /*is_auto_resume_after_error*/ true,
                vec![],
                ctx,
            );
        });
        self.pending_auto_resume_handles
            .insert(conversation_id, handle);
    }

    pub fn send_passive_code_diff_request(
        &mut self,
        query: String,
        block_id: &BlockId,
        file_contexts: Vec<FileContext>,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<(AIConversationId, ResponseStreamId)> {
        let mut input_context = file_contexts
            .into_iter()
            .map(AIAgentContext::File)
            .collect_vec();
        if let Some(block_context) = self
            .context_model
            .as_ref(ctx)
            .transform_block_to_context(block_id, false)
        {
            input_context.push(block_context);
        }

        let new_conversation = self.start_new_conversation_for_request(ctx);
        self.send_request_input(
            RequestInput::for_task(
                vec![AIAgentInput::AutoCodeDiffQuery {
                    query,
                    context: input_context.into(),
                }],
                new_conversation.get_root_task_id().clone(),
                &self.active_session,
                self.get_current_response_initiator(),
                new_conversation.id(),
                self.terminal_surface_id,
                ctx,
            ),
            Some(RequestMetadata {
                is_autodetected_user_query: false,
                entrypoint: EntrypointType::PromptSuggestion {
                    is_static: false,
                    is_coding: true,
                },
                is_auto_resume_after_error: false,
            }),
            /*can_attempt_resume_on_error*/ true,
            /*is_queued_prompt*/ false,
            ctx,
        )
    }

    /// Builds request params for an out-of-band passive suggestions request.
    ///
    /// This reads conversation state read-only and does NOT create exchanges,
    /// register response streams, or modify conversation status. The caller
    /// is responsible for spawning the API call and handling the response.
    ///
    /// If `followup_conversation_id` is provided, the conversation's task context
    /// and server token are included so the server can use prior context.
    /// Otherwise, a new conversation is created to anchor the request.
    /// Builds request params for an out-of-band passive suggestions request.
    ///
    /// This is read-only and does NOT create exchanges, register response
    /// streams, or modify conversation history. The caller is responsible for
    /// spawning the API call and handling the response.
    ///
    /// If `followup_conversation_id` is provided, the conversation's task
    /// context and server token are included so the server can use prior
    /// context. Otherwise a fresh, ephemeral conversation ID is generated
    /// without touching the history model.
    pub fn build_passive_suggestions_request_params(
        &self,
        followup_conversation_id: Option<AIConversationId>,
        trigger: PassiveSuggestionTrigger,
        supported_tools: Vec<ToolType>,
        ctx: &ModelContext<Self>,
    ) -> anyhow::Result<(AIConversationId, api::RequestParams)> {
        let history_model = BlocklistAIHistoryModel::as_ref(ctx);

        // Resolve conversation state. For follow-ups we read from history;
        // for new triggers we generate a fresh ID without persisting anything.
        let (conversation_id, task_id, conversation_data) = if let Some(conversation_id) =
            followup_conversation_id
        {
            let Some(conversation) = history_model.conversation(&conversation_id) else {
                return Err(anyhow!(
                        "Tried to build passive suggestions request params for non-existent conversation with ID {conversation_id:?}"
                    ));
            };
            let task_id = conversation.get_root_task_id().clone();
            let conversation_data = api::ConversationData {
                id: conversation_id,
                tasks: conversation.compute_active_tasks(),
                server_conversation_token: conversation.server_conversation_token().cloned(),
                forked_from_conversation_token: conversation
                    .forked_from_server_conversation_token()
                    .cloned(),
                // Do not tie passive suggestion requests to the cloud agent task, since they are
                // separate, read-only requests.
                ambient_agent_task_id: None,
                existing_suggestions: None,
            };
            (conversation_id, task_id, conversation_data)
        } else if !matches!(
            trigger,
            PassiveSuggestionTrigger::AgentResponseCompleted { .. }
        ) {
            // Generate a fresh, ephemeral conversation ID without mutating history.
            let conversation_id = AIConversationId::new();
            let task_id = TaskId::new(uuid::Uuid::new_v4().to_string());
            let conversation_data = api::ConversationData {
                id: conversation_id,
                tasks: vec![],
                server_conversation_token: None,
                forked_from_conversation_token: None,
                // Do not tie passive suggestion requests to the cloud agent task, since they are
                // separate, read-only requests.
                ambient_agent_task_id: None,
                existing_suggestions: None,
            };
            (conversation_id, task_id, conversation_data)
        } else {
            return Err(anyhow!(
                    "Tried to use agent response completed trigger to generate passive suggestions without a conversation ID"
                ));
        };

        let inputs = vec![AIAgentInput::TriggerPassiveSuggestion {
            context: input_context_for_request(
                false,
                self.context_model.as_ref(ctx),
                self.active_session.as_ref(ctx),
                vec![],
                ctx,
            ),
            attachments: vec![],
            trigger: trigger.clone(),
        }];

        let request_input = RequestInput::for_task(
            inputs,
            task_id,
            &self.active_session,
            self.get_current_response_initiator(),
            conversation_id,
            self.terminal_surface_id,
            ctx,
        )
        .with_supported_tools(supported_tools);

        let metadata = Some(RequestMetadata {
            is_autodetected_user_query: false,
            entrypoint: EntrypointType::TriggerPassiveSuggestion {
                trigger: Some((&trigger).into()),
            },
            is_auto_resume_after_error: false,
        });

        let request_params = api::RequestParams::new(
            Some(self.terminal_surface_id),
            SessionContext::from_session(self.active_session.as_ref(ctx), ctx),
            &request_input,
            conversation_data,
            metadata,
            ctx,
        );

        Ok((conversation_id, request_params))
    }

    pub fn send_unit_test_suggestions_request(
        &mut self,
        block_output: String,
        trigger: PassiveSuggestionTrigger,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<(AIConversationId, ResponseStreamId)> {
        let attachments = vec![AIAgentAttachment::PlainText(block_output.to_string())];
        let trigger_type = (&trigger).into();
        let inputs = vec![AIAgentInput::TriggerPassiveSuggestion {
            context: input_context_for_request(
                false,
                self.context_model.as_ref(ctx),
                self.active_session.as_ref(ctx),
                vec![],
                ctx,
            ),
            attachments,
            trigger,
        }];

        let new_conversation = self.start_new_conversation_for_request(ctx);
        self.send_request_input(
            RequestInput::for_task(
                inputs,
                new_conversation.get_root_task_id().clone(),
                &self.active_session,
                self.get_current_response_initiator(),
                new_conversation.id(),
                self.terminal_surface_id,
                ctx,
            ),
            Some(RequestMetadata {
                is_autodetected_user_query: false,
                entrypoint: EntrypointType::TriggerPassiveSuggestion {
                    trigger: Some(trigger_type),
                },
                is_auto_resume_after_error: false,
            }),
            /*can_attempt_resume_on_error*/ true,
            /*is_queued_prompt*/ false,
            ctx,
        )
    }

    /// Set the ID of the ambient agent task which owns this controller and its backing session.
    pub fn set_ambient_agent_task_id(
        &mut self,
        id: Option<AmbientAgentTaskId>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.ambient_agent_task_id = id;
        self.action_model.update(ctx, |action_model, ctx| {
            action_model.set_ambient_agent_task_id(id, ctx);
        });
    }

    #[cfg(test)]
    pub fn get_ambient_agent_task_id(&self) -> Option<AmbientAgentTaskId> {
        self.ambient_agent_task_id
    }

    /// Set the per-session directory for downloading file attachments.
    pub fn set_attachments_download_dir(&mut self, dir: std::path::PathBuf) {
        self.attachments_download_dir = Some(dir);
    }

    fn start_new_conversation_for_request<'a>(
        &self,
        ctx: &'a mut ModelContext<Self>,
    ) -> &'a AIConversation {
        let is_autoexecute_override = self
            .context_model
            .as_ref(ctx)
            .pending_query_autoexecute_override(ctx)
            .is_autoexecute_any_action();
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        let id = history_model.update(ctx, |history_model, ctx| {
            // We don't mark passive conversations as "the active conversation" (at least when they first appear).
            history_model.start_new_conversation(
                self.terminal_surface_id,
                is_autoexecute_override,
                false,
                false,
                ctx,
            )
        });
        history_model
            .as_ref(ctx)
            .conversation(&id)
            .expect("Conversation exists- was just created.")
    }

    /// Attempts to send a request to the AI model API. Adds context to the input if it
    /// contains a user query. Returns `Err` if the AI input was not able to be sent due to an
    /// existing in-flight request. Emits an event containing a receiver for the AI's output.
    /// If conversation_id is Some, we follow up in that conversation.
    /// If it's None or we can't find a conversation with that ID, we start a new one.
    /// Returns the conversation ID of affected conversation and response stream ID.
    ///
    ///  This function does not handle cancelling any in flight requests (and sending them back as
    /// input) for an existing conversation. Consider calling [`Self::send_custom_ai_input_query`] if
    /// you're trying to send a query with a custom [`AIAgentInput`] type where you'd like the "normal"
    /// flow that handles existing conversations properly.
    fn send_request_input(
        &mut self,
        mut request_input: RequestInput,
        query_metadata: Option<RequestMetadata>,
        can_attempt_resume_on_error: bool,
        is_queued_prompt: bool,
        ctx: &mut ModelContext<Self>,
    ) -> anyhow::Result<(AIConversationId, ResponseStreamId)> {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        let (
            conversation_id,
            conversation_server_token,
            conversation_forked_from_token,
            active_tasks,
            parent_agent_id,
            agent_name,
            bedrock_history,
            bedrock_tool_result_archive,
            bedrock_progressive_summary,
            agent_backend,
        ) = {
            let Some(conversation) = history_model
                .as_ref(ctx)
                .conversation(&request_input.conversation_id)
            else {
                return Err(anyhow!(
                    "Tried to send request for non-existent conversation with ID {:?}",
                    request_input.conversation_id
                ));
            };

            let active_tasks = conversation.compute_active_tasks();

            (
                conversation.id(),
                conversation.server_conversation_token().cloned(),
                conversation
                    .forked_from_server_conversation_token()
                    .cloned(),
                active_tasks,
                conversation.parent_agent_id().map(str::to_string),
                conversation.agent_name().map(str::to_string),
                conversation.bedrock_message_history().to_vec(),
                conversation.tool_result_archive().to_vec(),
                conversation.progressive_summary().map(str::to_string),
                conversation.agent_backend().clone(),
            )
        };

        if let Some(acp_model_id) = acp_backend_model_id(&agent_backend) {
            // ACP agents own model selection. Keep every native exchange,
            // identifier, and SentRequest event from attributing this turn to
            // whichever LiteLLM/Bedrock model happens to be selected in Galaxy.
            request_input.model_id = acp_model_id.clone();
            request_input.coding_model_id = acp_model_id.clone();
            request_input.cli_agent_model_id = acp_model_id.clone();
            request_input.computer_use_model_id = acp_model_id;
        }

        // Cancel any pending auto-resume for this conversation, since the user is sending a new
        // request.
        if let Some(handle) = self
            .pending_auto_resume_handles
            .remove(&request_input.conversation_id)
        {
            handle.abort();
        }

        // Passive background requests never auto-resume: a resume would issue a fresh
        // turn on a conversation the user never sees.
        let is_passive_request = request_input
            .all_inputs()
            .any(|input| input.is_passive_request());
        let can_attempt_resume_on_error = can_attempt_resume_on_error && !is_passive_request;

        // Make sure there's no existing response stream for the conversation. If
        // there is, something has gone wrong.
        if self
            .in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, ctx)
        {
            send_telemetry_from_ctx!(
                TelemetryEvent::AIInputNotSent {
                    entrypoint: query_metadata.map(|metadata| metadata.entrypoint),
                    inputs: request_input
                        .all_inputs()
                        .cloned()
                        .map(|input| input.into())
                        .collect(),
                    active_server_conversation_id: conversation_server_token.clone(),
                    active_client_conversation_id: Some(conversation_id),
                },
                ctx
            );
            const AI_INPUT_NOT_SENT_ERROR_STR: &str =
                "Not sending AI input because there is an in-flight request";
            safe_assert!(false, "{}", AI_INPUT_NOT_SENT_ERROR_STR);
            return Err(anyhow::anyhow!(AI_INPUT_NOT_SENT_ERROR_STR));
        }

        let conversation_data = api::ConversationData {
            id: conversation_id,
            tasks: active_tasks,
            server_conversation_token: conversation_server_token,
            forked_from_conversation_token: conversation_forked_from_token,
            ambient_agent_task_id: self.ambient_agent_task_id,
            existing_suggestions: history_model
                .as_ref(ctx)
                .existing_suggestions_for_conversation(conversation_id)
                .cloned(),
        };

        // Log an error if tool call results do not have corresponding tool calls in task context
        validate_tool_call_results(
            request_input.all_inputs(),
            &conversation_data.tasks,
            &conversation_data.server_conversation_token,
        );

        // Safety net: if the Gemini Enterprise (GEAP) OIDC/WIF credential is
        // nearing or past expiry, kick off a background refresh so upcoming
        // requests can authenticate even when the proactive refresh loop isn't
        // running (e.g. a parked or never-armed refresh chain).
        #[cfg(not(target_family = "wasm"))]
        {
            use ::ai::api_keys::ApiKeyManager;

            ApiKeyManager::handle(ctx).update(ctx, |manager, ctx| {
                crate::ai::geap_credentials::refresh_geap_credentials_if_needed(manager, ctx);
            });
        }

        let mut request_params = api::RequestParams::new(
            Some(self.terminal_surface_id),
            SessionContext::from_session(self.active_session.as_ref(ctx), ctx),
            &request_input,
            conversation_data.clone(),
            query_metadata,
            ctx,
        );
        let action_result_ids = request_input
            .all_inputs()
            .filter_map(AIAgentInput::action_result)
            .map(|result| result.id.to_string())
            .collect::<HashSet<_>>();
        request_params.tool_results = self.action_model.update(ctx, |action_model, _| {
            action_model
                .drain_finished_tool_results(conversation_id)
                .into_iter()
                .filter(|result| action_result_ids.contains(&result.call_id))
                .collect()
        });
        request_params.parent_agent_id = parent_agent_id;
        request_params.agent_name = agent_name;
        if history_model
            .as_ref(ctx)
            .conversation(&conversation_id)
            .is_some_and(|conversation| conversation.is_child_agent_conversation())
        {
            request_params.orchestration_enabled = false;
        }
        request_params.message_history = bedrock_history;
        request_params.tool_result_archive = bedrock_tool_result_archive;
        request_params.progressive_summary = bedrock_progressive_summary;

        // For the Bedrock path, when this is the first request in a new conversation
        // (no tasks established yet), use the conversation's root task ID so the
        // CreateTask response action can correctly upgrade the optimistic root task.
        //
        // However, for CLI subagent requests (long-running command interactions), the
        // input_messages key is the subagent task ID, and we must keep that so response
        // messages (AddMessagesToTask) are routed to the correct task/exchange. Overriding
        // with the root task ID would cause the output to be added to a hidden root-task
        // exchange instead of the visible subagent exchange.
        {
            let history_model = BlocklistAIHistoryModel::as_ref(ctx);
            if let Some(conversation) = history_model.conversation(&conversation_id) {
                let has_optimistic_cli_subagent = conversation.has_active_subagent();
                if !has_optimistic_cli_subagent {
                    request_params.root_task_id = Some(conversation.get_root_task_id().to_string());
                }
            }
        }
        let server_conversation_token_for_identifiers =
            conversation_data.server_conversation_token.clone();

        let response_stream = ctx.add_model(|ctx| {
            // Create AIIdentifiers for the response stream
            let ai_identifiers = AIIdentifiers {
                server_output_id: None, // Will be populated by the successful response
                server_conversation_id: server_conversation_token_for_identifiers.map(Into::into),
                client_conversation_id: Some(conversation_data.id),
                client_exchange_id: None,
                model_id: Some(request_params.model.clone()),
            };
            ResponseStream::new(
                request_params.clone(),
                ai_identifiers,
                agent_backend.clone(),
                can_attempt_resume_on_error,
                ctx,
            )
        });
        let response_stream_id = response_stream.as_ref(ctx).id().clone();
        let response_stream_clone = response_stream.clone();
        let input_contains_user_query = request_input
            .all_inputs()
            .any(|input| input.is_user_query());
        ctx.subscribe_to_model(&response_stream, move |me, _, event, ctx| {
            me.handle_response_stream_event(
                input_contains_user_query,
                event,
                &response_stream_clone,
                ctx,
            );
        });

        for input in request_input.all_inputs() {
            if let AIAgentInput::UserQuery {
                referenced_attachments,
                ..
            } = input
            {
                self.maybe_populate_plans_for_ai_document_model(
                    referenced_attachments,
                    conversation_data.id,
                    ctx,
                );
            }
        }

        history_model.update(ctx, |history_model, ctx| {
            match history_model.update_conversation_for_new_request_input(
                request_input,
                response_stream_id.clone(),
                self.terminal_surface_id,
                ctx,
            ) {
                Ok(_) => {
                    history_model.update_conversation_status(
                        self.terminal_surface_id,
                        conversation_data.id,
                        ConversationStatus::InProgress,
                        ctx,
                    );
                }
                Err(e) => {
                    log::warn!("Failed to push new exchange to AI conversation: {e:?}");
                }
            }
        });

        self.in_flight_response_streams.register_new_stream(
            response_stream_id.clone(),
            conversation_data.id,
            response_stream,
            CancellationReason::FollowUpSubmitted {
                is_for_same_conversation: true,
            },
            ctx,
        );

        // Skip the context reset for a fired queued-prompt row (`is_queued_prompt`): its
        // attachments came from the row, not the live staging, so the live `pending_attachments`
        // belong to the user's next prompt and must be preserved.
        if input_contains_user_query && !is_queued_prompt {
            // Get the pending document ID before clearing context
            let pending_document_id = self.context_model.as_ref(ctx).pending_document_id();

            // Reset the context state to the default.
            self.context_model.update(ctx, |context_model, ctx| {
                context_model.reset_context_to_default(ctx);
            });

            // Update the document status to UpToDate after query submission
            if let Some(doc_id) = pending_document_id {
                AIDocumentModel::handle(ctx).update(ctx, |model, mctx| {
                    model.set_user_edit_status(&doc_id, AIDocumentUserEditStatus::UpToDate, mctx);
                });
            }
        }

        ctx.emit(BlocklistAIControllerEvent::SentRequest {
            contains_user_query: input_contains_user_query,
            is_queued_prompt,
            model_id: request_params.model.clone(),
            stream_id: response_stream_id.clone(),
        });
        if !is_passive_request {
            history_model.update(ctx, |history_model, ctx| {
                history_model.mark_active_conversation_id(
                    conversation_data.id,
                    self.terminal_surface_id,
                    ctx,
                );
            });
        }

        // Trigger a snapshot save to persist the agent view state when a user query is sent.
        // This ensures the agent view is restored if the app restarts.
        if input_contains_user_query {
            ctx.dispatch_global_action("workspace:save_app", ());
        }

        Ok((conversation_data.id, response_stream_id))
    }

    /// Cancels a pending AI request response stream, given the exchange ID, if it exists.
    /// Returns true if a pending stream was found and canceled, false otherwise.
    pub fn try_cancel_pending_response_stream(
        &mut self,
        stream_id: &ResponseStreamId,
        reason: CancellationReason,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        self.in_flight_response_streams
            .try_cancel_stream(stream_id, reason, ctx)
    }

    pub fn has_active_stream_for_conversation(
        &self,
        conversation_id: AIConversationId,
        app: &AppContext,
    ) -> bool {
        self.in_flight_response_streams
            .has_active_stream_for_conversation(conversation_id, app)
    }

    #[cfg(test)]
    pub fn register_mock_stream_for_test(
        &mut self,
        stream_id: ResponseStreamId,
        conversation_id: AIConversationId,
        stream: ModelHandle<ResponseStream>,
        ctx: &mut ModelContext<Self>,
    ) {
        let stream_clone = stream.clone();
        ctx.subscribe_to_model(&stream, move |me, _, event, ctx| {
            me.handle_response_stream_event(false, event, &stream_clone, ctx);
        });
        self.in_flight_response_streams.register_new_stream(
            stream_id,
            conversation_id,
            stream,
            CancellationReason::ManuallyCancelled,
            ctx,
        );
    }

    /// Cancels 'progress' for the active conversation if there is one:
    ///  * If there is an in-flight request, cancels it.
    ///  * Else, if the request finished, but actions from the response are pending or mid-execution, cancels all of them.
    pub fn cancel_conversation_progress(
        &mut self,
        conversation_id: AIConversationId,
        reason: CancellationReason,
        ctx: &mut ModelContext<Self>,
    ) {
        // Cancel any pending auto-resume for this conversation.
        if let Some(handle) = self.pending_auto_resume_handles.remove(&conversation_id) {
            handle.abort();
        }

        // Discard any queued passive suggestion results for this conversation.
        self.pending_passive_suggestion_results
            .remove(&conversation_id);

        if !self
            .in_flight_response_streams
            .try_cancel_streams_for_conversation(conversation_id, reason, ctx)
        {
            // No active stream whose cancellation would mark the conversation `Cancelled`.
            // A parked auto-resume was aborted above; nothing else will move the
            // conversation out of TransientError, so surface the cancellation directly.
            //
            // TODO(REMOTE-1950): Track the parked auto-resume as a first-class cancelable so its
            // cancellation flows through the same `AfterStreamFinished` path, dropping this special case.
            if matches!(
                reason.conversation_outcome(),
                CancellationOutcome::Cancelled
            ) {
                let history_model = BlocklistAIHistoryModel::handle(ctx);
                let is_recovering = history_model
                    .as_ref(ctx)
                    .conversation(&conversation_id)
                    .is_some_and(|conversation| conversation.status().is_transient_error());
                if is_recovering {
                    history_model.update(ctx, |history_model, ctx| {
                        history_model.update_conversation_status(
                            self.terminal_surface_id,
                            conversation_id,
                            ConversationStatus::Cancelled,
                            ctx,
                        );
                    });
                }
            }

            // Otherwise, cancel pending actions and update the input state.
            self.action_model.update(ctx, |action_model, ctx| {
                action_model.cancel_all_pending_actions(conversation_id, Some(reason), ctx);
            });
            self.set_input_mode_for_cancellation(ctx);

            // Force-cancel all streaming exchanges and set the conversation status to
            // Cancelled. This handles the case where a query gets stuck (e.g. a subagent
            // hangs or the stream ended unexpectedly without proper cleanup) and repeated
            // Ctrl+C presses cannot resolve it. Without this, the conversation remains
            // InProgress indefinitely, hiding the input box and blocking user interaction.
            if !reason.is_follow_up_for_same_conversation() {
                let history_model = BlocklistAIHistoryModel::handle(ctx);
                if history_model
                    .as_ref(ctx)
                    .conversation(&conversation_id)
                    .is_some_and(|c| c.status().is_in_progress())
                {
                    let terminal_view_id = self.terminal_surface_id;
                    history_model.update(ctx, |history_model, ctx| {
                        if let Some(conversation) = history_model.conversation_mut(&conversation_id)
                        {
                            conversation.force_cancel_all_streaming_exchanges(
                                terminal_view_id,
                                reason,
                                ctx,
                            );
                        }
                    });
                }
            }
        }
    }

    /// Finalizes a conversation as a terminal failure because an agent-issued
    /// command caused the shell process to exit (e.g. it ran `exit`, or ran a
    /// failing command after enabling `set -e`).
    ///
    /// Invoked from the terminal view's shell-exit handler before the pane is
    /// torn down. The conversation is moved into a terminal `Error` state with a
    /// shell-exit message so that the Oz run reports `FAILED` (with an
    /// explanation) instead of "Cancelled by user", and so the subsequent
    /// pane-close cancellation — which is guarded by `is_in_progress` — becomes a
    /// no-op and cannot overwrite the failure.
    pub fn fail_conversation_due_to_shell_exit(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let terminal_surface_id = self.terminal_surface_id;
        let history_model = BlocklistAIHistoryModel::handle(ctx);

        // Only act on conversations that are still running. A finished
        // conversation (e.g. the agent already completed) must not be
        // retroactively marked as failed.
        let is_in_progress = history_model
            .as_ref(ctx)
            .conversation(&conversation_id)
            .is_some_and(|conversation| conversation.status().is_in_progress());
        if !is_in_progress {
            return;
        }

        // Finish any in-flight response stream(s) with the shell-exit error. This
        // marks the streaming exchange(s) as errored, sets the conversation to
        // `Error` (with a message), and renders the failure inline. We then cancel
        // the underlying request so it stops streaming; the cancellation does not
        // overwrite the status because `mark_request_cancelled` ignores the
        // `AgentExitedShell` reason.
        let stream_ids = self
            .in_flight_response_streams
            .stream_ids_for_conversation(conversation_id, ctx);
        let had_in_flight_stream = !stream_ids.is_empty();
        for stream_id in &stream_ids {
            history_model.update(ctx, |history_model, ctx| {
                history_model.mark_response_stream_completed_with_error(
                    RenderableAIError::AgentExitedShell,
                    /* recovery_pending */ false,
                    stream_id,
                    conversation_id,
                    terminal_surface_id,
                    ctx,
                );
            });
            self.try_cancel_pending_response_stream(
                stream_id,
                CancellationReason::AgentExitedShell,
                ctx,
            );
        }

        // Stop any pending or mid-execution actions so a queued action result
        // can't subsequently move the conversation back to Success/Cancelled.
        self.action_model.update(ctx, |action_model, ctx| {
            action_model.cancel_all_pending_actions(
                conversation_id,
                Some(CancellationReason::AgentExitedShell),
                ctx,
            );
        });

        // If there was no in-flight stream to attach the error to (e.g. the agent
        // ran `exit` and no follow-up request was in flight), set the terminal
        // `Error` status directly, recording the structured shell-exit error so
        // status consumers (Oz task sync and the ambient SDK driver) classify it
        // as FAILED rather than a generic ERROR.
        if !had_in_flight_stream {
            history_model.update(ctx, |history_model, ctx| {
                history_model.update_conversation_status_with_error(
                    terminal_surface_id,
                    conversation_id,
                    ConversationStatus::Error,
                    Some(RenderableAIError::AgentExitedShell),
                    ctx,
                );
            });
        }
    }

    /// Clears finished action results for a conversation. Used when reverting.
    pub fn clear_finished_action_results(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.action_model.update(ctx, |action_model, _| {
            action_model.clear_finished_action_results(conversation_id);
        });
    }

    /// Cancels the in-flight request for the given conversation, if there is one.
    ///
    /// Returns `true` if a request was actually cancelled.
    pub fn cancel_request(
        &mut self,
        response_stream_id: &ResponseStreamId,
        reason: CancellationReason,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        self.in_flight_response_streams
            .try_cancel_stream(response_stream_id, reason, ctx)
    }

    fn handle_response_stream_event(
        &mut self,
        did_input_contain_user_query: bool,
        event: &ResponseStreamEvent,
        response_stream: &ModelHandle<ResponseStream>,
        ctx: &mut ModelContext<Self>,
    ) {
        let stream_id = response_stream.as_ref(ctx).id().clone();

        match event {
            ResponseStreamEvent::ReceivedEvent(event) => {
                // Dynamic lookup handles conversation splits mid-stream.
                let Some(conversation_id) = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation_for_response_stream(&stream_id)
                else {
                    log::warn!("Could not find conversation for response stream: {stream_id:?}");
                    return;
                };
                let Some(event) = event.consume() else {
                    debug_assert!(
                        false,
                        "This model should only have a single subscriber that takes ownership over the event."
                    );
                    return;
                };
                let history_model = BlocklistAIHistoryModel::handle(ctx);
                match event {
                    Ok(api::StreamEvent::ToolProposed(action)) => {
                        #[cfg(not(target_family = "wasm"))]
                        let action_log_context = (
                            action.id.to_string(),
                            action.task_id.to_string(),
                            remote_action_tool_name(&action),
                            action.requires_result,
                        );
                        let failed_proposal = FailedToolProposal::new(&action, String::new());
                        let apply_result = history_model.update(ctx, |history_model, ctx| {
                            history_model.apply_domain_tool_proposal(
                                &stream_id,
                                conversation_id,
                                self.terminal_surface_id,
                                action,
                                ctx,
                            )
                        });
                        if let Err(error) = apply_result {
                            log::error!(
                                "Failed to apply Rig tool proposal to conversation: {error:?}"
                            );
                            let mut failed_proposal = failed_proposal;
                            failed_proposal.error = format!("{error:?}");
                            self.failed_tool_proposals_by_stream
                                .entry(stream_id.clone())
                                .or_default()
                                .push(failed_proposal);
                            #[cfg(not(target_family = "wasm"))]
                            {
                                let (action_id, task_id, tool_name, requires_result) =
                                    action_log_context;
                                remote_logging::log_model_event(
                                    ctx,
                                    RemoteLogRecord {
                                        level: RemoteLogLevel::Error,
                                        message: "Tool proposal apply failed".to_string(),
                                        context: serde_json::json!({
                                            "event": "tool_proposal_apply_failed",
                                            "stream_id": stream_id.as_str(),
                                            "conversation_id": conversation_id.to_string(),
                                            "action_id": action_id,
                                            "task_id": task_id,
                                            "tool_name": tool_name,
                                            "requires_result": requires_result,
                                            "error": remote_logging::sanitize_error(format!("{error:?}")),
                                        }),
                                    },
                                );
                            }
                        }
                    }
                    Ok(api::StreamEvent::Response(event)) => {
                        // If this controller is part of a shared session, forward the entire response event to viewers first.
                        if FeatureFlag::AgentSharedSessions.is_enabled()
                            && response_stream.as_ref(ctx).supports_shared_session_sync()
                        {
                            let mut model = self.terminal_model.lock();
                            if model.shared_session_status().is_sharer() {
                                // Get the participant who initiated this response, falling back to the sharer if needed.
                                let participant_id = self
                                    .get_current_response_initiator()
                                    .or_else(|| self.get_sharer_participant_id());

                                // For forked conversations (e.g. when loading from cloud), include
                                // the original conversation token so viewers can link the new
                                // server-assigned token to their existing conversation.
                                //
                                // This token is cleared after the first Init event (see below),
                                // so it's only sent once per forked conversation.
                                let forked_from_token = history_model
                                    .as_ref(ctx)
                                    .conversation(&conversation_id)
                                    .and_then(|conv| {
                                        conv.forked_from_server_conversation_token()
                                            .map(|t| t.as_str().to_string())
                                    });

                                model.send_agent_response_for_shared_session(
                                    &event,
                                    participant_id,
                                    forked_from_token,
                                );
                            }
                        }
                        let Some(event) = event.r#type else {
                            return;
                        };
                        match event {
                            warp_multi_agent_api::response_event::Type::Init(init_event) => {
                                history_model.update(ctx, |history_model, ctx| {
                                    #[cfg(not(target_family = "wasm"))]
                                    if let Some(session_id) = response_stream
                                        .as_ref(ctx)
                                        .acp_session_metadata()
                                        .and_then(|metadata| metadata.session_id)
                                    {
                                        history_model.set_acp_session_id(
                                            conversation_id,
                                            session_id,
                                            ctx,
                                        );
                                    }
                                    history_model.initialize_output_for_response_stream(
                                        &stream_id,
                                        conversation_id,
                                        self.terminal_surface_id,
                                        init_event,
                                        ctx,
                                    );

                                    // Clear the forked_from token after the first Init event.
                                    // For forked conversations, we only need to send this once so
                                    // viewers can update their conversation's server token. After
                                    // that, the viewer's conversation uses the new token directly.
                                    if let Some(conversation) =
                                        history_model.conversation_mut(&conversation_id)
                                    {
                                        conversation.clear_forked_from_server_conversation_token();
                                    }
                                });
                            }
                            warp_multi_agent_api::response_event::Type::Finished(
                                finished_event,
                            ) => {
                                self.handle_response_stream_finished(
                                    &stream_id,
                                    finished_event,
                                    conversation_id,
                                    did_input_contain_user_query,
                                    ctx,
                                );

                                // After the stream finishes, persist the full message
                                // history (input + assistant response) from the Arc back
                                // into the conversation for the next request cycle.
                                let new_history = response_stream
                                    .as_ref(ctx)
                                    .host_manages_history()
                                    .then(|| response_stream.as_ref(ctx).messages_sent().clone())
                                    .and_then(|messages_sent| {
                                        messages_sent.lock().ok().and_then(|sent| {
                                            if sent.is_empty() {
                                                None
                                            } else {
                                                Some(sent.clone())
                                            }
                                        })
                                    });
                                if let Some(mut new_history) = new_history {
                                    let history_model = BlocklistAIHistoryModel::handle(ctx);
                                    history_model.update(ctx, |history_model, _| {
                                        if let Some(conversation) =
                                            history_model.conversation_mut(&conversation_id)
                                        {
                                            let skip = conversation.messages_summarized_up_to();
                                            if skip > 0 && skip <= new_history.len() {
                                                let drained: Vec<_> = new_history
                                                    .iter()
                                                    .take(skip)
                                                    .cloned()
                                                    .collect();
                                                conversation.archive_tool_results(drained);
                                                let reconciled = new_history.split_off(skip);
                                                conversation.reset_messages_summarized_up_to();
                                                *conversation.bedrock_message_history_mut() =
                                                    reconciled;
                                            } else {
                                                *conversation.bedrock_message_history_mut() =
                                                    new_history;
                                            }
                                            log::info!(
                                    "[bedrock] Updated conversation bedrock history: {} messages",
                                    conversation.bedrock_message_history().len()
                                );
                                        }
                                    });
                                }
                            }
                            warp_multi_agent_api::response_event::Type::ClientActions(actions) => {
                                let client_actions = actions.actions;
                                let skill_path_origin = SessionContext::from_session(
                                    self.active_session.as_ref(ctx),
                                    ctx,
                                )
                                .skill_path_origin();
                                let apply_result =
                                    history_model.update(ctx, |history_model, ctx| {
                                        history_model.apply_client_actions(
                                            &stream_id,
                                            client_actions,
                                            conversation_id,
                                            self.terminal_surface_id,
                                            &skill_path_origin,
                                            ctx,
                                        )
                                    });
                                if let Err(e) = apply_result {
                                    log::error!(
                                        "Failed to apply client actions to conversation: {e:?}"
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if matches!(e.as_ref(), AIApiError::QuotaLimit { .. }) {
                            // If the error is a quota limit, we want to refresh workspace metadata
                            // So the current state of AI overages is immediately up to date.
                            TeamUpdateManager::handle(ctx).update(
                                ctx,
                                |team_update_manager, ctx| {
                                    std::mem::drop(
                                        team_update_manager.refresh_workspace_metadata(ctx),
                                    );
                                },
                            );
                            AIRequestUsageModel::handle(ctx).update(ctx, |model, ctx| {
                                model.enable_buy_credits_banner(ctx);
                            });
                        }

                        // Check if this error is eligible for corrective retry.
                        // Similar to loop detection, inject a message telling the LLM
                        // to try a different approach rather than just failing.
                        // Exclude errors that are proxy/config issues (cache_control,
                        // BadRequestError from LiteLLM) since the LLM can't fix those.
                        let error_str = format!("{e}");
                        let is_proxy_config_error = error_str.contains("cache_control")
                            || error_str.contains("tool_use` ids were found without")
                            || error_str.contains("BadRequestError");
                        let is_corrective_retry_candidate = !is_proxy_config_error
                            && !matches!(e.as_ref(), AIApiError::QuotaLimit { .. })
                            && (error_str.contains("ValidationException")
                                || error_str.contains("context window")
                                || error_str.contains("too many tokens")
                                || error_str.contains("input is too long")
                                || error_str.contains("throttl")
                                || error_str.contains("ThrottlingException"));

                        const MAX_ERROR_RETRIES: usize = 2;
                        let retry_count =
                            self.error_retry_counts.entry(conversation_id).or_insert(0);
                        let should_corrective_retry =
                            response_stream.as_ref(ctx).allows_corrective_retries()
                                && is_corrective_retry_candidate
                                && *retry_count < MAX_ERROR_RETRIES;

                        if should_corrective_retry {
                            *retry_count += 1;
                            let retry_num = *retry_count;
                            log::warn!(
                                "[error-retry] Attempting corrective retry {}/{} for conversation {:?}: {}",
                                retry_num,
                                MAX_ERROR_RETRIES,
                                conversation_id,
                                error_str
                            );

                            // Mark the error on the conversation but with recovery pending
                            let renderable_error = RenderableAIError::Other {
                                error_message: format!(
                                    "Error encountered, retrying with different approach (attempt {}/{})",
                                    retry_num, MAX_ERROR_RETRIES
                                ),
                                will_attempt_resume: true,
                                waiting_for_network: false,
                                is_user_error: false,
                            };
                            history_model.update(ctx, |history_model, ctx| {
                                history_model.mark_response_stream_completed_with_error(
                                    renderable_error,
                                    /*recovery_pending*/ true,
                                    &stream_id,
                                    conversation_id,
                                    self.terminal_surface_id,
                                    ctx,
                                );
                            });

                            // Inject a corrective message and resume
                            if let Some(conversation) =
                                BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
                            {
                                let root_task_id = conversation.get_root_task_id().clone();
                                let corrective_msg = format!(
                                    "[SYSTEM] The previous request resulted in an error: {}\n\n\
                                     Please try a completely different approach to accomplish the goal. \
                                     If the error is related to context size, reduce the amount of content \
                                     you are working with (read fewer files, use smaller commands, break \
                                     the task into smaller steps). If you cannot find an alternative, \
                                     explain to the user what is failing and why.",
                                    error_str
                                );

                                let inputs = vec![AIAgentInput::UserQuery {
                                    query: corrective_msg,
                                    context: Arc::from([]),
                                    static_query_type: None,
                                    referenced_attachments: HashMap::new(),
                                    user_query_mode: UserQueryMode::Normal,
                                    running_command: None,
                                    intended_agent: None,
                                }];

                                let _ = self.send_request_input(
                                    RequestInput::for_task(
                                        inputs,
                                        root_task_id,
                                        &self.active_session,
                                        self.get_current_response_initiator(),
                                        conversation_id,
                                        self.terminal_surface_id,
                                        ctx,
                                    ),
                                    None,
                                    /*can_attempt_resume_on_error*/ false,
                                    /*is_queued_prompt*/ false,
                                    ctx,
                                );
                            }
                        } else {
                            // Clear retry count on non-retryable errors or exhausted retries
                            self.error_retry_counts.remove(&conversation_id);

                            // A resume scheduled for this failure keeps the conversation in
                            // the non-terminal TransientError status instead of Error.
                            let recovery_pending = response_stream
                                .as_ref(ctx)
                                .should_resume_conversation_after_stream_finished();
                            let mut renderable_error: RenderableAIError = (&e).into();
                            if let RenderableAIError::Other {
                                will_attempt_resume,
                                waiting_for_network,
                                ..
                            }
                            | RenderableAIError::TransientNetworkError {
                                will_attempt_resume,
                                waiting_for_network,
                                ..
                            } = &mut renderable_error
                            {
                                // Rendering-only hints; state machine consumers key off the
                                // TransientError conversation status instead.
                                *will_attempt_resume |= recovery_pending;
                                if recovery_pending {
                                    let network_status = NetworkStatus::as_ref(ctx);
                                    *waiting_for_network = !network_status.is_online();
                                }
                            }

                            history_model.update(ctx, |history_model, ctx| {
                                history_model.mark_response_stream_completed_with_error(
                                    renderable_error,
                                    recovery_pending,
                                    &stream_id,
                                    conversation_id,
                                    self.terminal_surface_id,
                                    ctx,
                                );
                            });
                        }
                    }
                }
            }
            ResponseStreamEvent::WaitingForNetwork { waiting } => {
                let Some(conversation_id) = BlocklistAIHistoryModel::as_ref(ctx)
                    .conversation_for_response_stream(&stream_id)
                else {
                    log::warn!("Could not find conversation for response stream: {stream_id:?}");
                    return;
                };
                // Mirror the parked-retry state on the conversation: TransientError while
                // waiting for connectivity, back to InProgress when the retry fires.
                // This event is only emitted after a recoverable request failure parks a
                // retry while offline (see `defer_retry_until_online`), so treating
                // `waiting` as a transient-error state is always correct here.
                let status = if *waiting {
                    ConversationStatus::TransientError
                } else {
                    ConversationStatus::InProgress
                };
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    history_model.update_conversation_status(
                        self.terminal_surface_id,
                        conversation_id,
                        status,
                        ctx,
                    );
                });
            }
            ResponseStreamEvent::AfterStreamFinished {
                cancellation,
                proposed_actions,
            } => {
                // Cancellations provide conversation_id (survives truncation); otherwise use dynamic lookup.
                let conversation_id = match &cancellation {
                    Some(stream_cancellation) => stream_cancellation.conversation_id,
                    None => {
                        let Some(id) = BlocklistAIHistoryModel::as_ref(ctx)
                            .conversation_for_response_stream(&stream_id)
                        else {
                            log::warn!(
                                "Could not find conversation for response stream: {stream_id:?}"
                            );
                            return;
                        };
                        id
                    }
                };

                let history_model = BlocklistAIHistoryModel::handle(ctx);
                let Some((agent_id, new_exchange_ids, exchanges)) = history_model
                    .as_ref(ctx)
                    .conversation(&conversation_id)
                    .map(|conversation| {
                        (
                            match conversation.agent_backend() {
                                AgentBackend::Acp(acp) => {
                                    Some((acp.provider_id.clone(), acp.agent_id.clone()))
                                }
                                AgentBackend::Provider => None,
                            },
                            conversation
                                .new_exchange_ids_for_response(&stream_id)
                                .collect::<Vec<_>>(),
                            conversation.clone(),
                        )
                    })
                else {
                    log::warn!("Conversation not found.");
                    return;
                };
                #[cfg(not(target_family = "wasm"))]
                if let Some(metadata) = response_stream.as_ref(ctx).acp_session_metadata() {
                    if !metadata.config_options.is_empty() {
                        if let Some((provider_id, agent_id)) = &agent_id {
                            crate::settings::AISettings::handle(ctx).update(
                                ctx,
                                |settings, ctx| {
                                    let normalized_options =
                                        crate::ai::acp::AcpRuntimeModel::normalize_config_options(
                                            metadata.config_options.clone(),
                                        );
                                    let mut providers =
                                        settings.acp_providers.value().clone();
                                    if let Some(provider) = providers
                                        .iter_mut()
                                        .find(|provider| provider.id == provider_id.as_str())
                                    {
                                        provider.config_options = normalized_options;
                                        if let Err(error) =
                                            settings.acp_providers.set_value(providers, ctx)
                                        {
                                            log::warn!(
                                                "Failed to persist ACP provider runtime config: {error}"
                                            );
                                        }
                                    }
                                    if let Err(error) =
                                        crate::ai::acp::AcpRuntimeModel::persist_runtime_options(
                                            settings,
                                            agent_id,
                                            &metadata.config_options,
                                            ctx,
                                        )
                                    {
                                        log::warn!("Failed to persist ACP runtime config: {error}");
                                    }
                                },
                            );
                        }
                    }
                }
                log::info!(
                    "[bedrock-debug] AfterStreamFinished: stream_id={:?}, conversation_id={:?}, new_exchange_ids count={}",
                    stream_id, conversation_id, new_exchange_ids.len()
                );
                let mut was_passive_request = false;
                let mut is_any_exchange_unfinished = false;
                let mut actions_to_queue = vec![];

                for new_exchange_id in new_exchange_ids {
                    let Some(exchange) = exchanges.exchange_with_id(new_exchange_id) else {
                        log::warn!("Exchange not found.");
                        return;
                    };
                    was_passive_request |= exchange.has_passive_request();
                    is_any_exchange_unfinished |= !exchange.output_status.is_finished();
                    log::info!(
                        "[bedrock-debug] AfterStreamFinished: exchange_id={:?}, is_finished={}, output_status={:?}",
                        new_exchange_id,
                        exchange.output_status.is_finished(),
                        std::mem::discriminant(&exchange.output_status)
                    );

                    if let AIAgentOutputStatus::Finished {
                        finished_output: FinishedAIAgentOutput::Success { output },
                        ..
                    } = &exchange.output_status
                    {
                        let action_count = output.get().actions().count();
                        let msg_count = output.get().messages.len();
                        log::info!(
                            "[bedrock-debug] AfterStreamFinished: output has {} messages, {} actions",
                            msg_count, action_count
                        );
                        for msg in output.get().messages.iter() {
                            log::info!(
                                "[bedrock-debug] AfterStreamFinished:   msg type={:?}",
                                std::mem::discriminant(&msg.message)
                            );
                        }
                        actions_to_queue.extend(output.get().actions().cloned());
                    }
                }

                let history_action_count = actions_to_queue.len();
                let proposed_action_count = proposed_actions.len();
                let failed_tool_proposals = self
                    .failed_tool_proposals_by_stream
                    .remove(&stream_id)
                    .unwrap_or_default();
                let mut queued_action_ids = actions_to_queue
                    .iter()
                    .map(|action| action.id.clone())
                    .collect::<HashSet<_>>();
                let mut queued_from_stream_snapshot_count = 0;
                for action in proposed_actions {
                    if queued_action_ids.insert(action.id.clone()) {
                        queued_from_stream_snapshot_count += 1;
                        actions_to_queue.push(action.clone());
                    }
                }
                let active_child_conversation_ids =
                    active_descendant_conversation_ids(history_model.as_ref(ctx), conversation_id);
                let queue_decision = tool_queue_decision(
                    cancellation.is_some(),
                    is_any_exchange_unfinished,
                    !failed_tool_proposals.is_empty(),
                    !active_child_conversation_ids.is_empty(),
                    actions_to_queue.len(),
                    queued_from_stream_snapshot_count,
                );
                #[cfg(not(target_family = "wasm"))]
                {
                    remote_logging::log_model_event(
                        ctx,
                        RemoteLogRecord {
                            level: queue_decision.remote_log_level(),
                            message: "Tool queue decision".to_string(),
                            context: serde_json::json!({
                                "event": "tool_queue_decision",
                                "stream_id": stream_id.as_str(),
                                "conversation_id": conversation_id.to_string(),
                                "decision": queue_decision.label(),
                                "history_action_count": history_action_count,
                                "proposed_action_count": proposed_action_count,
                                "candidate_action_count": actions_to_queue.len(),
                                "will_queue_action_count": if queue_decision.will_queue_actions() {
                                    actions_to_queue.len()
                                } else {
                                    0
                                },
                                "queued_from_stream_snapshot_count": queued_from_stream_snapshot_count,
                                "failed_tool_proposal_count": failed_tool_proposals.len(),
                                "failed_tool_proposals": failed_tool_proposals
                                    .iter()
                                    .map(FailedToolProposal::to_remote_log_value)
                                    .collect::<Vec<_>>(),
                                "active_descendant_conversation_ids": active_child_conversation_ids
                                    .iter()
                                    .map(ToString::to_string)
                                    .collect::<Vec<_>>(),
                                "was_passive_request": was_passive_request,
                                "is_any_exchange_unfinished": is_any_exchange_unfinished,
                                "cancellation_reason": cancellation
                                    .as_ref()
                                    .map(|stream_cancellation| format!("{:?}", stream_cancellation.reason)),
                                "queued_tools": remote_action_summaries(&actions_to_queue),
                                "proposed_tools": remote_action_summaries(&proposed_actions),
                            }),
                        },
                    );
                }

                if let Some(stream_cancellation) = &cancellation {
                    // If this is a shared session, send a synthetic StreamFinished event to notify viewers
                    // of any user-initiated cancellation. We skip internal cancellations that preserve
                    // the conversation's InProgress status, such as follow-ups and CLI subagent user
                    // takeover, because those do not end the conversation.
                    if FeatureFlag::AgentSharedSessions.is_enabled()
                        && !matches!(
                            stream_cancellation.reason.conversation_outcome(),
                            CancellationOutcome::KeepInProgress
                        )
                    {
                        // For any terminal status (not just canceled), we need to inform viewers the stream has stopped.
                        self.send_cancellation_to_viewers(ctx);
                    }

                    history_model.update(ctx, |history_model, ctx| {
                        history_model.mark_response_stream_cancelled(
                            &stream_id,
                            conversation_id,
                            self.terminal_surface_id,
                            stream_cancellation.reason,
                            ctx,
                        );
                    });

                    if !was_passive_request
                        && matches!(
                            stream_cancellation.reason.conversation_outcome(),
                            CancellationOutcome::Cancelled
                        )
                    {
                        self.set_input_mode_for_cancellation(ctx);
                    }
                } else if is_any_exchange_unfinished {
                    // Defensive: truncated streams are detected inside `ResponseStream`,
                    // so an unfinished exchange here means an unexpected completion path.
                    log::warn!(
                        "Response stream completed with an unfinished exchange and no error event."
                    );

                    history_model.update(ctx, |history_model, ctx| {
                        history_model.mark_response_stream_completed_with_error(
                            RenderableAIError::transient_network_error(
                                false,
                                false,
                                TransientNetworkErrorKind::UnfinishedExchange,
                            ),
                            /*recovery_pending*/ false,
                            &stream_id,
                            conversation_id,
                            self.terminal_surface_id,
                            ctx,
                        );
                    });
                } else if !failed_tool_proposals.is_empty() {
                    log::warn!(
                        "Skipping tool queue for stream {stream_id:?}: failed tool proposal attach count={}",
                        failed_tool_proposals.len()
                    );
                } else if !active_child_conversation_ids.is_empty() {
                    log::info!(
                        "Skipping tool queue for conversation {conversation_id:?}: active child conversations remain: {:?}",
                        active_child_conversation_ids
                    );
                } else if queue_decision.will_queue_actions() {
                    log::info!(
                        "[bedrock-debug] AfterStreamFinished: queuing {} actions",
                        actions_to_queue.len()
                    );
                    self.action_model.update(ctx, |action_model, ctx| {
                        action_model.queue_actions(actions_to_queue, conversation_id, ctx);
                    });
                } else {
                    log::warn!(
                        "[bedrock-debug] AfterStreamFinished: NO actions to queue, was_passive={}, is_any_unfinished={}",
                        was_passive_request, is_any_exchange_unfinished
                    );
                    // If this is a child conversation (has a parent) and the
                    // stream ended with EndTurn and no actions, the child agent
                    // is done. Mark it as Success so the StartAgentExecutor
                    // can resolve and return the output to the parent.
                    let is_child = history_model
                        .as_ref(ctx)
                        .conversation(&conversation_id)
                        .and_then(|c| c.parent_conversation_id())
                        .is_some();
                    let (host_manages_history, had_failed_tool_result, tool_result_count) = {
                        let response_stream = response_stream.as_ref(ctx);
                        (
                            response_stream.host_manages_history(),
                            response_stream.has_error_tool_results(),
                            response_stream.tool_result_count(),
                        )
                    };
                    let recovery_reason = if !is_child
                        && !was_passive_request
                        && !host_manages_history
                    {
                        let agent_output = self.extract_last_agent_output(conversation_id, ctx);
                        no_action_tool_error_recovery_reason(had_failed_tool_result, &agent_output)
                    } else {
                        None
                    };

                    if let Some(recovery_reason) = recovery_reason {
                        log::warn!(
                            "[tool-error-recovery] Sending corrective follow-up for \
                             conversation {:?}: reason={}, tool_result_count={}",
                            conversation_id,
                            recovery_reason,
                            tool_result_count
                        );
                        #[cfg(not(target_family = "wasm"))]
                        remote_logging::log_model_event(
                            ctx,
                            RemoteLogRecord {
                                level: RemoteLogLevel::Warn,
                                message: "Tool error no-action recovery".to_string(),
                                context: serde_json::json!({
                                    "event": "tool_error_no_action_recovery",
                                    "stream_id": stream_id.as_str(),
                                    "conversation_id": conversation_id.to_string(),
                                    "reason": recovery_reason,
                                    "tool_result_count": tool_result_count,
                                    "was_passive_request": was_passive_request,
                                    "is_child": is_child,
                                    "host_manages_history": host_manages_history,
                                }),
                            },
                        );

                        // Remove the completed stream before starting the corrective turn.
                        // Otherwise `send_request_input` sees this conversation as in-flight
                        // and rejects the recovery request.
                        self.in_flight_response_streams.cleanup_stream(&stream_id);
                        self.send_tool_error_no_action_recovery(
                            conversation_id,
                            recovery_reason,
                            ctx,
                        );
                    } else if is_child && !was_passive_request {
                        log::info!(
                            "[bedrock-debug] AfterStreamFinished: child conversation {:?} completed, setting status to Success",
                            conversation_id
                        );
                        history_model.update(ctx, |history_model, ctx| {
                            history_model.update_conversation_status(
                                self.terminal_surface_id,
                                conversation_id,
                                crate::ai::agent::conversation::ConversationStatus::Success,
                                ctx,
                            );
                        });

                        if cancellation.is_none() {
                            self.in_flight_response_streams.cleanup_stream(&stream_id);

                            // Now that the stream is cleaned up, re-check for pending
                            // orchestration events that couldn't be drained earlier.
                            self.handle_pending_events_ready(conversation_id, ctx);
                        }
                    } else {
                        // Crosscheck Work experiment: remember that the main agent produced a final
                        // response. Start the reviewer only after stream cleanup below so fast reviewer
                        // feedback cannot race the stale in-flight response-stream entry.
                        let should_trigger_crosscheck = !is_child && !was_passive_request;

                        // Remove the completed stream before starting the reviewer. A fast reviewer can
                        // otherwise return feedback while this stream is still considered in flight,
                        // causing `send_request_input` to reject and silently drop the correction turn.
                        if cancellation.is_none() {
                            self.in_flight_response_streams.cleanup_stream(&stream_id);

                            // Now that the stream is cleaned up, re-check for pending
                            // orchestration events that couldn't be drained earlier.
                            self.handle_pending_events_ready(conversation_id, ctx);
                        }

                        if should_trigger_crosscheck {
                            self.maybe_trigger_crosscheck(conversation_id, ctx);
                        }
                    }
                }

                // Cancelled streams handle pending-response-stream updates synchronously. The
                // no-action crosscheck path above also cleans up early before starting its reviewer.
                if cancellation.is_none() && self.in_flight_response_streams.has_stream(&stream_id)
                {
                    self.in_flight_response_streams.cleanup_stream(&stream_id);

                    // Now that the stream is cleaned up, re-check for pending
                    // orchestration events that couldn't be drained earlier.
                    self.handle_pending_events_ready(conversation_id, ctx);
                }

                // Before cleaning up the response stream, check if we should attempt to resume.
                if response_stream
                    .as_ref(ctx)
                    .should_resume_conversation_after_stream_finished()
                {
                    self.schedule_auto_resume_after_error(conversation_id, ctx);
                }

                // Clean up the response stream tracking entry now that the stream is complete.
                history_model.update(ctx, |history_model, _| {
                    if let Some(conversation) = history_model.conversation_mut(&conversation_id) {
                        conversation.cleanup_completed_response_stream(&stream_id);
                    }
                });
                ctx.unsubscribe_from_model(response_stream);

                if self.should_refresh_available_llms_on_stream_finish {
                    self.should_refresh_available_llms_on_stream_finish = false;
                    LLMPreferences::handle(ctx).update(ctx, |llm_preferences, ctx| {
                        llm_preferences.refresh_authed_models(ctx);
                    });
                }
                ctx.emit(BlocklistAIControllerEvent::FinishedReceivingOutput {
                    stream_id,
                    conversation_id,
                });
                AIRequestUsageModel::handle(ctx).update(ctx, |request_usage_model, ctx| {
                    request_usage_model.refresh_request_usage_async(ctx);
                });

                self.maybe_refresh_ai_overages(ctx);
            }
        }
    }

    /// Sets the terminal input state after an AI request is cancelled.
    /// From the user perspective, we downgrade the level of autonomy so:
    /// * Executing a task automatically -> interactive AI input
    /// * Interactive AI input -> interactive shell input
    fn set_input_mode_for_cancellation(&mut self, ctx: &mut ModelContext<Self>) {
        // If the request was cancelled, default to shell mode with autodetection
        // enabled.
        self.input_model.update(ctx, |input_model, ctx| {
            input_model.set_input_config_for_classic_mode(
                input_model
                    .input_config()
                    .with_shell_type()
                    .unlocked_if_autodetection_enabled(false, ctx),
                ctx,
            );
        });
    }

    /// Checks if we should refresh AI overage information after an AI request completes.
    /// This is used to ensure the UI matches the state of the workspace,
    /// especially because overages are not real-time communicated to clients.
    fn maybe_refresh_ai_overages(&mut self, ctx: &mut ModelContext<Self>) {
        let workspace = UserWorkspaces::as_ref(ctx).current_workspace();
        let Some(workspace) = workspace else {
            return;
        };

        // We want to minimize the number of times we ping our backend for updated usage information;
        // doing it after every AI query finishes would be very expensive.

        // If a user is below their personal limits, then we know that they won't eat into overages,
        // so we don't need to refresh.
        let has_no_requests_remaining = !AIRequestUsageModel::as_ref(ctx).has_requests_remaining();
        // If overages aren't enabled, we're not going to reap the benefit of refreshing at all anyway.
        let are_overages_enabled = workspace.are_overages_enabled();

        if are_overages_enabled && has_no_requests_remaining {
            // Give a one second delay to ensure that Stripe has been charged and the database is completely updated,
            // before syncing new AI overages data.
            ctx.spawn(
                async move { Timer::after(Duration::from_secs(1)).await },
                |_, _, ctx| {
                    UserWorkspaces::handle(ctx).update(ctx, |user_workspaces, ctx| {
                        user_workspaces.refresh_ai_overages(ctx);
                    });
                },
            );
        }
    }

    pub(super) fn handle_response_stream_finished(
        &mut self,
        stream_id: &ResponseStreamId,
        mut finished_event: warp_multi_agent_api::response_event::StreamFinished,
        conversation_id: AIConversationId,
        did_request_contain_user_query: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        history_model.update(ctx, |history_model, ctx| {
            // Update conversation cost and usage information before updating and
            // persisting the conversation.
            history_model.update_conversation_cost_and_usage_for_request(
                conversation_id,
                finished_event.request_cost.map(|cost| {
                    // Total credits charged for this request = inference (`exact`) + platform.
                    RequestCost::new(f64::from(cost.exact) + f64::from(cost.platform_credits))
                }),
                finished_event.token_usage,
                finished_event.conversation_usage_metadata.take(),
                did_request_contain_user_query,
                ctx,
            );
        });

        let history_model = BlocklistAIHistoryModel::handle(ctx);
        match finished_event.reason {
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::Done(_)) | None => {
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_successfully(
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::Other(_)) => {
                let error_message = "Response stream finished unexpectedly (with finish reason `Other`).";
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        RenderableAIError::Other {
                            error_message: error_message.to_owned(),
                            will_attempt_resume: false,
                            waiting_for_network: false,
                            is_user_error: false,
                        },
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::ContextWindowExceeded(_)) => {
                let error_message = "Input exceeded context window limit.";
                crate::ai::bedrock::crash_log::log_crash(
                    "ContextWindowExceeded",
                    error_message,
                    "unknown",
                    0,
                    None,
                );
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        RenderableAIError::ContextWindowExceeded(error_message.to_owned()),
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::QuotaLimit(_)) => {
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        RenderableAIError::QuotaLimit {
                            user_display_message: None,
                        },
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::LlmUnavailable(_)) => {
                let error_message = "The LLM is currently unavailable.";
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        RenderableAIError::Other {
                            error_message: error_message.to_owned(),
                            will_attempt_resume: false,
                            waiting_for_network: false,
                            is_user_error: false,
                        },
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::InvalidApiKey(details)) => {
                use warp_multi_agent_api::LlmProvider;
                let is_aws_bedrock = details
                    .provider
                    .try_into()
                    .ok()
                    .is_some_and(|p: LlmProvider| p == LlmProvider::AwsBedrock);

                let error = if is_aws_bedrock {
                    RenderableAIError::AwsBedrockCredentialsExpiredOrInvalid {
                        model_name: details.model_name,
                    }
                } else {
                    let provider = details.provider.try_into().ok().and_then(|p| match p {
                        LlmProvider::Google => Some("Google"),
                        LlmProvider::Anthropic => Some("Anthropic"),
                        LlmProvider::Openai => Some("OpenAI"),
                        LlmProvider::Xai => Some("xAI"),
                        LlmProvider::Openrouter => Some("OpenRouter"),
                        LlmProvider::AwsBedrock | LlmProvider::Unknown => None,
                    });
                    RenderableAIError::InvalidApiKey {
                        provider: provider.unwrap_or("Unknown").to_string(),
                        model_name: details.model_name,
                    }
                };

                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        error,
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::InternalError(
                warp_multi_agent_api::response_event::stream_finished::InternalError{ message})) => {
                let error_message = format!(
                    "Response stream finished unexpectedly with internal error: {message}",
                );
                crate::ai::bedrock::crash_log::log_crash(
                    "InternalError",
                    &error_message,
                    "unknown",
                    0,
                    None,
                );
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        RenderableAIError::Other {
                            error_message,
                            will_attempt_resume: false,
                            waiting_for_network: false,
                            is_user_error: false,
                        },
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
            Some(warp_multi_agent_api::response_event::stream_finished::Reason::MaxTokenLimit(_)) => {
                let error_message = "Input exceeded context window limit.";
                crate::ai::bedrock::crash_log::log_crash(
                    "MaxTokenLimit",
                    error_message,
                    "unknown",
                    0,
                    None,
                );
                history_model.update(ctx, |history_model, ctx| {
                    history_model.mark_response_stream_completed_with_error(
                        RenderableAIError::ContextWindowExceeded(error_message.to_owned()),
                        /*recovery_pending*/ false,
                        stream_id,
                        conversation_id,
                        self.terminal_surface_id,
                        ctx,
                    );
                });
            }
        }

        if finished_event.should_refresh_model_config {
            LLMPreferences::handle(ctx).update(ctx, |llm_preferences, ctx| {
                llm_preferences.refresh_authed_models(ctx);
            });
            ctx.emit(BlocklistAIControllerEvent::FreeTierLimitCheckTriggered);
        }

        // Progressive summarization: when context window usage >= 85% and we have
        // more than 100 messages, summarize the oldest messages while keeping the
        // most recent 100 verbatim. This runs as a background Bedrock call — no UI,
        // no exchange created, no tool execution shown.
        let should_progressive_summarize = {
            let history_model = BlocklistAIHistoryModel::as_ref(ctx);
            history_model
                .conversation(&conversation_id)
                .is_some_and(|conversation| {
                    let is_summarization_request =
                        conversation.latest_exchange().is_some_and(|exchange| {
                            exchange
                                .input
                                .iter()
                                .any(|i| matches!(i, AIAgentInput::SummarizeConversation { .. }))
                        });
                    conversation.context_window_usage() >= 0.85
                        && !conversation.has_pending_progressive_summary()
                        && !is_summarization_request
                        && conversation.bedrock_message_history().len() > 100
                })
        };

        if should_progressive_summarize {
            self.trigger_progressive_summarization(conversation_id, ctx);
        }
    }

    fn trigger_progressive_summarization(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        use settings::Setting;

        use crate::ai::bedrock::client::{BedrockClient, BedrockClientConfig};
        use crate::ai::bedrock::convert::{ConversationMessage, MessageContent, MessageRole};
        use crate::ai::bedrock::response_translator::{
            context_window_for_model, estimate_cost_cents,
        };
        use crate::settings::ai::AISettings;

        let settings = AISettings::as_ref(ctx);
        if !*settings.bedrock_enabled.value() {
            return;
        }

        let api_key_manager = ::ai::api_keys::ApiKeyManager::as_ref(ctx);
        let mut config = BedrockClientConfig {
            auth_method: *settings.bedrock_auth_method.value(),
            profile: settings.bedrock_profile.value().clone(),
            region: settings.bedrock_region.value().clone(),
            access_key_id: settings.bedrock_access_key_id.value().clone(),
            secret_access_key: settings.bedrock_secret_access_key.value().clone(),
            session_token: None,
            cross_region_inference: *settings.bedrock_cross_region_inference.value(),
            use_rig: false,
        };

        if let ::ai::api_keys::AwsCredentialsState::Loaded { credentials, .. } =
            api_key_manager.aws_credentials_state()
        {
            config.auth_method = crate::settings::BedrockAuthMethod::StaticKeys;
            config.access_key_id = credentials.access_key().to_string();
            config.secret_access_key = credentials.secret_key().to_string();
            config.session_token = credentials.session_token().map(|s| s.to_string());
        }

        let config = config.with_external_fallbacks();

        let cross_region = config.cross_region_inference;
        // Use Sonnet for summarization — cheaper and fast enough for this task
        let model_id = "us.anthropic.claude-sonnet-4-6-20250514-v1:0".to_string();

        let history_model = BlocklistAIHistoryModel::handle(ctx);

        // Extract the messages to summarize and set the guard flag
        let (messages_to_summarize, existing_summary, messages_count) = {
            let history = history_model.as_ref(ctx);
            let Some(conversation) = history.conversation(&conversation_id) else {
                return;
            };

            let history_len = conversation.bedrock_message_history().len();
            let split_point = history_len.saturating_sub(100);
            if split_point == 0 {
                return;
            }

            let msgs: Vec<ConversationMessage> =
                conversation.bedrock_message_history()[..split_point].to_vec();
            let existing = conversation.progressive_summary().map(str::to_string);

            (msgs, existing, split_point)
        };

        history_model.update(ctx, |history_model, _| {
            if let Some(conversation) = history_model.conversation_mut(&conversation_id) {
                conversation.set_has_pending_progressive_summary(true);
            }
        });

        log::info!(
            "[progressive-summary] Triggering for conversation {:?}: summarizing {} messages, keeping last 100",
            conversation_id,
            messages_count
        );

        // Build the summarization input
        let mut summarize_content = String::new();
        if let Some(ref prior) = existing_summary {
            summarize_content.push_str("<prior-summary>\n");
            summarize_content.push_str(prior);
            summarize_content.push_str("\n</prior-summary>\n\n");
        }
        summarize_content.push_str("<messages-to-summarize>\n");
        fn safe_truncate(s: &str, max_chars: usize) -> String {
            if s.len() <= max_chars {
                s.to_string()
            } else {
                let trunc = s.chars().take(max_chars).collect::<String>();
                format!("{trunc}... [truncated, {len} total chars]", len = s.len())
            }
        }

        for msg in &messages_to_summarize {
            let role_str = match msg.role {
                MessageRole::User => "User",
                MessageRole::Assistant => "Assistant",
            };
            let content_str = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::ToolUse { name, input, .. } => {
                    format!("[Tool Call: {}] {}", name, input)
                }
                MessageContent::ToolResult { content, .. } => safe_truncate(content, 2000),
                MessageContent::MultiPart(parts) => {
                    use crate::ai::bedrock::convert::ContentPart;
                    parts
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text(t) => t.clone(),
                            ContentPart::Reasoning { text, .. } => {
                                format!("[Reasoning] {text}")
                            }
                            ContentPart::Image { .. } => "[Image attachment]".to_string(),
                            ContentPart::ToolUse { name, input, .. } => {
                                format!("[Tool: {}] {}", name, input)
                            }
                            ContentPart::ToolResult { content, .. } => safe_truncate(content, 2000),
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            };
            summarize_content.push_str(&format!("[{}]: {}\n", role_str, content_str));
        }
        summarize_content.push_str("</messages-to-summarize>");

        let summarize_prompt = "Summarize the following conversation history. Preserve:\n\
            - All decisions made and their rationale\n\
            - All file paths modified and what was changed\n\
            - All tool calls with their significant results (commands run, files read, errors encountered)\n\
            - Current task state and any pending work\n\
            - Technical details, code patterns, and architecture discussed\n\n\
            Be comprehensive. This summary will be the only record of these exchanges.";

        let summarize_messages = vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(format!("{}\n\n{}", summarize_prompt, summarize_content)),
        }];

        // Spawn the background Bedrock call
        let model_id_clone = model_id.clone();
        ctx.spawn(
            async move {
                let client = BedrockClient::from_config(config).await?;
                client
                    .converse_collect(
                        &model_id_clone,
                        summarize_messages,
                        None,
                        16000,
                        cross_region,
                    )
                    .await
            },
            move |me, result, ctx| {
                let history_model = BlocklistAIHistoryModel::handle(ctx);
                match result {
                    Ok((summary_text, input_tokens, output_tokens)) => {
                        log::info!(
                            "[progressive-summary] Completed for {:?}: {} chars, input={} output={} tokens",
                            conversation_id,
                            summary_text.len(),
                            input_tokens,
                            output_tokens,
                        );

                        let cost_cents = estimate_cost_cents(
                            input_tokens,
                            output_tokens,
                            0,
                            0,
                            &model_id,
                        );

                        // Use the conversation's active model for context window sizing,
                        // not the summarizer model.
                        let active_model_id = crate::ai::llms::LLMPreferences::as_ref(ctx)
                            .get_active_base_model(ctx, Some(me.terminal_surface_id))
                            .id
                            .to_string();

                        history_model.update(ctx, |history_model, _| {
                            if let Some(conversation) =
                                history_model.conversation_mut(&conversation_id)
                            {
                                // Drain the summarized messages from history
                                let drain_count =
                                    messages_count.min(conversation.bedrock_message_history().len());
                                let drained: Vec<_> = conversation
                                    .bedrock_message_history()
                                    .iter()
                                    .take(drain_count)
                                    .cloned()
                                    .collect();
                                conversation.archive_tool_results(drained);
                                conversation
                                    .bedrock_message_history_mut()
                                    .drain(0..drain_count);

                                conversation
                                    .set_progressive_summary(Some(summary_text.clone()), drain_count);
                                conversation.set_has_pending_progressive_summary(false);

                                // Estimate new context window usage
                                let summary_tokens = (summary_text.len() / 4) as u32;
                                let remaining_msgs_tokens: u32 = conversation
                                    .bedrock_message_history()
                                    .iter()
                                    .map(|m| match &m.content {
                                        MessageContent::Text(t) => (t.len() / 4) as u32,
                                        MessageContent::ToolUse { input, .. } => {
                                            (input.to_string().len() / 4) as u32 + 20
                                        }
                                        MessageContent::ToolResult { content, .. } => {
                                            (content.len() / 4) as u32
                                        }
                                        MessageContent::MultiPart(parts) => {
                                            parts
                                                .iter()
                                                .map(|p| match p {
                                                    ContentPart::Text(t) => (t.len() / 4) as u32,
                                                    ContentPart::Reasoning { text, .. } => {
                                                        (text.len() / 4) as u32
                                                    }
                                                    ContentPart::Image { .. } => 1_600,
                                                    ContentPart::ToolUse { input, .. } => {
                                                        (input.to_string().len() / 4) as u32
                                                    }
                                                    ContentPart::ToolResult { content, .. } => {
                                                        (content.len() / 4) as u32
                                                    }
                                                })
                                                .sum()
                                        }
                                    })
                                    .sum();

                                let max_ctx = context_window_for_model(&active_model_id);
                                let new_usage =
                                    (summary_tokens + remaining_msgs_tokens) as f32 / max_ctx as f32;
                                conversation.set_context_window_usage(new_usage.clamp(0.0, 1.0));
                                conversation
                                    .set_current_context_tokens(summary_tokens + remaining_msgs_tokens);

                                log::info!(
                                    "[progressive-summary] Post-summary: ~{} tokens ({:.1}% of {} context), {} messages retained",
                                    summary_tokens + remaining_msgs_tokens,
                                    new_usage * 100.0,
                                    active_model_id,
                                    conversation.bedrock_message_history().len()
                                );
                            }
                        });

                        // Update cost tracking
                        history_model.update(ctx, |history_model, ctx| {
                            use warp_multi_agent_api::response_event::stream_finished;
                            let token_usage = vec![stream_finished::TokenUsage {
                                model_id: "bedrock".to_string(),
                                total_input: input_tokens,
                                output: output_tokens,
                                input_cache_read: 0,
                                input_cache_write: 0,
                                cost_in_cents: cost_cents,
                            }];
                            history_model.update_conversation_cost_and_usage_for_request(
                                conversation_id,
                                None,
                                token_usage,
                                None,
                                false,
                                ctx,
                            );
                        });
                    }
                    Err(e) => {
                        log::error!(
                            "[progressive-summary] Failed for {:?}: {:?}",
                            conversation_id,
                            e
                        );
                        history_model.update(ctx, |history_model, _| {
                            if let Some(conversation) =
                                history_model.conversation_mut(&conversation_id)
                            {
                                conversation.set_has_pending_progressive_summary(false);
                            }
                        });
                    }
                }
                let _ = me;
            },
        );
    }
}

impl Entity for BlocklistAIController {
    type Event = BlocklistAIControllerEvent;
}

#[derive(Clone)]
pub struct ClientIdentifiers {
    pub conversation_id: AIConversationId,
    pub client_exchange_id: AIAgentExchangeId,
    /// Not populated for restored AI blocks.
    pub response_stream_id: Option<ResponseStreamId>,
}

/// Returns `true` if the given error message from a Bedrock stream indicates an
/// AWS credentials issue (expired, invalid, or missing session token).
fn is_bedrock_credentials_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    // "Session token not found or invalid" is the most common SSO expiry message.
    // AccessDenied / ExpiredToken / UnrecognizedClient cover other credential failures.
    // SSO cache file not found means the token file was deleted or never created.
    lower.contains("session token not found")
        || lower.contains("expiredtoken")
        || lower.contains("expired token")
        || lower.contains("unrecognizedclientexception")
        || lower.contains("unauthorizedexception")
        || (lower.contains("sso/cache") && lower.contains("notfound"))
        || (lower.contains("sso/cache") && lower.contains("no such file"))
        || (lower.contains("accessdenied")
            && (lower.contains("token")
                || lower.contains("credential")
                || lower.contains("security")))
}

#[allow(clippy::too_many_arguments)]
fn input_for_query(
    query: String,
    task_id: &TaskId,
    conversation_id: AIConversationId,
    static_query_type: Option<StaticQueryType>,
    user_query_mode: UserQueryMode,
    running_command: Option<RunningCommand>,
    additional_attachments: HashMap<String, AIAgentAttachment>,
    prompt_attachments: Vec<PendingAttachment>,
    context_model: &BlocklistAIContextModel,
    active_session: &ActiveSession,
    app: &AppContext,
) -> AIAgentInput {
    // Split the resolved attachment set into image context (sent inline) and file references.
    let mut image_context = Vec::new();
    let mut file_attachments = Vec::new();
    for attachment in prompt_attachments {
        match attachment {
            PendingAttachment::Image(image) => image_context.push(AIAgentContext::Image(image)),
            PendingAttachment::File(file) => file_attachments.push(file),
        }
    }

    let context =
        input_context_for_request(true, context_model, active_session, image_context, app);
    let intended_agent = BlocklistAIHistoryModel::as_ref(app)
        .conversation(&conversation_id)
        .and_then(|c| c.get_task(task_id))
        .and_then(|task| {
            if task.is_root_task() {
                Some(warp_multi_agent_api::AgentType::Primary)
            } else if task.is_cli_subagent() {
                Some(warp_multi_agent_api::AgentType::Cli)
            } else {
                None
            }
        });
    let mut referenced_attachments = parse_context_attachments(&query, context_model, app);
    referenced_attachments.extend(additional_attachments);
    add_pending_file_attachments(&mut referenced_attachments, file_attachments);

    AIAgentInput::UserQuery {
        query,
        context,
        static_query_type,
        referenced_attachments,
        user_query_mode,
        running_command,
        intended_agent,
    }
}

pub(super) fn add_pending_file_attachments(
    referenced_attachments: &mut HashMap<String, AIAgentAttachment>,
    file_attachments: Vec<PendingFile>,
) {
    for file in file_attachments {
        let attachment = AIAgentAttachment::FilePathReference {
            file_id: uuid::Uuid::new_v4().to_string(),
            file_name: file.file_name.clone(),
            file_path: file.file_path.to_string_lossy().to_string(),
        };
        let mut key = file.file_name.clone();
        if referenced_attachments.contains_key(&key) {
            let mut suffix = 1;
            loop {
                key = format!("{} ({suffix})", file.file_name);
                if !referenced_attachments.contains_key(&key) {
                    break;
                }
                suffix += 1;
            }
        }
        referenced_attachments.insert(key, attachment);
    }
}

/// Validates that tool call results have corresponding tool calls in the task context, otherwise
/// logs a warning.
fn validate_tool_call_results<'a>(
    inputs: impl Iterator<Item = &'a AIAgentInput>,
    tasks: &[Task],
    server_conversation_token: &Option<ServerConversationToken>,
) {
    // Create a mapping from tool call IDs to their task IDs
    let mut tool_call_to_task_map: HashMap<String, String> = HashMap::new();
    for task in tasks {
        for message in &task.messages {
            if let Some(message::Message::ToolCall(tool_call)) = &message.message {
                tool_call_to_task_map
                    .insert(tool_call.tool_call_id.clone(), message.task_id.clone());
            }
        }
    }

    // Check each input for tool call results and validate they have corresponding tool calls
    for input in inputs {
        if let AIAgentInput::ActionResult { result, .. } = input {
            let action_id_str = result.id.to_string();
            let server_conversation_id = server_conversation_token
                .as_ref()
                .map(|token| token.as_str())
                .unwrap_or("None");

            if !tool_call_to_task_map.contains_key(&action_id_str) {
                log::warn!(
                    "Found tool call result with ID '{action_id_str}' but no corresponding tool \
                    call in task context. Server conversation ID: '{server_conversation_id}'"
                );
            }
        }
    }
}

fn get_running_command(terminal_model: &TerminalModel) -> Option<RunningCommand> {
    let active_block = terminal_model.block_list().active_block();
    if !active_block.is_active_and_long_running() || active_block.is_agent_monitoring() {
        return None;
    }
    Some(running_command_snapshot(terminal_model))
}

/// Returns the active command when it is unclaimed or already monitored by the
/// requested conversation. This keeps steering on the CLI task and preserves
/// the terminal-specialized model/tool set for every subsequent user turn.
fn get_running_command_for_conversation(
    terminal_model: &TerminalModel,
    conversation_id: AIConversationId,
) -> Option<RunningCommand> {
    let active_block = terminal_model.block_list().active_block();
    if !active_block.is_active_and_long_running() {
        return None;
    }
    if active_block.is_agent_monitoring()
        && active_block
            .agent_interaction_metadata()
            .is_none_or(|metadata| metadata.conversation_id() != &conversation_id)
    {
        return None;
    }
    Some(running_command_snapshot(terminal_model))
}

fn running_command_belongs_to_monitor(
    terminal_model: &TerminalModel,
    conversation_id: AIConversationId,
    running_command: &RunningCommand,
) -> bool {
    let active_block = terminal_model.block_list().active_block();
    active_block.id() == &running_command.block_id
        && active_block.is_agent_monitoring()
        && active_block
            .agent_interaction_metadata()
            .is_some_and(|metadata| metadata.conversation_id() == &conversation_id)
}

fn running_command_snapshot(terminal_model: &TerminalModel) -> RunningCommand {
    let active_block = terminal_model.block_list().active_block();
    let is_alt_screen_active = terminal_model.is_alt_screen_active();
    RunningCommand {
        block_id: active_block.id().clone(),
        command: active_block.command_to_string(),
        grid_contents: if is_alt_screen_active {
            formatted_terminal_contents_for_input(
                terminal_model.alt_screen().grid_handler(),
                None,
                CURSOR_MARKER,
            )
        } else {
            formatted_terminal_contents_for_input(
                active_block.output_grid().grid_handler(),
                // TODO(vorporeal): This is probably too large.
                Some(1000),
                CURSOR_MARKER,
            )
        },
        cursor: CURSOR_MARKER.to_owned(),
        requested_command_id: active_block.requested_command_action_id().cloned(),
        is_alt_screen_active,
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
