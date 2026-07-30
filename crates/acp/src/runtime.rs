use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{io, thread};

use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodId, AuthenticateRequest, CancelNotification, ClientCapabilities,
    ContentBlock, Implementation, InitializeRequest, InitializeResponse, LoadSessionRequest,
    McpServer, Meta, NewSessionRequest, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionConfigOption,
    SessionConfigOptionValue, SessionId, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, StopReason, TextContent, ToolCallContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::{
    AcpAgent, Agent, ConnectionTo, JsonRpcRequest, JsonRpcResponse, Responder,
};
use async_channel::{Receiver, Sender};
use futures::channel::oneshot;

use futures::future::{self, Either, FutureExt as _};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{AcpLaunchConfig, AcpManagerConfig};
use crate::events::AcpEvent;
use crate::permissions::{
    outcome_for_decision, AcpPermissionPolicy, PermissionContext, PermissionDecision,
    SharedPermissionHandler,
};

static NEXT_TURN_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_PERMISSION_ID: AtomicU64 = AtomicU64::new(1);
const MAX_VISIBLE_TOOL_OUTPUT_BYTES: usize = 32 * 1024;
const TOOL_OUTPUT_TRUNCATION_MARKER: &str = "\n… [tool output truncated by Galaxy]";
const STEERING_TIMEOUT: Duration = Duration::from_secs(5);

/// Errors returned by the ACP runtime's public control surface.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AcpRuntimeError {
    /// The background runtime could not be started.
    #[error("failed to spawn ACP runtime worker: {0}")]
    WorkerSpawn(#[source] io::Error),
    /// The platform cannot yet guarantee termination of the agent's complete
    /// process tree.
    #[error("ACP is unavailable because this platform cannot safely terminate the full agent process tree")]
    ProcessTreeTeardownUnsupported,
    /// The manager's connection has closed.
    #[error("ACP runtime is closed: {0}")]
    RuntimeClosed(String),
    /// A turn request was invalid.
    #[error("invalid ACP turn request: {0}")]
    InvalidTurn(String),
    /// The requested turn is no longer active.
    #[error("the ACP turn is no longer active")]
    TurnNotActive,
    /// The connected agent does not support interactive steering.
    #[error("the ACP agent does not support _session/steering")]
    SteeringUnsupported,
    /// The agent did not acknowledge a live steering request in time.
    #[error("the ACP agent did not acknowledge live steering within {0:?}")]
    SteeringTimeout(Duration),
    /// The peer returned or caused a protocol error.
    #[error("ACP protocol error: {0}")]
    Protocol(String),
    /// Process startup or ACP initialization did not complete in time.
    #[error("ACP agent did not initialize within {0:?}")]
    InitializationTimeout(Duration),
    /// Agent-owned authentication did not complete in time.
    #[error("ACP agent did not authenticate within {0:?}")]
    AuthenticationTimeout(Duration),
}

/// Result of the Codex `_session/steering` extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpSteeringOutcome {
    /// Input was injected into the active turn.
    Injected,
    /// The adapter accepted the input as a new turn.
    StartedNewTurn,
    /// The adapter could not apply the input.
    Failed,
}

/// One prompt turn to run on an ACP session.
#[derive(Clone, Debug, PartialEq)]
pub struct AcpTurnRequest {
    /// Selected ACP session configuration values keyed by agent-provided option ID.
    pub config_values: std::collections::BTreeMap<String, SessionConfigOptionValue>,
    /// Stable Galaxy-side key used to serialize turns for one conversation.
    pub conversation_key: String,
    /// Existing agent session to load. `None` creates a new session.
    pub session_id: Option<SessionId>,
    /// Absolute working directory for the session.
    pub cwd: PathBuf,
    /// Additional absolute workspace roots.
    pub additional_directories: Vec<PathBuf>,
    /// User content sent to the agent.
    pub prompt: Vec<ContentBlock>,
    /// MCP servers made available for this session.
    pub mcp_servers: Vec<McpServer>,
    /// Whether Galaxy's autonomous execution mode explicitly permits automatic
    /// approval for this turn.
    pub auto_approve_permissions: bool,
    /// Category permissions inherited from Galaxy's active execution profile.
    pub permission_policy: AcpPermissionPolicy,
}

impl AcpTurnRequest {
    /// Creates a turn from structured ACP content.
    #[must_use]
    pub fn new(
        conversation_key: impl Into<String>,
        cwd: impl Into<PathBuf>,
        prompt: Vec<ContentBlock>,
    ) -> Self {
        Self {
            config_values: std::collections::BTreeMap::new(),
            conversation_key: conversation_key.into(),
            session_id: None,
            cwd: cwd.into(),
            additional_directories: Vec::new(),
            prompt,
            mcp_servers: Vec::new(),
            auto_approve_permissions: false,
            permission_policy: AcpPermissionPolicy::default(),
        }
    }

    /// Creates a plain-text turn.
    #[must_use]
    pub fn text(
        conversation_key: impl Into<String>,
        cwd: impl Into<PathBuf>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(
            conversation_key,
            cwd,
            vec![ContentBlock::Text(TextContent::new(text))],
        )
    }

    /// Loads an existing ACP session before running the turn.
    #[must_use]
    pub fn session_id(mut self, session_id: impl Into<SessionId>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Adds additional workspace roots.
    #[must_use]
    pub fn additional_directories(mut self, directories: Vec<PathBuf>) -> Self {
        self.additional_directories = directories;
        self
    }

    /// Adds MCP server configuration.
    #[must_use]
    pub fn mcp_servers(mut self, servers: Vec<McpServer>) -> Self {
        self.mcp_servers = servers;
        self
    }

    /// Enables or disables explicit autonomous approval for this turn.
    #[must_use]
    pub fn auto_approve_permissions(mut self, auto_approve: bool) -> Self {
        self.auto_approve_permissions = auto_approve;
        self
    }

    /// Applies Galaxy's category permissions to this turn.
    #[must_use]
    pub fn permission_policy(mut self, policy: AcpPermissionPolicy) -> Self {
        self.permission_policy = policy;
        self
    }

    fn validate(&self) -> Result<(), AcpRuntimeError> {
        if self.conversation_key.trim().is_empty() {
            return Err(AcpRuntimeError::InvalidTurn(
                "conversation_key cannot be empty".to_owned(),
            ));
        }
        if !self.cwd.is_absolute() {
            return Err(AcpRuntimeError::InvalidTurn(
                "cwd must be an absolute path".to_owned(),
            ));
        }
        if let Some(path) = self
            .additional_directories
            .iter()
            .find(|path| !path.is_absolute())
        {
            return Err(AcpRuntimeError::InvalidTurn(format!(
                "additional directory must be absolute: {}",
                path.display()
            )));
        }
        if self.prompt.is_empty() {
            return Err(AcpRuntimeError::InvalidTurn(
                "prompt must contain at least one content block".to_owned(),
            ));
        }
        for server in &self.mcp_servers {
            if let McpServer::Stdio(server) = server {
                if !server.command.is_absolute() {
                    return Err(AcpRuntimeError::InvalidTurn(format!(
                        "MCP stdio command must be absolute: {}",
                        server.command.display()
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Control handle for a queued or active turn.
#[derive(Clone)]
pub struct AcpSessionHandle {
    manager: AcpSessionManager,
    conversation_key: String,
    turn_id: u64,
}

impl std::fmt::Debug for AcpSessionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpSessionHandle")
            .field("conversation_key", &self.conversation_key)
            .field("turn_id", &self.turn_id)
            .finish_non_exhaustive()
    }
}

impl AcpSessionHandle {
    /// Stable Galaxy conversation key.
    #[must_use]
    pub fn conversation_key(&self) -> &str {
        &self.conversation_key
    }

    /// Cancels this queued or active turn.
    ///
    /// Success means the cancellation was accepted by the runtime. The
    /// per-turn event receiver reports the eventual `Finished` or teardown
    /// error.
    pub async fn cancel(&self) -> Result<(), AcpRuntimeError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.manager
            .send(Command::Cancel {
                conversation_key: self.conversation_key.clone(),
                turn_id: self.turn_id,
                ack: ack_tx,
            })
            .await?;
        ack_rx.await.map_err(|_| self.manager.closed_error())?
    }

    /// Steers the active turn through the optional Codex ACP extension.
    ///
    /// This never falls back to a concurrent `session/prompt`; callers receive
    /// [`AcpRuntimeError::SteeringUnsupported`] and can choose an explicit
    /// cancel-and-queue fallback.
    pub async fn steer(
        &self,
        prompt: Vec<ContentBlock>,
    ) -> Result<AcpSteeringOutcome, AcpRuntimeError> {
        if prompt.is_empty() {
            return Err(AcpRuntimeError::InvalidTurn(
                "steering prompt must contain at least one content block".to_owned(),
            ));
        }
        let (ack_tx, ack_rx) = oneshot::channel();
        self.manager
            .send(Command::Steer {
                conversation_key: self.conversation_key.clone(),
                turn_id: self.turn_id,
                prompt,
                ack: ack_tx,
            })
            .await?;
        ack_rx.await.map_err(|_| self.manager.closed_error())?
    }
}

/// Reusable manager for one ACP agent subprocess.
#[derive(Clone)]
pub struct AcpSessionManager {
    inner: Arc<ManagerInner>,
}

impl std::fmt::Debug for AcpSessionManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpSessionManager")
            .field("launch", &self.inner.launch)
            .field("alive", &self.inner.alive.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

struct ManagerInner {
    command_tx: Sender<Command>,
    agent_info: Mutex<Option<Implementation>>,
    agent_capabilities: Mutex<Option<agent_client_protocol::schema::v1::AgentCapabilities>>,
    launch: AcpLaunchConfig,
    alive: AtomicBool,
    terminal_error: Mutex<Option<String>>,
}

impl Drop for ManagerInner {
    fn drop(&mut self) {
        let _ = self.command_tx.try_send(Command::Shutdown);
    }
}

impl AcpSessionManager {
    /// Spawns the background ACP runtime and its configured agent subprocess.
    ///
    /// Process creation and ACP initialization happen on the worker so this
    /// method does not block the Galaxy UI thread.
    pub fn spawn(config: AcpManagerConfig) -> Result<Self, AcpRuntimeError> {
        validate_process_tree_teardown(cfg!(unix))?;
        let (command_tx, command_rx) = async_channel::unbounded();
        let inner = Arc::new(ManagerInner {
            command_tx: command_tx.clone(),
            agent_info: Mutex::new(None),
            agent_capabilities: Mutex::new(None),
            launch: config.launch.clone(),
            alive: AtomicBool::new(true),
            terminal_error: Mutex::new(None),
        });
        let worker_state = Arc::downgrade(&inner);

        let manager_for_worker = Arc::clone(&inner);
        thread::Builder::new()
            .name("galaxy-acp-runtime".to_owned())
            .spawn(move || {
                let result = futures::executor::block_on(run_connection_supervised(
                    config,
                    command_rx.clone(),
                    command_tx,
                    manager_for_worker,
                ));
                let terminal_error = result
                    .err()
                    .map_or_else(|| "runtime shut down".to_owned(), |error| error.to_string());
                if let Some(worker_state) = worker_state.upgrade() {
                    worker_state.alive.store(false, Ordering::Release);
                    if let Ok(mut error) = worker_state.terminal_error.lock() {
                        *error = Some(terminal_error.clone());
                    }
                }
                fail_queued_commands(&command_rx, &terminal_error);
            })
            .map_err(AcpRuntimeError::WorkerSpawn)?;

        Ok(Self { inner })
    }

    /// Comparable launch settings for manager restart decisions.
    #[must_use]
    pub fn launch_config(&self) -> &AcpLaunchConfig {
        &self.inner.launch
    }

    /// Returns the implementation metadata advertised during initialization.
    #[must_use]
    pub fn agent_info(&self) -> Option<Implementation> {
        self.inner
            .agent_info
            .lock()
            .ok()
            .and_then(|info| info.clone())
    }

    /// Returns the capabilities advertised during initialization.
    #[must_use]
    pub fn agent_capabilities(
        &self,
    ) -> Option<agent_client_protocol::schema::v1::AgentCapabilities> {
        self.inner
            .agent_capabilities
            .lock()
            .ok()
            .and_then(|capabilities| capabilities.clone())
    }

    /// Discovers the current configuration options by creating a temporary ACP session.
    pub async fn discover_config_options(
        &self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
    ) -> Result<Vec<SessionConfigOption>, AcpRuntimeError> {
        self.ensure_alive()?;
        let (result_tx, result_rx) = oneshot::channel();
        self.inner
            .command_tx
            .send(Command::Discover {
                cwd,
                mcp_servers,
                result: result_tx,
            })
            .await
            .map_err(|_| self.closed_error())?;
        result_rx.await.map_err(|_| self.closed_error())?
    }

    /// Whether the background worker and its ACP process are still available.
    ///
    /// This becomes `false` after protocol failure, normal shutdown, or a
    /// cancellation watchdog teardown. Callers can use it to recreate their
    /// runtime singleton before starting the next turn.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    /// Queues a turn and returns its control handle and isolated event stream.
    ///
    /// Turns sharing `conversation_key` are executed in FIFO order.
    pub fn run_turn(
        &self,
        request: AcpTurnRequest,
    ) -> Result<(AcpSessionHandle, Receiver<AcpEvent>), AcpRuntimeError> {
        request.validate()?;
        self.ensure_alive()?;

        let turn_id = NEXT_TURN_ID.fetch_add(1, Ordering::Relaxed);
        let conversation_key = request.conversation_key.clone();
        let (events_tx, events_rx) = async_channel::unbounded();
        self.inner
            .command_tx
            .try_send(Command::RunTurn(PendingTurn {
                turn_id,
                request,
                events: events_tx,
            }))
            .map_err(|_| self.closed_error())?;

        Ok((
            AcpSessionHandle {
                manager: self.clone(),
                conversation_key,
                turn_id,
            },
            events_rx,
        ))
    }

    async fn send(&self, command: Command) -> Result<(), AcpRuntimeError> {
        self.ensure_alive()?;
        self.inner
            .command_tx
            .send(command)
            .await
            .map_err(|_| self.closed_error())
    }

    fn ensure_alive(&self) -> Result<(), AcpRuntimeError> {
        if self.inner.alive.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(self.closed_error())
        }
    }

    fn closed_error(&self) -> AcpRuntimeError {
        let message = self
            .inner
            .terminal_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
            .unwrap_or_else(|| "worker is unavailable".to_owned());
        AcpRuntimeError::RuntimeClosed(message)
    }
}

fn validate_process_tree_teardown(supported: bool) -> Result<(), AcpRuntimeError> {
    if supported {
        return Ok(());
    }

    // Remove this gate once the pinned ACP SDK retains a Windows Job Object
    // that kills every launcher descendant when its connection is dropped.
    Err(AcpRuntimeError::ProcessTreeTeardownUnsupported)
}

#[derive(Clone)]
struct EventRoute {
    turn_id: u64,
    events: Sender<AcpEvent>,
    auto_approve: bool,
    permission_policy: AcpPermissionPolicy,
}

#[derive(Default)]
struct EventRouter {
    sessions: Mutex<HashMap<SessionId, EventRoute>>,
    pending_permissions: Mutex<HashMap<SessionId, HashMap<u64, Sender<()>>>>,
    replay_suppressed: Mutex<HashSet<SessionId>>,
}

impl EventRouter {
    fn set_route(&self, session_id: SessionId, route: EventRoute) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(session_id, route);
        }
    }

    fn remove_route(&self, session_id: &SessionId, turn_id: u64) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if sessions
                .get(session_id)
                .is_some_and(|route| route.turn_id == turn_id)
            {
                sessions.remove(session_id);
            }
        }
    }

    fn route_for(&self, session_id: &SessionId) -> Option<EventRoute> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(session_id).cloned())
    }

    fn emit(&self, session_id: &SessionId, event: AcpEvent) {
        if self.is_replay_suppressed(session_id) {
            return;
        }
        if let Some(route) = self.route_for(session_id) {
            let _ = route.events.try_send(event);
        }
    }

    fn suppress_replay(&self, session_id: SessionId) {
        if let Ok(mut suppressed) = self.replay_suppressed.lock() {
            suppressed.insert(session_id);
        }
    }

    fn finish_replay(&self, session_id: &SessionId) {
        if let Ok(mut suppressed) = self.replay_suppressed.lock() {
            suppressed.remove(session_id);
        }
    }

    fn is_replay_suppressed(&self, session_id: &SessionId) -> bool {
        self.replay_suppressed
            .lock()
            .is_ok_and(|suppressed| suppressed.contains(session_id))
    }

    fn register_permission(&self, session_id: SessionId, cancellation: Sender<()>) -> u64 {
        let permission_id = NEXT_PERMISSION_ID.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut pending) = self.pending_permissions.lock() {
            pending
                .entry(session_id)
                .or_default()
                .insert(permission_id, cancellation);
        }
        permission_id
    }

    fn unregister_permission(&self, session_id: &SessionId, permission_id: u64) {
        if let Ok(mut pending) = self.pending_permissions.lock() {
            if let Some(session) = pending.get_mut(session_id) {
                session.remove(&permission_id);
                if session.is_empty() {
                    pending.remove(session_id);
                }
            }
        }
    }

    fn cancel_permissions(&self, session_id: &SessionId) {
        let cancellations = self
            .pending_permissions
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(session_id))
            .unwrap_or_default();
        for cancellation in cancellations.into_values() {
            let _ = cancellation.try_send(());
        }
    }

    fn on_session_notification(&self, notification: SessionNotification) {
        let Some(event) = event_from_session_update(notification.update) else {
            return;
        };
        self.emit(&notification.session_id, event);
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SessionSpec {
    cwd: PathBuf,
    additional_directories: Vec<PathBuf>,
    mcp_servers: Vec<McpServer>,
    config_values: std::collections::BTreeMap<String, SessionConfigOptionValue>,
}

impl From<&AcpTurnRequest> for SessionSpec {
    fn from(request: &AcpTurnRequest) -> Self {
        Self {
            cwd: request.cwd.clone(),
            additional_directories: request.additional_directories.clone(),
            mcp_servers: request.mcp_servers.clone(),
            config_values: request.config_values.clone(),
        }
    }
}

struct PendingTurn {
    turn_id: u64,
    request: AcpTurnRequest,
    events: Sender<AcpEvent>,
}

struct ActiveTurn {
    pending: PendingTurn,
    phase: TurnPhase,
    cancel_requested: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TurnPhase {
    Opening,
    Prompting,
}

struct ConversationState {
    spec: SessionSpec,
    session_id: Option<SessionId>,
    ready: bool,
    active: Option<ActiveTurn>,
    queued: VecDeque<PendingTurn>,
}

impl ConversationState {
    fn new(turn: PendingTurn) -> Self {
        Self {
            spec: SessionSpec::from(&turn.request),
            session_id: None,
            ready: false,
            active: Some(ActiveTurn {
                pending: turn,
                phase: TurnPhase::Opening,
                cancel_requested: false,
            }),
            queued: VecDeque::new(),
        }
    }

    fn activate(&mut self, mut turn: PendingTurn) {
        debug_assert!(self.active.is_none());

        let next_spec = SessionSpec::from(&turn.request);
        if self.spec != next_spec {
            // ACP v1 cannot mutate a session's cwd or MCP configuration after
            // creation. Rotate to a fresh session instead of rejecting a
            // normal Galaxy context change. A persisted session ID is only a
            // restoration hint for the first turn; it must not reload the old
            // spec here.
            self.spec = next_spec;
            self.session_id = None;
            self.ready = false;
            turn.request.session_id = None;
        }

        self.active = Some(ActiveTurn {
            pending: turn,
            phase: if self.ready {
                TurnPhase::Prompting
            } else {
                TurnPhase::Opening
            },
            cancel_requested: false,
        });
    }

    fn activate_next(&mut self) -> bool {
        let Some(next) = self.queued.pop_front() else {
            return false;
        };
        self.activate(next);
        true
    }
}

enum Command {
    RunTurn(PendingTurn),
    Discover {
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
        result: oneshot::Sender<Result<Vec<SessionConfigOption>, AcpRuntimeError>>,
    },
    SessionOpened {
        conversation_key: String,
        turn_id: u64,
        requested_session_id: Option<SessionId>,
        result: Result<SessionId, agent_client_protocol::Error>,
    },
    PromptFinished {
        conversation_key: String,
        turn_id: u64,
        result: Result<PromptResponse, agent_client_protocol::Error>,
    },
    Cancel {
        conversation_key: String,
        turn_id: u64,
        ack: oneshot::Sender<Result<(), AcpRuntimeError>>,
    },
    Steer {
        conversation_key: String,
        turn_id: u64,
        prompt: Vec<ContentBlock>,
        ack: oneshot::Sender<Result<AcpSteeringOutcome, AcpRuntimeError>>,
    },
    AbortUntrackedSteering {
        conversation_key: String,
        turn_id: u64,
        result: Result<AcpSteeringOutcome, AcpRuntimeError>,
        ack: oneshot::Sender<Result<AcpSteeringOutcome, AcpRuntimeError>>,
    },
    ForceTeardown {
        conversation_key: String,
        turn_id: u64,
    },
    ConnectionClosed,
    Shutdown,
}

struct RuntimeActor {
    connection: ConnectionTo<Agent>,
    command_rx: Receiver<Command>,
    command_tx: Sender<Command>,
    router: Arc<EventRouter>,
    conversations: HashMap<String, ConversationState>,
    can_load: bool,
    can_steer: bool,
    agent_info: Option<Implementation>,
    agent_capabilities: agent_client_protocol::schema::v1::AgentCapabilities,
    cancellation_grace_period: std::time::Duration,
}

enum ActorControl {
    Continue,
    Stop,
}

impl RuntimeActor {
    async fn run(mut self) -> Result<(), AcpRuntimeError> {
        while let Ok(command) = self.command_rx.recv().await {
            let control = match self.handle_command(command) {
                Ok(control) => control,
                Err(error) => {
                    self.fail_all(&error.to_string());
                    return Err(error);
                }
            };
            if matches!(control, ActorControl::Stop) {
                break;
            }
        }
        Ok(())
    }

    fn handle_command(&mut self, command: Command) -> Result<ActorControl, AcpRuntimeError> {
        match command {
            Command::RunTurn(turn) => self.queue_turn(turn)?,
            Command::Discover {
                cwd,
                mcp_servers,
                result,
            } => {
                self.spawn_discovery(cwd, mcp_servers, result)?;
            }
            Command::SessionOpened {
                conversation_key,
                turn_id,
                requested_session_id,
                result,
            } => self.session_opened(
                &conversation_key,
                turn_id,
                requested_session_id.as_ref(),
                result,
            )?,
            Command::PromptFinished {
                conversation_key,
                turn_id,
                result,
            } => self.prompt_finished(&conversation_key, turn_id, result)?,
            Command::Cancel {
                conversation_key,
                turn_id,
                ack,
            } => {
                let result = self.cancel_turn(&conversation_key, turn_id);
                let should_stop = result
                    .as_ref()
                    .is_err_and(|error| matches!(error, AcpRuntimeError::Protocol(_)));
                let _ = ack.send(result);
                if should_stop {
                    return Ok(ActorControl::Stop);
                }
            }
            Command::Steer {
                conversation_key,
                turn_id,
                prompt,
                ack,
            } => self.steer_turn(&conversation_key, turn_id, prompt, ack)?,
            Command::AbortUntrackedSteering {
                conversation_key,
                turn_id,
                result,
                ack,
            } => {
                self.fail_all(&format!(
                    "ACP process was terminated immediately after steering for conversation {conversation_key:?}, turn {turn_id}, created activity Galaxy could not safely track"
                ));
                let _ = ack.send(result);
                return Ok(ActorControl::Stop);
            }
            Command::ForceTeardown {
                conversation_key,
                turn_id,
            } => {
                if self.force_teardown_if_still_active(&conversation_key, turn_id) {
                    return Ok(ActorControl::Stop);
                }
            }
            Command::ConnectionClosed => {
                self.fail_all("ACP agent closed its protocol stream");
                return Ok(ActorControl::Stop);
            }
            Command::Shutdown => return Ok(ActorControl::Stop),
        }
        Ok(ActorControl::Continue)
    }

    fn queue_turn(&mut self, turn: PendingTurn) -> Result<(), AcpRuntimeError> {
        let key = turn.request.conversation_key.clone();
        if let Some(state) = self.conversations.get_mut(&key) {
            if state.active.is_some() {
                state.queued.push_back(turn);
                return Ok(());
            }
            state.activate(turn);
        } else {
            self.conversations
                .insert(key.clone(), ConversationState::new(turn));
        }

        self.start_active_turn(&key)
    }

    fn spawn_discovery(
        &self,
        cwd: PathBuf,
        mcp_servers: Vec<McpServer>,
        result: oneshot::Sender<Result<Vec<SessionConfigOption>, AcpRuntimeError>>,
    ) -> Result<(), AcpRuntimeError> {
        let connection = self.connection.clone();
        self.connection
            .spawn(async move {
                let response = connection
                    .send_request(NewSessionRequest::new(cwd).mcp_servers(mcp_servers))
                    .block_task()
                    .await;
                let result_value = match response {
                    Ok(response) => Ok(response.config_options.unwrap_or_default()),
                    Err(error) => Err(AcpRuntimeError::Protocol(error.to_string())),
                };
                let _ = result.send(result_value);
                Ok(())
            })
            .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))?;
        Ok(())
    }

    fn start_active_turn(&mut self, conversation_key: &str) -> Result<(), AcpRuntimeError> {
        let (ready, session_id, turn_id, request, events) = {
            let state = self
                .conversations
                .get_mut(conversation_key)
                .ok_or(AcpRuntimeError::TurnNotActive)?;
            let active = state
                .active
                .as_mut()
                .ok_or(AcpRuntimeError::TurnNotActive)?;
            active.phase = if state.ready {
                TurnPhase::Prompting
            } else {
                TurnPhase::Opening
            };
            (
                state.ready,
                state.session_id.clone(),
                active.pending.turn_id,
                active.pending.request.clone(),
                active.pending.events.clone(),
            )
        };

        if ready {
            let session_id = session_id
                .ok_or_else(|| AcpRuntimeError::Protocol("ready session has no id".to_owned()))?;
            self.router.set_route(
                session_id.clone(),
                EventRoute {
                    turn_id,
                    events: events.clone(),
                    auto_approve: request.auto_approve_permissions,
                    permission_policy: request.permission_policy,
                },
            );
            emit_session_started(
                &events,
                session_id.clone(),
                self.can_load,
                self.can_steer,
                self.agent_info.clone(),
                self.agent_capabilities.clone(),
            );
            self.spawn_prompt(
                conversation_key.to_owned(),
                turn_id,
                session_id,
                request.prompt,
            )
        } else {
            self.spawn_open_session(conversation_key.to_owned(), turn_id, &request)
        }
    }

    fn spawn_open_session(
        &self,
        conversation_key: String,
        turn_id: u64,
        request: &AcpTurnRequest,
    ) -> Result<(), AcpRuntimeError> {
        let requested_session_id = request.session_id.clone();
        let requested_session_id =
            match restorable_session_id(request.session_id.as_ref(), self.can_load) {
                Ok(session_id) => session_id,
                Err(error) => {
                    self.command_tx
                        .try_send(Command::SessionOpened {
                            conversation_key,
                            turn_id,
                            requested_session_id,
                            result: Err(error),
                        })
                        .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))?;
                    return Ok(());
                }
            };

        if let Some(session_id) = requested_session_id.as_ref() {
            // `session/load` replays the agent's prior transcript through
            // `session/update`. Galaxy already restored that transcript from
            // its own persistence, so keep those notifications off the active
            // turn's event stream.
            self.router.suppress_replay(session_id.clone());
        }

        let connection = self.connection.clone();
        let completion_tx = self.command_tx.clone();
        let router = Arc::clone(&self.router);
        let cwd = request.cwd.clone();
        let additional_directories = request.additional_directories.clone();
        let mcp_servers = request.mcp_servers.clone();
        let config_values = request.config_values.clone();
        let suppressed_session_id = requested_session_id.clone();
        let replay_session_id = requested_session_id.clone();
        let spawn_result = self.connection.spawn(async move {
            let load_connection = connection.clone();
            let load_cwd = cwd.clone();
            let load_additional_directories = additional_directories.clone();
            let load_mcp_servers = mcp_servers.clone();
            let result = open_session(
                requested_session_id.clone(),
                move |session_id| async move {
                    load_connection
                        .send_request(
                            LoadSessionRequest::new(session_id, load_cwd)
                                .additional_directories(load_additional_directories)
                                .mcp_servers(load_mcp_servers),
                        )
                        .block_task()
                        .await
                        .map(|_| ())
                },
                move || async move {
                    let response = connection
                        .send_request(
                            NewSessionRequest::new(cwd)
                                .additional_directories(additional_directories)
                                .mcp_servers(mcp_servers),
                        )
                        .block_task()
                        .await?;
                    for (config_id, value) in config_values {
                        connection
                            .send_request(SetSessionConfigOptionRequest::new(
                                response.session_id.clone(),
                                config_id,
                                value,
                            ))
                            .block_task()
                            .await?;
                    }
                    Ok(response.session_id)
                },
            )
            .await;
            if let Some(session_id) = replay_session_id.as_ref() {
                router.finish_replay(session_id);
            }
            let _ = completion_tx
                .send(Command::SessionOpened {
                    conversation_key,
                    turn_id,
                    requested_session_id,
                    result,
                })
                .await;
            Ok(())
        });
        if let Err(error) = spawn_result {
            if let Some(session_id) = suppressed_session_id.as_ref() {
                self.router.finish_replay(session_id);
            }
            return Err(AcpRuntimeError::Protocol(error.to_string()));
        }
        Ok(())
    }

    fn session_opened(
        &mut self,
        conversation_key: &str,
        turn_id: u64,
        requested_session_id: Option<&SessionId>,
        result: Result<SessionId, agent_client_protocol::Error>,
    ) -> Result<(), AcpRuntimeError> {
        let is_current = self
            .conversations
            .get(conversation_key)
            .and_then(|state| state.active.as_ref())
            .is_some_and(|active| active.pending.turn_id == turn_id);
        if !is_current {
            return Ok(());
        }

        match result {
            Ok(session_id) => {
                if let Some(requested_session_id) = requested_session_id {
                    if requested_session_id != &session_id {
                        self.router.remove_route(requested_session_id, turn_id);
                    }
                }
                let (events, auto_approve, permission_policy, cancelled) = {
                    let state = self
                        .conversations
                        .get_mut(conversation_key)
                        .ok_or(AcpRuntimeError::TurnNotActive)?;
                    state.session_id = Some(session_id.clone());
                    state.ready = true;
                    let active = state
                        .active
                        .as_mut()
                        .ok_or(AcpRuntimeError::TurnNotActive)?;
                    active.phase = TurnPhase::Prompting;
                    (
                        active.pending.events.clone(),
                        active.pending.request.auto_approve_permissions,
                        active.pending.request.permission_policy,
                        active.cancel_requested,
                    )
                };
                self.router.set_route(
                    session_id.clone(),
                    EventRoute {
                        turn_id,
                        events: events.clone(),
                        auto_approve,
                        permission_policy,
                    },
                );
                emit_session_started(
                    &events,
                    session_id.clone(),
                    self.can_load,
                    self.can_steer,
                    self.agent_info.clone(),
                    self.agent_capabilities.clone(),
                );
                if cancelled {
                    let _ = events.try_send(AcpEvent::Finished {
                        stop_reason: StopReason::Cancelled,
                    });
                    self.finish_active_and_start_next(conversation_key)?;
                } else {
                    let prompt = self
                        .conversations
                        .get(conversation_key)
                        .and_then(|state| state.active.as_ref())
                        .map(|active| active.pending.request.prompt.clone())
                        .ok_or(AcpRuntimeError::TurnNotActive)?;
                    self.spawn_prompt(conversation_key.to_owned(), turn_id, session_id, prompt)?;
                }
            }
            Err(error) => {
                if let Some(session_id) = requested_session_id {
                    self.router.remove_route(session_id, turn_id);
                }
                self.fail_conversation(conversation_key, &error.to_string());
            }
        }
        Ok(())
    }

    fn spawn_prompt(
        &self,
        conversation_key: String,
        turn_id: u64,
        session_id: SessionId,
        prompt: Vec<ContentBlock>,
    ) -> Result<(), AcpRuntimeError> {
        let connection = self.connection.clone();
        let completion_tx = self.command_tx.clone();
        self.connection
            .spawn(async move {
                let result = connection
                    .send_request(PromptRequest::new(session_id, prompt))
                    .block_task()
                    .await;
                let _ = completion_tx
                    .send(Command::PromptFinished {
                        conversation_key,
                        turn_id,
                        result,
                    })
                    .await;
                Ok(())
            })
            .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))
    }

    fn prompt_finished(
        &mut self,
        conversation_key: &str,
        turn_id: u64,
        result: Result<PromptResponse, agent_client_protocol::Error>,
    ) -> Result<(), AcpRuntimeError> {
        let Some(state) = self.conversations.get(conversation_key) else {
            return Ok(());
        };
        let Some(active) = state.active.as_ref() else {
            return Ok(());
        };
        if active.pending.turn_id != turn_id {
            return Ok(());
        }

        match result {
            Ok(response) => {
                let _ = active.pending.events.try_send(AcpEvent::Finished {
                    stop_reason: response.stop_reason,
                });
            }
            Err(error) => emit_error(&active.pending.events, &error.to_string()),
        }
        self.finish_active_and_start_next(conversation_key)
    }

    fn finish_active_and_start_next(
        &mut self,
        conversation_key: &str,
    ) -> Result<(), AcpRuntimeError> {
        let (session_id, finished_turn, next) = {
            let state = self
                .conversations
                .get_mut(conversation_key)
                .ok_or(AcpRuntimeError::TurnNotActive)?;
            let session_id = state.session_id.clone();
            let finished_turn = state
                .active
                .take()
                .map(|active| active.pending.turn_id)
                .ok_or(AcpRuntimeError::TurnNotActive)?;
            let has_next = state.activate_next();
            (session_id, finished_turn, has_next)
        };

        if let Some(session_id) = session_id.as_ref() {
            self.router.remove_route(session_id, finished_turn);
        }
        if next {
            self.start_active_turn(conversation_key)?;
        }
        Ok(())
    }

    fn cancel_turn(&mut self, conversation_key: &str, turn_id: u64) -> Result<(), AcpRuntimeError> {
        let Some(state) = self.conversations.get_mut(conversation_key) else {
            return Ok(());
        };

        if state
            .active
            .as_ref()
            .is_some_and(|active| active.pending.turn_id == turn_id)
        {
            let Some(active) = state.active.as_mut() else {
                return Ok(());
            };
            if active.cancel_requested {
                return Ok(());
            }
            active.cancel_requested = true;
            let phase = active.phase;

            if phase == TurnPhase::Prompting {
                let session_id = state.session_id.clone().ok_or_else(|| {
                    AcpRuntimeError::Protocol("active turn has no session id".to_owned())
                })?;
                self.router.cancel_permissions(&session_id);
                self.connection
                    .send_notification(CancelNotification::new(session_id))
                    .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))?;
            }
            self.spawn_cancellation_watchdog(conversation_key.to_owned(), turn_id)?;
            return Ok(());
        }

        if let Some(index) = state.queued.iter().position(|turn| turn.turn_id == turn_id) {
            if let Some(turn) = state.queued.remove(index) {
                let _ = turn.events.try_send(AcpEvent::Finished {
                    stop_reason: StopReason::Cancelled,
                });
            }
        }
        Ok(())
    }

    fn spawn_cancellation_watchdog(
        &self,
        conversation_key: String,
        turn_id: u64,
    ) -> Result<(), AcpRuntimeError> {
        let completion_tx = self.command_tx.clone();
        let grace = self.cancellation_grace_period;
        self.connection
            .spawn(async move {
                async_io::Timer::after(grace).await;
                let _ = completion_tx
                    .send(Command::ForceTeardown {
                        conversation_key,
                        turn_id,
                    })
                    .await;
                Ok(())
            })
            .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))
    }

    fn steer_turn(
        &self,
        conversation_key: &str,
        turn_id: u64,
        prompt: Vec<ContentBlock>,
        ack: oneshot::Sender<Result<AcpSteeringOutcome, AcpRuntimeError>>,
    ) -> Result<(), AcpRuntimeError> {
        if !self.can_steer {
            let _ = ack.send(Err(AcpRuntimeError::SteeringUnsupported));
            return Ok(());
        }
        let Some(state) = self.conversations.get(conversation_key) else {
            let _ = ack.send(Err(AcpRuntimeError::TurnNotActive));
            return Ok(());
        };
        let is_active = state.active.as_ref().is_some_and(|active| {
            active.pending.turn_id == turn_id && active.phase == TurnPhase::Prompting
        });
        if !is_active {
            let _ = ack.send(Err(AcpRuntimeError::TurnNotActive));
            return Ok(());
        }
        let Some(session_id) = state.session_id.clone() else {
            let _ = ack.send(Err(AcpRuntimeError::TurnNotActive));
            return Ok(());
        };

        let connection = self.connection.clone();
        let command_tx = self.command_tx.clone();
        let conversation_key = conversation_key.to_owned();
        self.connection
            .spawn(async move {
                let steering = Box::pin(
                    connection
                        .send_request(SteeringRequest { session_id, prompt })
                        .block_task(),
                );
                let result = match future::select(
                    steering,
                    Box::pin(async_io::Timer::after(STEERING_TIMEOUT)),
                )
                .await
                {
                    Either::Left((response, _)) => match response {
                        Ok(response) => {
                            let outcome = response.outcome;
                            if steering_became_untracked(outcome) {
                                let command = Command::AbortUntrackedSteering {
                                    conversation_key,
                                    turn_id,
                                    result: Ok(outcome),
                                    ack,
                                };
                                if let Err(error) = command_tx.send(command).await {
                                    if let Command::AbortUntrackedSteering { ack, .. } =
                                        error.into_inner()
                                    {
                                        let _ = ack.send(Err(AcpRuntimeError::RuntimeClosed(
                                            "ACP runtime closed while containing an untracked steering turn"
                                                .to_owned(),
                                        )));
                                    }
                                }
                                return Ok(());
                            }
                            Ok(outcome)
                        }
                        Err(error) => Err(AcpRuntimeError::Protocol(error.to_string())),
                    },
                    Either::Right((_, _)) => {
                        // A timed-out extension request has an indeterminate
                        // outcome. The actor acknowledges the timeout and then
                        // immediately drops the connection and process group;
                        // a cancellation grace period would leave a ghost turn
                        // free to act without Galaxy tracking it.
                        let command = Command::AbortUntrackedSteering {
                            conversation_key,
                            turn_id,
                            result: Err(AcpRuntimeError::SteeringTimeout(STEERING_TIMEOUT)),
                            ack,
                        };
                        if let Err(error) = command_tx.send(command).await {
                            if let Command::AbortUntrackedSteering { ack, .. } = error.into_inner() {
                                let _ = ack.send(Err(AcpRuntimeError::RuntimeClosed(
                                    "ACP runtime closed while containing a timed-out steering request"
                                        .to_owned(),
                                )));
                            }
                        }
                        return Ok(());
                    }
                };
                let _ = ack.send(result);
                Ok(())
            })
            .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))
    }

    fn force_teardown_if_still_active(&mut self, conversation_key: &str, turn_id: u64) -> bool {
        let Some(state) = self.conversations.get(conversation_key) else {
            return false;
        };
        let Some(active) = state.active.as_ref() else {
            return false;
        };
        if active.pending.turn_id != turn_id || !active.cancel_requested {
            return false;
        }
        emit_error(
            &active.pending.events,
            "agent did not acknowledge cancellation; its ACP process was terminated",
        );
        let _ = active.pending.events.try_send(AcpEvent::Finished {
            stop_reason: StopReason::Cancelled,
        });
        self.fail_other_than(
            conversation_key,
            turn_id,
            "ACP process was terminated after a cancellation timeout",
        );
        true
    }

    fn fail_conversation(&mut self, conversation_key: &str, message: &str) {
        let Some(mut state) = self.conversations.remove(conversation_key) else {
            return;
        };
        if let Some(active) = state.active.take() {
            emit_error(&active.pending.events, message);
            if let Some(session_id) = state.session_id.as_ref() {
                self.router.remove_route(session_id, active.pending.turn_id);
            }
        }
        for turn in state.queued {
            emit_error(&turn.events, message);
        }
    }

    fn fail_other_than(&self, conversation_key: &str, turn_id: u64, message: &str) {
        for (key, state) in &self.conversations {
            if let Some(active) = state.active.as_ref() {
                if key != conversation_key || active.pending.turn_id != turn_id {
                    emit_error(&active.pending.events, message);
                }
            }
            for turn in &state.queued {
                emit_error(&turn.events, message);
            }
        }
    }

    fn fail_all(&self, message: &str) {
        fail_conversations(&self.conversations, message);
    }
}

fn fail_conversations(conversations: &HashMap<String, ConversationState>, message: &str) {
    for state in conversations.values() {
        if let Some(active) = state.active.as_ref() {
            emit_error(&active.pending.events, message);
        }
        for turn in &state.queued {
            emit_error(&turn.events, message);
        }
    }
}

async fn run_connection_supervised(
    config: AcpManagerConfig,
    command_rx: Receiver<Command>,
    command_tx: Sender<Command>,
    manager: Arc<ManagerInner>,
) -> Result<(), AcpRuntimeError> {
    let initialization_timeout = config.initialization_timeout;
    let authentication_timeout = config.authentication_timeout;
    let (initialized_tx, initialized_rx) = oneshot::channel();
    let (authenticated_tx, authenticated_rx) = oneshot::channel();
    supervise_connection_readiness(
        run_connection(
            config,
            command_rx,
            command_tx,
            initialized_tx,
            authenticated_tx,
            manager,
        ),
        initialized_rx,
        authenticated_rx,
        initialization_timeout,
        authentication_timeout,
    )
    .await
}

async fn supervise_connection_readiness<ConnectionFuture>(
    connection: ConnectionFuture,
    initialized: oneshot::Receiver<()>,
    authenticated: oneshot::Receiver<()>,
    initialization_timeout: Duration,
    authentication_timeout: Duration,
) -> Result<(), AcpRuntimeError>
where
    ConnectionFuture: Future<Output = Result<(), AcpRuntimeError>>,
{
    let connection = Box::pin(connection);
    let initialization = Box::pin(async move {
        match future::select(initialized, async_io::Timer::after(initialization_timeout)).await {
            Either::Left((Ok(()), _)) => Ok(()),
            Either::Left((Err(_), _)) => Err(AcpRuntimeError::Protocol(
                "ACP connection closed before initialization completed".to_owned(),
            )),
            Either::Right((_, _)) => Err(AcpRuntimeError::InitializationTimeout(
                initialization_timeout,
            )),
        }
    });

    let connection = match future::select(connection, initialization).await {
        Either::Left((result, _)) => return result,
        Either::Right((Ok(()), connection)) => connection,
        Either::Right((Err(error), _)) => return Err(error),
    };
    let authentication = Box::pin(async move {
        match future::select(
            authenticated,
            async_io::Timer::after(authentication_timeout),
        )
        .await
        {
            Either::Left((Ok(()), _)) => Ok(()),
            Either::Left((Err(_), _)) => Err(AcpRuntimeError::Protocol(
                "ACP connection closed before authentication completed".to_owned(),
            )),
            Either::Right((_, _)) => Err(AcpRuntimeError::AuthenticationTimeout(
                authentication_timeout,
            )),
        }
    });

    match future::select(connection, authentication).await {
        Either::Left((result, _)) => result,
        Either::Right((Ok(()), connection)) => connection.await,
        Either::Right((Err(error), _)) => Err(error),
    }
}

async fn run_connection(
    config: AcpManagerConfig,
    command_rx: Receiver<Command>,
    command_tx: Sender<Command>,
    initialized: oneshot::Sender<()>,
    authenticated: oneshot::Sender<()>,
    manager: Arc<ManagerInner>,
) -> Result<(), AcpRuntimeError> {
    let router = Arc::new(EventRouter::default());
    let permission_handler = Arc::clone(&config.permission_handler);
    let agent = AcpAgent::new(config.launch.to_agent_config());

    let notification_router = Arc::clone(&router);
    let permission_router = Arc::clone(&router);
    let close_tx = command_tx.clone();
    agent_client_protocol::Client
        .builder()
        .name("galaxy-acp")
        .on_receive_notification(
            async move |notification: SessionNotification, _connection| {
                notification_router.on_session_notification(notification);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_request(
            move |request: RequestPermissionRequest,
                  responder: Responder<RequestPermissionResponse>,
                  connection: ConnectionTo<Agent>| {
                handle_permission_request(
                    request,
                    responder,
                    connection,
                    Arc::clone(&permission_router),
                    Arc::clone(&permission_handler),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_close(async move |_connection| {
            let _ = close_tx.send(Command::ConnectionClosed).await;
            Ok(())
        })
        .connect_with(agent, async move |connection: ConnectionTo<Agent>| {
            let mut meta = serde_json::Map::new();
            meta.insert("terminal_output".to_owned(), serde_json::Value::Bool(true));
            let response = connection
                .send_request(
                    InitializeRequest::new(ProtocolVersion::V1)
                        .client_capabilities(ClientCapabilities::new().meta(meta))
                        .client_info(
                            Implementation::new(config.client_name, config.client_version)
                                .title("Galaxy"),
                        ),
                )
                .block_task()
                .await?;

            if response.protocol_version != ProtocolVersion::V1 {
                return Err(agent_client_protocol::Error::new(
                    -32003,
                    format!(
                        "agent negotiated unsupported ACP version {}",
                        response.protocol_version
                    ),
                ));
            }

            let _ = initialized.send(());
            if let Ok(mut agent_info) = manager.agent_info.lock() {
                *agent_info = response.agent_info.clone();
            }
            if let Ok(mut capabilities) = manager.agent_capabilities.lock() {
                *capabilities = Some(response.agent_capabilities.clone());
            }

            if let Some(request) = authentication_request(
                &response.auth_methods,
                config.launch.preferred_auth_method.as_ref(),
            )
            .map_err(|error| agent_client_protocol::Error::new(-32003, error.to_string()))?
            {
                connection.send_request(request).block_task().await?;
            }
            let _ = authenticated.send(());

            RuntimeActor {
                connection,
                command_rx,
                command_tx,
                router,
                conversations: HashMap::new(),
                can_load: response.agent_capabilities.load_session,
                can_steer: supports_steering(&response),
                agent_info: response.agent_info,
                agent_capabilities: response.agent_capabilities,
                cancellation_grace_period: config.cancellation_grace_period,
            }
            .run()
            .await
            .map_err(|error| agent_client_protocol::Error::new(-32003, error.to_string()))
        })
        .await
        .map_err(|error| AcpRuntimeError::Protocol(error.to_string()))
}

fn authentication_request(
    advertised_methods: &[AuthMethod],
    preferred_method: Option<&AuthMethodId>,
) -> Result<Option<AuthenticateRequest>, AcpRuntimeError> {
    let Some(first_method) = advertised_methods.first() else {
        return Ok(None);
    };
    let method = if let Some(preferred_method) = preferred_method {
        advertised_methods
            .iter()
            .find(|method| method.id() == preferred_method)
            .ok_or_else(|| {
                let advertised = advertised_methods
                    .iter()
                    .map(|method| method.id().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                AcpRuntimeError::Protocol(format!(
                    "preferred authentication method {preferred_method} was not advertised by the ACP agent; advertised methods: {advertised}"
                ))
            })?
    } else {
        first_method
    };
    Ok(Some(AuthenticateRequest::new(method.id().clone())))
}

async fn handle_permission_request(
    request: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
    connection: ConnectionTo<Agent>,
    router: Arc<EventRouter>,
    handler: SharedPermissionHandler,
) -> Result<(), agent_client_protocol::Error> {
    let session_id = request.session_id.clone();
    let route = router.route_for(&session_id);
    if let Some(route) = route.as_ref() {
        let _ = route.events.try_send(AcpEvent::PermissionRequested {
            request: request.clone(),
        });
    }
    let (auto_approve, policy) = route
        .as_ref()
        .map_or((false, AcpPermissionPolicy::default()), |route| {
            (route.auto_approve, route.permission_policy)
        });
    let (cancel_tx, cancel_rx) = async_channel::bounded(1);
    let permission_id = router.register_permission(session_id.clone(), cancel_tx);
    connection.spawn(async move {
        let decision = handler
            .decide(PermissionContext {
                request: request.clone(),
                auto_approve,
                policy,
            })
            .fuse();
        let cancelled = cancel_rx.recv().fuse();
        futures::pin_mut!(decision, cancelled);
        let decision = match future::select(decision, cancelled).await {
            Either::Left((decision, _)) => decision,
            Either::Right((_, _)) => PermissionDecision::Cancel,
        };
        router.unregister_permission(&session_id, permission_id);
        router.emit(
            &session_id,
            AcpEvent::PermissionResolved {
                session_id: session_id.clone(),
                decision: decision.clone(),
            },
        );
        let outcome: RequestPermissionOutcome = outcome_for_decision(&request, decision);
        responder.respond(RequestPermissionResponse::new(outcome))
    })
}

fn event_from_session_update(update: SessionUpdate) -> Option<AcpEvent> {
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => Some(event_from_content(chunk.content, false)),
        SessionUpdate::AgentThoughtChunk(chunk) => Some(event_from_content(chunk.content, true)),
        SessionUpdate::ToolCall(tool_call) => {
            let output = visible_tool_output(
                &tool_call.content,
                tool_call.raw_output.as_ref(),
                tool_call.meta.as_ref(),
            );
            Some(AcpEvent::ToolCall {
                id: tool_call.tool_call_id,
                title: tool_call.title,
                status: tool_call.status,
                output,
            })
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let output = visible_tool_output(
                update.fields.content.as_deref().unwrap_or_default(),
                update.fields.raw_output.as_ref(),
                update.meta.as_ref(),
            );
            Some(AcpEvent::ToolCallUpdate {
                id: update.tool_call_id,
                title: update.fields.title,
                status: update.fields.status,
                output,
            })
        }
        SessionUpdate::ConfigOptionUpdate(update) => Some(AcpEvent::ConfigOptions {
            options: update.config_options,
        }),
        SessionUpdate::UsageUpdate(usage) => Some(AcpEvent::Usage {
            used: usage.used,
            size: usage.size,
            cost: usage.cost,
        }),
        SessionUpdate::UserMessageChunk(chunk) => Some(AcpEvent::UserContent {
            content: chunk.content,
        }),
        SessionUpdate::Plan(_)
        | SessionUpdate::AvailableCommandsUpdate(_)
        | SessionUpdate::CurrentModeUpdate(_)
        | SessionUpdate::SessionInfoUpdate(_) => None,
        // `SessionUpdate` is non-exhaustive so newer stable protocol updates
        // remain forward-compatible and can be added to the visible surface.
        _ => None,
    }
}

fn visible_tool_output(
    content: &[ToolCallContent],
    raw_output: Option<&serde_json::Value>,
    meta: Option<&Meta>,
) -> Option<String> {
    let mut output = VisibleToolOutput::default();

    if append_streaming_output_meta(&mut output, meta) {
        append_terminal_exit_meta(&mut output, meta);
        return output.finish();
    }

    if append_terminal_exit_meta(&mut output, meta) {
        return output.finish();
    }

    if append_tool_content(&mut output, content) {
        return output.finish();
    }

    append_raw_tool_output(&mut output, raw_output);
    if output.is_empty() {
        append_terminal_info_meta(&mut output, meta);
    }
    output.finish()
}

fn append_streaming_output_meta(output: &mut VisibleToolOutput, meta: Option<&Meta>) -> bool {
    let Some(meta) = meta else {
        return false;
    };
    for key in [
        "terminal_output",
        "terminal_output_delta",
        "mcp_output_delta",
    ] {
        let Some(value) = meta.get(key) else {
            continue;
        };
        let Some(data) = value.get("data").and_then(serde_json::Value::as_str) else {
            continue;
        };
        output.append(data);
        if value.get("truncated").and_then(serde_json::Value::as_bool) == Some(true) {
            output.append_line("[output truncated by ACP agent]");
        }
        return !output.is_empty();
    }
    false
}

fn append_terminal_exit_meta(output: &mut VisibleToolOutput, meta: Option<&Meta>) -> bool {
    let Some(exit) = meta.and_then(|meta| meta.get("terminal_exit")) else {
        return false;
    };
    let Some(exit) = exit.as_object() else {
        return false;
    };

    let exit_code = exit
        .get("exit_code")
        .or_else(|| exit.get("exitCode"))
        .and_then(json_scalar_text);
    let signal = exit.get("signal").and_then(json_scalar_text);
    let status = match (exit_code, signal) {
        (Some(exit_code), Some(signal)) => {
            format!("[terminal exited: code {exit_code}, signal {signal}]")
        }
        (Some(exit_code), None) => format!("[terminal exited: code {exit_code}]"),
        (None, Some(signal)) => format!("[terminal exited: signal {signal}]"),
        (None, None) => "[terminal exited]".to_owned(),
    };
    output.append(&status);
    true
}

fn append_terminal_info_meta(output: &mut VisibleToolOutput, meta: Option<&Meta>) -> bool {
    let Some(info) = meta
        .and_then(|meta| meta.get("terminal_info"))
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    let terminal_id = info
        .get("terminal_id")
        .or_else(|| info.get("terminalId"))
        .and_then(serde_json::Value::as_str);
    let cwd = info.get("cwd").and_then(serde_json::Value::as_str);
    let label = match (terminal_id, cwd) {
        (Some(terminal_id), Some(cwd)) => format!("[terminal: {terminal_id}; cwd: {cwd}]"),
        (Some(terminal_id), None) => format!("[terminal: {terminal_id}]"),
        (None, Some(cwd)) => format!("[terminal cwd: {cwd}]"),
        (None, None) => return false,
    };
    output.append(&label);
    true
}

fn append_tool_content(output: &mut VisibleToolOutput, content: &[ToolCallContent]) -> bool {
    for item in content {
        match item {
            ToolCallContent::Content(content) => match &content.content {
                ContentBlock::Text(text) => output.append_line(&text.text),
                ContentBlock::Image(image) => {
                    output.append_line(&format!("[image: {}]", image.mime_type));
                }
                ContentBlock::Audio(audio) => {
                    output.append_line(&format!("[audio: {}]", audio.mime_type));
                }
                ContentBlock::ResourceLink(resource) => {
                    output.append_line(&format!("[resource: {} ({})]", resource.name, resource.uri));
                }
                ContentBlock::Resource(resource) => match &resource.resource {
                    agent_client_protocol::schema::v1::EmbeddedResourceResource::TextResourceContents(
                        text,
                    ) => output.append_line(&text.text),
                    agent_client_protocol::schema::v1::EmbeddedResourceResource::BlobResourceContents(
                        blob,
                    ) => output.append_line(&format!("[binary resource: {}]", blob.uri)),
                    // ACP content is non-exhaustive. Unknown future content remains
                    // hidden until Galaxy has a safe textual representation for it.
                    _ => {}
                },
                // ACP content is non-exhaustive. Unknown future content remains
                // hidden until Galaxy has a safe textual representation for it.
                _ => {}
            },
            ToolCallContent::Diff(diff) => {
                output.append_line(&format!("[diff: {}]", diff.path.display()));
            }
            ToolCallContent::Terminal(terminal) => {
                output.append_line(&format!("[terminal: {}]", terminal.terminal_id));
            }
            // ACP tool content is non-exhaustive. Unknown future variants remain
            // hidden until Galaxy has a safe textual representation for them.
            _ => {}
        }
    }
    !output.is_empty()
}

fn append_raw_tool_output(output: &mut VisibleToolOutput, raw_output: Option<&serde_json::Value>) {
    let Some(raw_output) = raw_output else {
        return;
    };
    if let Some(text) = raw_output.as_str() {
        output.append(text);
        return;
    }

    if let Some(object) = raw_output.as_object() {
        let rendered = [
            "formatted_output",
            "formattedOutput",
            "output",
            "error",
            "message",
        ]
        .into_iter()
        .find_map(|key| object.get(key).and_then(serde_json::Value::as_str));
        if let Some(rendered) = rendered {
            output.append(rendered);
            if raw_output_was_truncated(object) {
                output.append_line("[output truncated by ACP agent]");
            }
            append_raw_exit_status(output, object);
            return;
        }
    }

    let (json, truncated) = bounded_pretty_json(raw_output);
    output.append(&json);
    if truncated {
        output.mark_truncated();
    }
}

fn raw_output_was_truncated(object: &serde_json::Map<String, serde_json::Value>) -> bool {
    object
        .get("truncated")
        .and_then(serde_json::Value::as_bool)
        .or_else(|| {
            object
                .get("metadata")
                .and_then(serde_json::Value::as_object)
                .and_then(|metadata| metadata.get("truncated"))
                .and_then(serde_json::Value::as_bool)
        })
        == Some(true)
}

fn append_raw_exit_status(
    output: &mut VisibleToolOutput,
    object: &serde_json::Map<String, serde_json::Value>,
) {
    let nested = object
        .get("exit_status")
        .or_else(|| object.get("exitStatus"))
        .and_then(serde_json::Value::as_object);
    let exit_code = object
        .get("exit_code")
        .or_else(|| object.get("exitCode"))
        .and_then(json_scalar_text)
        .or_else(|| {
            nested.and_then(|status| {
                status
                    .get("exit_code")
                    .or_else(|| status.get("exitCode"))
                    .and_then(json_scalar_text)
            })
        });
    let signal = object
        .get("signal")
        .and_then(json_scalar_text)
        .or_else(|| nested.and_then(|status| status.get("signal").and_then(json_scalar_text)));
    let status = match (exit_code, signal) {
        (Some(exit_code), Some(signal)) => {
            Some(format!("[exit code: {exit_code}; signal: {signal}]"))
        }
        (Some(exit_code), None) => Some(format!("[exit code: {exit_code}]")),
        (None, Some(signal)) => Some(format!("[signal: {signal}]")),
        (None, None) => None,
    };
    if let Some(status) = status {
        output.append_line(&status);
    }
}

fn json_scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

fn bounded_pretty_json(value: &serde_json::Value) -> (String, bool) {
    let mut writer = BoundedJsonWriter::default();
    if serde_json::to_writer_pretty(&mut writer, value).is_err() {
        return (String::new(), false);
    }
    let valid_len = match std::str::from_utf8(&writer.bytes) {
        Ok(_) => writer.bytes.len(),
        Err(error) => error.valid_up_to(),
    };
    writer.bytes.truncate(valid_len);
    (
        String::from_utf8(writer.bytes).unwrap_or_default(),
        writer.truncated,
    )
}

#[derive(Default)]
struct BoundedJsonWriter {
    bytes: Vec<u8>,
    truncated: bool,
}

impl io::Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = MAX_VISIBLE_TOOL_OUTPUT_BYTES.saturating_sub(self.bytes.len());
        let copied = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..copied]);
        self.truncated |= copied < bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct VisibleToolOutput {
    text: String,
    truncated: bool,
}

impl VisibleToolOutput {
    fn append_line(&mut self, value: &str) {
        if self.truncated || value.is_empty() {
            return;
        }
        if !self.text.is_empty() && !self.text.ends_with('\n') {
            self.append("\n");
        }
        self.append(value);
    }

    fn append(&mut self, value: &str) {
        if self.truncated || value.is_empty() {
            return;
        }
        let payload_limit =
            MAX_VISIBLE_TOOL_OUTPUT_BYTES.saturating_sub(TOOL_OUTPUT_TRUNCATION_MARKER.len());
        let mut chars = value.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '\u{1b}' {
                skip_escape_sequence(&mut chars);
                continue;
            }
            if character == '\u{009b}' {
                skip_control_sequence(&mut chars);
                continue;
            }
            if character == '\u{009d}' {
                skip_operating_system_command(&mut chars);
                continue;
            }
            let character = if character == '\r' {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                '\n'
            } else {
                character
            };
            if !is_safe_display_character(character) {
                continue;
            }
            if self.text.len() + character.len_utf8() > payload_limit {
                self.truncated = true;
                break;
            }
            self.text.push(character);
        }
    }

    fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn finish(mut self) -> Option<String> {
        if self.truncated {
            self.text.push_str(TOOL_OUTPUT_TRUNCATION_MARKER);
        }
        (!self.text.is_empty()).then_some(self.text)
    }
}

fn skip_escape_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek() {
        Some('[') => {
            chars.next();
            skip_control_sequence(chars);
        }
        Some(']') => {
            chars.next();
            skip_operating_system_command(chars);
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn skip_control_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for character in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&character) {
            break;
        }
    }
}

fn skip_operating_system_command(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(character) = chars.next() {
        if character == '\u{07}' {
            break;
        }
        if character == '\u{1b}' && chars.peek() == Some(&'\\') {
            chars.next();
            break;
        }
    }
}

fn is_safe_display_character(character: char) -> bool {
    if matches!(character, '\n' | '\t') {
        return true;
    }
    if character.is_control() {
        return false;
    }
    !matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn event_from_content(content: ContentBlock, thought: bool) -> AcpEvent {
    match content {
        ContentBlock::Text(text) if thought => AcpEvent::AgentThought { text: text.text },
        ContentBlock::Text(text) => AcpEvent::AgentText { text: text.text },
        content => AcpEvent::AgentContent { content, thought },
    }
}

fn supports_steering(response: &InitializeResponse) -> bool {
    response
        .meta
        .as_ref()
        .and_then(|meta| meta.get("steering"))
        .and_then(serde_json::Value::as_object)
        .and_then(|steering| steering.get("supported"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn steering_became_untracked(outcome: AcpSteeringOutcome) -> bool {
    matches!(outcome, AcpSteeringOutcome::StartedNewTurn)
}

fn restorable_session_id(
    requested_session_id: Option<&SessionId>,
    can_load: bool,
) -> Result<Option<SessionId>, agent_client_protocol::Error> {
    match (requested_session_id, can_load) {
        (Some(session_id), true) => Ok(Some(session_id.clone())),
        (Some(_), false) => Err(agent_client_protocol::Error::new(
            -32004,
            "the ACP agent cannot restore this persisted conversation because it does not advertise session/load; start a new ACP conversation",
        )),
        (None, true | false) => Ok(None),
    }
}

async fn open_session<Load, LoadFuture, Create, CreateFuture>(
    requested_session_id: Option<SessionId>,
    load: Load,
    create: Create,
) -> Result<SessionId, agent_client_protocol::Error>
where
    Load: FnOnce(SessionId) -> LoadFuture,
    LoadFuture: Future<Output = Result<(), agent_client_protocol::Error>>,
    Create: FnOnce() -> CreateFuture,
    CreateFuture: Future<Output = Result<SessionId, agent_client_protocol::Error>>,
{
    if let Some(session_id) = requested_session_id {
        load(session_id.clone()).await?;
        return Ok(session_id);
    }
    create().await
}

fn emit_session_started(
    events: &Sender<AcpEvent>,
    session_id: SessionId,
    can_load: bool,
    can_steer: bool,
    agent_info: Option<Implementation>,
    capabilities: agent_client_protocol::schema::v1::AgentCapabilities,
) {
    let _ = events.try_send(AcpEvent::SessionStarted {
        session_id,
        agent_info,
        capabilities,
        can_load,
        can_steer,
    });
}

fn emit_error(events: &Sender<AcpEvent>, message: &str) {
    let _ = events.try_send(AcpEvent::Error {
        message: message.to_owned(),
    });
}

fn fail_queued_commands(command_rx: &Receiver<Command>, message: &str) {
    while let Ok(command) = command_rx.try_recv() {
        match command {
            Command::RunTurn(turn) => emit_error(&turn.events, message),
            Command::Cancel { ack, .. } => {
                let _ = ack.send(Err(AcpRuntimeError::RuntimeClosed(message.to_owned())));
            }
            Command::Steer { ack, .. } => {
                let _ = ack.send(Err(AcpRuntimeError::RuntimeClosed(message.to_owned())));
            }
            Command::AbortUntrackedSteering { ack, .. } => {
                let _ = ack.send(Err(AcpRuntimeError::RuntimeClosed(message.to_owned())));
            }
            Command::SessionOpened { .. }
            | Command::PromptFinished { .. }
            | Command::ForceTeardown { .. }
            | Command::ConnectionClosed
            | Command::Shutdown
            | Command::Discover { .. } => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcRequest)]
#[request(method = "_session/steering", response = SteeringResponse)]
#[serde(rename_all = "camelCase")]
struct SteeringRequest {
    session_id: SessionId,
    prompt: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonRpcResponse)]
#[serde(rename_all = "camelCase")]
struct SteeringResponse {
    outcome: AcpSteeringOutcome,
}

impl Serialize for AcpSteeringOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = match self {
            Self::Injected => "injected",
            Self::StartedNewTurn => "startedNewTurn",
            Self::Failed => "failed",
        };
        serializer.serialize_str(value)
    }
}

impl<'de> Deserialize<'de> for AcpSteeringOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match String::deserialize(deserializer)?.as_str() {
            "injected" => Ok(Self::Injected),
            "startedNewTurn" => Ok(Self::StartedNewTurn),
            "failed" => Ok(Self::Failed),
            value => Err(serde::de::Error::unknown_variant(
                value,
                &["injected", "startedNewTurn", "failed"],
            )),
        }
    }
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
