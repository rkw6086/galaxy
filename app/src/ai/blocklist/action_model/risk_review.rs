//! Local, deterministic policy and exact-action human approval handling.

#[path = "local_permission_policy.rs"]
mod local_policy;

use std::time::Duration;

use galaxy_agent_core::{turn_control, TurnCommand, TurnCommandSender};
use galaxyui::r#async::Timer;
use instant::Instant;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::*;
use crate::ai::blocklist::permissions::{
    is_agent_mode_autonomy_allowed, CommandExecutionPermission,
    CommandExecutionPermissionDeniedReason, FileWritePermission, FileWritePermissionDeniedReason,
};
use crate::ai::blocklist::BlocklistAIPermissions;
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::execution_profiles::{ActionPermission, PermissionMode};
#[cfg(not(target_family = "wasm"))]
use crate::ai::mcp::TemplatableMCPServerManager;

const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
const REVIEW_TIMEOUT: Duration = Duration::from_secs(30);
const APPROVAL_EXPIRED_MESSAGE: &str = "Ask First approval expired after 60 seconds. The action was not executed. Choose a different approach or wait for user instructions.";
const MAX_REVIEW_INPUT_BYTES: usize = 64 * 1024;
#[derive(Debug)]
struct Assessment {
    risk: u8,
    reason: String,
}

impl Assessment {
    fn allows_automatic_execution(&self, policy_requires_human: bool) -> bool {
        self.risk <= 3 && !policy_requires_human
    }

    fn allowed(risk: u8, reason: &str) -> Self {
        Self {
            risk,
            reason: reason.to_owned(),
        }
    }

    fn denied(reason: &str) -> Self {
        Self {
            risk: 4,
            reason: reason.to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ReviewEnvironment {
    working_directory: Option<String>,
    session_type: String,
    hostname: Option<String>,
    shell_type: String,
    native_file_paths: bool,
    profile_id: String,
    permission_mode: PermissionMode,
}

pub(super) struct ActionReview {
    token: uuid::Uuid,
    action: AIAgentAction,
    execution_ref: Option<ProviderToolExecutionRef>,
    decision: ReviewDecision,
    environment: ReviewEnvironment,
    authorization_fingerprint: Option<String>,
    cancel: TurnCommandSender,
}

impl Drop for ActionReview {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(TurnCommand::Cancel);
    }
}

enum ReviewDecision {
    Assessing,
    Allow,
    Ask { reason: String, deadline: Instant },
}

#[derive(Clone, Copy)]
pub(crate) enum ReviewDisplayState {
    Assessing,
    AwaitingApproval,
    Approved,
    Denied,
}

pub(crate) struct ReviewDisplay {
    pub(crate) state: ReviewDisplayState,
    pub(crate) summary: String,
    pub(crate) detail: String,
}

pub(super) struct ReviewOutcome {
    summary: String,
    detail: String,
    approved: bool,
}

impl ReviewOutcome {
    pub(super) fn denied_by_user(action: &AIAgentAction) -> Self {
        Self {
            summary: review_summary(action),
            detail: "Denied by you.".to_string(),
            approved: false,
        }
    }
}

fn review_summary(action: &AIAgentAction) -> String {
    let description = match &action.action {
        AIAgentActionType::RequestCommandOutput { command, .. } => {
            format!("Run: {command}")
        }
        AIAgentActionType::WriteToLongRunningShellCommand { input, mode, .. }
            if mode.is_shell_interrupt(input) =>
        {
            "Interrupt the running command (Ctrl+C)".to_string()
        }
        AIAgentActionType::WriteToLongRunningShellCommand { input, .. } => {
            format!(
                "Send to running command: {}",
                String::from_utf8_lossy(input)
            )
        }
        _ => action.action.user_friendly_name(),
    };
    let summary = description.split_whitespace().collect::<Vec<_>>().join(" ");
    shorten_for_review(&summary, 160)
}

fn shorten_for_review(text: &str, limit: usize) -> String {
    let mut characters = text.chars();
    let shortened = characters.by_ref().take(limit).collect::<String>();
    if characters.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

/// Explicit allowlist: arbitrary shell text and MCP calls never qualify as read-only.
pub(crate) fn is_read_only_inspection(action: &AIAgentActionType) -> bool {
    matches!(
        action,
        AIAgentActionType::ReadFiles(_)
            | AIAgentActionType::SearchCodebase(_)
            | AIAgentActionType::Grep { .. }
            | AIAgentActionType::FileGlob { .. }
            | AIAgentActionType::FileGlobV2 { .. }
            | AIAgentActionType::ReadSkill(_)
            | AIAgentActionType::ReadDocuments(_)
            | AIAgentActionType::ReadShellCommandOutput { .. }
    )
}

fn is_observation_or_coordination(action: &AIAgentActionType) -> bool {
    is_read_only_inspection(action)
        || matches!(
            action,
            AIAgentActionType::ReadShellCommandOutput { .. }
                | AIAgentActionType::WaitForEvents { .. }
                | AIAgentActionType::AskUserQuestion { .. }
                | AIAgentActionType::SuggestNewConversation { .. }
                | AIAgentActionType::SuggestPrompt(_)
                | AIAgentActionType::OpenCodeReview
        )
}

/// Select request-bearing input variants, never infer provenance from text.
/// Every nonempty recorded request remains available as deny-only evidence;
/// neither a role-looking prefix nor automated wording can grant authority.
fn review_user_requests<'a>(inputs: impl Iterator<Item = &'a AIAgentInput>) -> Vec<String> {
    inputs
        .filter_map(|input| match input {
            AIAgentInput::UserQuery { query, .. }
            | AIAgentInput::CreateNewProject { query, .. } => Some(query.as_str()),
            AIAgentInput::InvokeSkill {
                user_query: Some(query),
                ..
            } => Some(query.query.as_str()),
            _ => None,
        })
        .filter(|query| !query.trim().is_empty())
        // Keep complete instructions, including intermediate revocations. The
        // existing review byte limit rejects oversized context rather than
        // silently dropping a restriction and reviving older authorization.
        .map(str::to_owned)
        .collect()
}

/// Deliberately narrow shell grammar: no expansion, quoting, chaining,
/// redirects, scripts, environment overrides, or unrecognized flags.
fn is_safe_inspection_command(command: &str) -> bool {
    if command.is_empty()
        || command.len() > 1024
        || command
            .chars()
            .any(|c| !c.is_ascii_alphanumeric() && !matches!(c, ' ' | '-' | '_' | '.' | '/' | '='))
    {
        return false;
    }
    let words = command.split_ascii_whitespace().collect::<Vec<_>>();
    match words.as_slice() {
        ["pwd"] | ["pwd", "-L" | "-P"] | ["git", "--version"] => true,
        ["ls", args @ ..] => args.iter().all(|arg| {
            if let Some(flags) = arg.strip_prefix('-') {
                *arg == "--"
                    || (arg.starts_with('-')
                        && !arg.starts_with("--")
                        && arg.len() > 1
                        && flags.chars().all(|c| "alhdtr1AF".contains(c)))
            } else {
                !arg.contains('=')
            }
        }),
        // Disable Git's configurable executable callbacks before fast approval.
        ["git", "--no-pager", "-c", "core.fsmonitor=false", "status", args @ ..] => {
            args.iter().all(|arg| {
                matches!(
                    *arg,
                    "--short" | "--branch" | "--porcelain" | "--porcelain=v1"
                )
            })
        }
        ["git", "--no-pager", "-c", "core.fsmonitor=false", "diff", "--no-ext-diff", "--no-textconv", args @ ..] => {
            !args.is_empty()
                && args.iter().all(|arg| {
                    matches!(
                        *arg,
                        "--stat"
                            | "--name-only"
                            | "--name-status"
                            | "--check"
                            | "--cached"
                            | "--staged"
                    )
                })
        }
        _ => false,
    }
}

impl BlocklistAIActionModel {
    fn review_environment(&self, ctx: &AppContext) -> ReviewEnvironment {
        let session = self.active_session.as_ref(ctx);
        ReviewEnvironment {
            working_directory: session.current_working_directory().cloned(),
            session_type: format!("{:?}", session.session_type(ctx)),
            hostname: session
                .session(ctx)
                .map(|session| session.hostname().to_owned()),
            shell_type: format!("{:?}", session.shell_type(ctx)),
            native_file_paths: !matches!(
                session.shell_launch_data(ctx),
                Some(
                    galaxy_terminal::shell::ShellLaunchData::WSL { .. }
                        | galaxy_terminal::shell::ShellLaunchData::MSYS2 { .. }
                )
            ),
            profile_id: AIExecutionProfilesModel::as_ref(ctx)
                .active_profile(Some(self.terminal_view_id), ctx)
                .id()
                .to_string(),
            permission_mode: BlocklistAIPermissions::as_ref(ctx)
                .get_permission_mode(ctx, Some(self.terminal_view_id)),
        }
    }

    fn review_authorization(
        &self,
        conversation_id: AIConversationId,
        ctx: &AppContext,
    ) -> anyhow::Result<(Vec<String>, String)> {
        let history = BlocklistAIHistoryModel::as_ref(ctx);
        let mut conversation = history
            .conversation(&conversation_id)
            .ok_or_else(|| anyhow::anyhow!("The owning conversation is unavailable."))?;
        let mut visited = HashSet::new();
        while let Some(parent_id) = conversation.parent_conversation_id() {
            anyhow::ensure!(
                visited.insert(parent_id) && visited.len() <= 8,
                "The root user authorization chain is invalid."
            );
            conversation = history
                .conversation(&parent_id)
                .ok_or_else(|| anyhow::anyhow!("The root user authorization is unavailable."))?;
        }
        let requests = review_user_requests(
            conversation
                .root_task_exchanges()
                .flat_map(|exchange| exchange.input.iter()),
        );
        let bytes = requests
            .iter()
            .fold(0usize, |size, request| size.saturating_add(request.len()));
        anyhow::ensure!(bytes <= MAX_REVIEW_INPUT_BYTES,
            "The complete recorded user requests exceed the local policy bound; no restrictions were discarded.");
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(&requests)?));
        Ok((requests, fingerprint))
    }

    fn uses_galaxy_decides(&self, action: &AIAgentAction, ctx: &AppContext) -> bool {
        BlocklistAIPermissions::as_ref(ctx).get_permission_mode(ctx, Some(self.terminal_view_id))
            == PermissionMode::GalaxyDecides
            && !is_observation_or_coordination(&action.action)
    }

    fn approved_command_fingerprint(
        &self,
        action: &AIAgentAction,
        ctx: &AppContext,
    ) -> Option<String> {
        let AIAgentActionType::RequestCommandOutput { command, .. } = &action.action else {
            return None;
        };
        let environment = self.review_environment(ctx);
        environment.working_directory.as_ref()?;
        environment.hostname.as_ref()?;
        self.active_session.as_ref(ctx).shell_type(ctx)?;
        let bytes = serde_json::to_vec(&(
            command,
            &environment.working_directory,
            &environment.session_type,
            &environment.hostname,
            &environment.shell_type,
        ))
        .ok()?;
        Some(format!("{:x}", Sha256::digest(bytes)))
    }

    fn remember_user_approval(&self, action: &AIAgentAction, ctx: &mut ModelContext<Self>) {
        if let Some(fingerprint) = self.approved_command_fingerprint(action, ctx) {
            AIExecutionProfilesModel::handle(ctx).update(ctx, |profiles, ctx| {
                let id = *profiles
                    .active_profile(Some(self.terminal_view_id), ctx)
                    .id();
                profiles.remember_approved_command(id, fingerprint, ctx);
            });
        }
    }

    pub(super) fn is_explicitly_allowlisted_command(
        &self,
        action: &AIAgentAction,
        ctx: &AppContext,
    ) -> bool {
        let AIAgentActionType::RequestCommandOutput { command, .. } = &action.action else {
            return false;
        };
        let profile =
            AIExecutionProfilesModel::as_ref(ctx).active_profile(Some(self.terminal_view_id), ctx);
        profile
            .data()
            .command_allowlist
            .iter()
            .any(|predicate| predicate.matches(command))
            && !BlocklistAIPermissions::as_ref(ctx)
                .get_execute_commands_denylist(ctx, Some(self.terminal_view_id))
                .iter()
                .any(|predicate| predicate.matches(command))
    }

    /// Local decisions never override explicit policy restrictions.
    fn review_requires_human(
        &self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        ctx: &AppContext,
    ) -> bool {
        if !is_agent_mode_autonomy_allowed(ctx) {
            return true;
        }
        let permissions = BlocklistAIPermissions::as_ref(ctx);
        let terminal = Some(self.terminal_view_id);
        let workspace =
            crate::workspaces::user_workspaces::UserWorkspaces::as_ref(ctx).ai_autonomy_settings();
        let requires_confirmation = match &action.action {
            AIAgentActionType::RequestCommandOutput { .. } => {
                workspace.execute_commands_setting == Some(ActionPermission::AlwaysAsk)
            }
            AIAgentActionType::RequestFileEdits { .. }
            | AIAgentActionType::EditDocuments(_)
            | AIAgentActionType::CreateDocuments(_) => {
                workspace.apply_code_diffs_setting == Some(ActionPermission::AlwaysAsk)
            }
            AIAgentActionType::WriteToLongRunningShellCommand { .. } => matches!(
                workspace.write_to_pty_setting,
                Some(
                    crate::ai::execution_profiles::WriteToPtyPermission::AlwaysAsk
                        | crate::ai::execution_profiles::WriteToPtyPermission::AskOnFirstWrite
                )
            ),
            AIAgentActionType::UseComputer(_) | AIAgentActionType::RequestComputerUse(_) => {
                matches!(
                    workspace.computer_use_setting,
                    Some(
                        crate::ai::execution_profiles::ComputerUsePermission::AlwaysAsk
                            | crate::ai::execution_profiles::ComputerUsePermission::Never
                    )
                )
            }
            action if is_read_only_inspection(action) => {
                workspace.read_files_setting == Some(ActionPermission::AlwaysAsk)
            }
            _ => false,
        };
        if requires_confirmation {
            return true;
        }
        match &action.action {
            AIAgentActionType::RequestCommandOutput { command, .. } => {
                // Test both supported shell escaping conventions conservatively. The normal
                // executor still applies the active shell's exact parsing and readiness checks.
                [
                    galaxy_util::path::EscapeChar::Backslash,
                    galaxy_util::path::EscapeChar::Backtick,
                ]
                .into_iter()
                .any(|escape| {
                    matches!(
                        permissions.can_autoexecute_command(
                            &conversation_id,
                            command,
                            escape,
                            false,
                            None,
                            terminal,
                            ctx
                        ),
                        CommandExecutionPermission::Denied(
                            CommandExecutionPermissionDeniedReason::ExplicitlyDenylisted
                        )
                    )
                })
            }
            AIAgentActionType::RequestFileEdits { file_edits, .. } => {
                let paths = file_edits
                    .iter()
                    .filter_map(|edit| edit.file().map(PathBuf::from))
                    .collect::<Vec<_>>();
                matches!(
                    permissions.can_write_files(&conversation_id, &paths, terminal, ctx),
                    FileWritePermission::Denied(FileWritePermissionDeniedReason::ProtectedPath)
                )
            }
            AIAgentActionType::CallMCPTool { .. } | AIAgentActionType::ReadMCPResource { .. } => {
                #[cfg(not(target_family = "wasm"))]
                {
                    let manager = TemplatableMCPServerManager::as_ref(ctx);
                    let template = match &action.action {
                        AIAgentActionType::CallMCPTool {
                            server_id, name, ..
                        } => server_id
                            .and_then(|id| manager.get_template_uuid(id))
                            .or_else(|| {
                                manager
                                    .server_from_tool(name.to_string())
                                    .copied()
                                    .and_then(|id| manager.get_template_uuid(id))
                            }),
                        AIAgentActionType::ReadMCPResource {
                            server_id,
                            name,
                            uri,
                            ..
                        } => server_id
                            .and_then(|id| manager.get_template_uuid(id))
                            .or_else(|| {
                                manager
                                    .server_from_resource(name, uri.as_deref())
                                    .copied()
                                    .and_then(|id| manager.get_template_uuid(id))
                            }),
                        _ => unreachable!(),
                    };
                    let denylist = permissions.get_mcp_denylist(ctx, terminal);
                    // An unresolved identity cannot establish that a configured restriction
                    // is irrelevant to this action.
                    template.map_or(!denylist.is_empty(), |id| denylist.contains(&id))
                }
                #[cfg(target_family = "wasm")]
                {
                    true
                }
            }
            _ => false,
        }
    }

    /// Returns None while assessment/approval is pending, Some(true) for a reviewed
    /// automatic approval, and Some(false) when normal permission handling applies.
    pub(super) fn review_pending_action(
        &mut self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        user_initiated: bool,
        ctx: &mut ModelContext<Self>,
    ) -> Option<bool> {
        let key = (conversation_id, action.id.clone());
        if self.is_view_only {
            self.action_reviews.remove(&key);
            return Some(false);
        }
        let execution_ref = self.provider_tool_execution_ref(conversation_id, &action.id);
        let environment = self.review_environment(ctx);
        if self.action_reviews.get(&key).is_some_and(|review| {
            review.action != *action
                || review.execution_ref != execution_ref
                || review.environment != environment
                || review
                    .authorization_fingerprint
                    .as_ref()
                    .is_some_and(|fingerprint| {
                        self.review_authorization(conversation_id, ctx)
                            .map(|(_, current)| current != *fingerprint)
                            .unwrap_or(true)
                    })
                || (matches!(review.decision, ReviewDecision::Allow)
                    && self.review_requires_human(action, conversation_id, ctx))
        }) {
            self.action_reviews.remove(&key);
            self.review_outcomes.remove(&key);
        }
        if let Some(ActionReview {
            decision: ReviewDecision::Ask { deadline, .. },
            ..
        }) = self.action_reviews.get(&key)
        {
            if Instant::now() >= *deadline {
                self.reject_reviewed_action(action, conversation_id, APPROVAL_EXPIRED_MESSAGE, ctx);
                return None;
            }
        }
        let mode = environment.permission_mode;
        let allowlisted = self.is_explicitly_allowlisted_command(action, ctx);
        if allowlisted && !self.review_requires_human(action, conversation_id, ctx) {
            self.action_reviews.remove(&key);
            return Some(true);
        }
        if let AIAgentActionType::WriteToLongRunningShellCommand { block_id, .. } = &action.action {
            let owns_command = self
                .executor
                .as_ref(ctx)
                .shell_command_executor()
                .as_ref(ctx)
                .can_continue_authorized_command(conversation_id, block_id, ctx);
            if owns_command {
                return Some(true);
            }
            self.reject_action_for_mode(action, conversation_id,
                "Only this conversation's owned background job can be cancelled. No process input was sent.", ctx);
            return None;
        }
        if mode == PermissionMode::ReadOnly && !is_read_only_inspection(&action.action) {
            self.reject_action_for_mode(action, conversation_id,
                "Read Only permits only file inspection, search, grep, and file discovery. This tool was not executed.", ctx);
            return None;
        }
        let restricted = self.review_requires_human(action, conversation_id, ctx);
        if mode == PermissionMode::AskFirst || mode == PermissionMode::ReadOnly {
            if user_initiated {
                // A stale click must not authorize an action after its scope changed.
                if !self.action_reviews.contains_key(&key)
                    && self.pending_permissions.contains(&key)
                {
                    self.request_action_approval(action, conversation_id, environment, ctx);
                    return None;
                }
                self.remember_user_approval(action, ctx);
                self.review_outcomes.insert(
                    key.clone(),
                    ReviewOutcome {
                        summary: review_summary(action),
                        detail: "Approved by you.".to_string(),
                        approved: true,
                    },
                );
                self.action_reviews.remove(&key);
                return Some(true);
            }
            let profile = AIExecutionProfilesModel::as_ref(ctx)
                .active_profile(Some(self.terminal_view_id), ctx);
            let remembered =
                self.approved_command_fingerprint(action, ctx)
                    .is_some_and(|fingerprint| {
                        profile
                            .data()
                            .approved_command_hashes
                            .contains(&fingerprint)
                    });
            if !restricted && (is_observation_or_coordination(&action.action) || remembered) {
                return Some(true);
            }
            if mode == PermissionMode::ReadOnly {
                self.reject_action_for_mode(
                    action,
                    conversation_id,
                    "Read Only cannot authorize inspection restricted by workspace policy.",
                    ctx,
                );
                return None;
            }
            if !self.action_reviews.contains_key(&key) {
                self.request_action_approval(action, conversation_id, environment, ctx);
            }
            return None;
        }
        if restricted {
            self.reject_action_for_mode(action, conversation_id,
                "Galaxy denied this action because it conflicts with a denylist, protected path, or workspace restriction. Revise the scope or choose a safer approach.", ctx);
            return None;
        }
        if mode == PermissionMode::AllowAll {
            self.action_reviews.remove(&key);
            return Some(true);
        }
        if matches!(&action.action, AIAgentActionType::RequestCommandOutput { command, .. }
            if is_safe_inspection_command(command))
        {
            return Some(true);
        }
        if !self.uses_galaxy_decides(action, ctx) {
            // Questions retain a separate task flow; Galaxy Decides skips them via the
            // mode-derived AskUserQuestionPermission rather than requesting approval.
            return Some(!matches!(
                action.action,
                AIAgentActionType::AskUserQuestion { .. }
            ));
        }
        if let Some(review) = self.action_reviews.get(&key) {
            if matches!(review.decision, ReviewDecision::Allow) {
                // Consume an exact local decision once. If execution is not
                // ready yet, its next attempt revalidates filesystem scope.
                self.action_reviews.remove(&key);
                return Some(true);
            }
            return None;
        }
        let (recorded_user_requests, authorization_fingerprint) =
            match self.review_authorization(conversation_id, ctx) {
                Ok(authorization) => authorization,
                Err(error) => {
                    self.reject_action_for_mode(
                        action,
                        conversation_id,
                        &format!("Galaxy Decides denied this action: {error}"),
                        ctx,
                    );
                    return None;
                }
            };
        let token = uuid::Uuid::new_v4();
        self.review_outcomes.remove(&key);
        let (cancel, control) = turn_control();
        self.action_reviews.insert(
            key.clone(),
            ActionReview {
                token,
                action: action.clone(),
                execution_ref,
                decision: ReviewDecision::Assessing,
                environment: environment.clone(),
                authorization_fingerprint: Some(authorization_fingerprint),
                cancel,
            },
        );
        ctx.emit(BlocklistAIActionEvent::PermissionReviewChanged {
            conversation_id,
            action_id: action.id.clone(),
        });
        let local_session = environment.native_file_paths
            && matches!(
                self.active_session.as_ref(ctx).session_type(ctx),
                Some(crate::terminal::model::session::SessionType::Local)
            );
        let proposed_action = action.action.clone();
        let started = Instant::now();
        let timeout_key = key.clone();
        ctx.spawn(
            async move {
                Ok(local_policy::assess(
                    &proposed_action,
                    environment.working_directory.as_deref(),
                    local_session,
                    &recorded_user_requests,
                    || matches!(control.try_receive(), Ok(TurnCommand::Cancel)),
                ))
            },
            move |me, result, ctx| me.finish_action_review(key, token, started, result, ctx),
        );
        // Filesystem resolution runs off the UI thread. A stalled filesystem
        // cannot leave permission checking pending or authorize a late result.
        ctx.spawn(
            async {
                Timer::after(REVIEW_TIMEOUT).await;
            },
            move |me, (), ctx| {
                let expired = me
                    .action_reviews
                    .get(&timeout_key)
                    .filter(|review| {
                        review.token == token
                            && matches!(review.decision, ReviewDecision::Assessing)
                    })
                    .map(|review| review.action.clone());
                if let Some(action) = expired {
                    me.reject_reviewed_action(
                        &action,
                        conversation_id,
                        "The local permission check timed out. The action was not executed.",
                        ctx,
                    );
                }
            },
        );
        None
    }

    fn finish_action_review(
        &mut self,
        key: (AIConversationId, AIAgentActionId),
        token: uuid::Uuid,
        started: Instant,
        result: anyhow::Result<Assessment>,
        ctx: &mut ModelContext<Self>,
    ) {
        let (conversation_id, action_id) = &key;
        let conversation_id = *conversation_id;
        let Some(review) = self.action_reviews.get(&key) else {
            return;
        };
        if review.token != token
            || self.provider_tool_execution_ref(conversation_id, action_id) != review.execution_ref
            || !self
                .pending_actions
                .get(&conversation_id)
                .is_some_and(|queue| queue.contains(&review.action))
        {
            return;
        }
        let action = review.action.clone();
        ctx.emit(BlocklistAIActionEvent::PermissionReviewChanged {
            conversation_id,
            action_id: action.id.clone(),
        });
        if review.environment != self.review_environment(ctx)
            || review
                .authorization_fingerprint
                .as_ref()
                .is_some_and(|fingerprint| {
                    self.review_authorization(conversation_id, ctx)
                        .map(|(_, current)| current != *fingerprint)
                        .unwrap_or(true)
                })
        {
            self.action_reviews.remove(&key);
            self.try_to_execute_available_actions(conversation_id, ctx);
            return;
        }
        if !self.uses_galaxy_decides(&action, ctx) {
            self.action_reviews.remove(&key);
            self.try_to_execute_available_actions(conversation_id, ctx);
            return;
        }
        let result = if started.elapsed() >= REVIEW_TIMEOUT {
            Err(anyhow::anyhow!("The local permission check timed out."))
        } else {
            result
        };
        let policy_requires_human = self.review_requires_human(&action, conversation_id, ctx);
        #[cfg(not(target_family = "wasm"))]
        if crate::ai::activity::is_recording() {
            crate::ai::activity::record_serializable(
                "risk_review_response",
                "Local permission policy decision",
                serde_json::json!({
                    "conversation_id": conversation_id.to_string(),
                    "action_id": action.id.to_string(),
                    "request_id": token.to_string(),
                    "assessment_kind": "local_policy",
                }),
                &serde_json::json!({
                    "allowed": result.as_ref().is_ok_and(|assessment| assessment.allows_automatic_execution(policy_requires_human)),
                    "reason": result.as_ref().ok().map(|assessment| assessment.reason.as_str()),
                    "error": result.as_ref().err().map(ToString::to_string),
                }),
            );
        }
        #[cfg(not(target_family = "wasm"))]
        log_tool_event(
            ctx,
            RemoteLogLevel::Info,
            "Galaxy local permission policy finished",
            serde_json::json!({
                "event": "tool_risk_assessment_finished",
                "assessment_kind": "local_policy",
                "conversation_id": conversation_id.to_string(),
                "action_id": action.id.to_string(),
                "elapsed_ms": started.elapsed().as_millis(),
                "risk": result.as_ref().ok().map(|assessment| assessment.risk),
                "assessment_succeeded": result.is_ok(),
                "assessment_error": result.as_ref().err().map(|error| error.to_string()),
                "policy_requires_human": policy_requires_human,
            }),
        );
        if result
            .as_ref()
            .is_ok_and(|assessment| assessment.allows_automatic_execution(policy_requires_human))
        {
            let assessment = result.as_ref().unwrap();
            self.review_outcomes.insert(
                key.clone(),
                ReviewOutcome {
                    summary: review_summary(&action),
                    detail: format!(
                        "Approved by Galaxy local policy · risk {}/5. {}",
                        assessment.risk, assessment.reason
                    ),
                    approved: true,
                },
            );
            self.action_reviews.get_mut(&key).unwrap().decision = ReviewDecision::Allow;
            self.try_to_execute_available_actions(conversation_id, ctx);
            return;
        }
        let assessment_reason = match result {
            Ok(assessment) => {
                let risk = assessment.risk;
                let reason = assessment.reason;
                format!("Galaxy Decides local policy: {reason} (risk {risk}/5).")
            }
            Err(error) => {
                format!("Galaxy Decides local policy could not establish permission ({error}).")
            }
        };
        let reason = format!("{assessment_reason} The action was not executed. Choose a narrower supported tool or an explicit permission grant. An agent rationale or a revised action explanation cannot grant permission; no model fallback or approval pause was requested.");
        self.reject_reviewed_action(&action, conversation_id, &reason, ctx);
    }

    fn reject_action_for_mode(
        &mut self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        reason: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        let key = (conversation_id, action.id.clone());
        let (cancel, control) = turn_control();
        drop(control);
        self.action_reviews.insert(
            key,
            ActionReview {
                token: uuid::Uuid::new_v4(),
                action: action.clone(),
                execution_ref: self.provider_tool_execution_ref(conversation_id, &action.id),
                decision: ReviewDecision::Assessing,
                environment: self.review_environment(ctx),
                authorization_fingerprint: None,
                cancel,
            },
        );
        self.reject_reviewed_action(action, conversation_id, reason, ctx);
    }

    fn request_action_approval(
        &mut self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        environment: ReviewEnvironment,
        ctx: &mut ModelContext<Self>,
    ) {
        let key = (conversation_id, action.id.clone());
        let token = uuid::Uuid::new_v4();
        let (cancel, control) = turn_control();
        drop(control);
        self.action_reviews.insert(key.clone(), ActionReview {
            token, action: action.clone(),
            execution_ref: self.provider_tool_execution_ref(conversation_id, &action.id),
            decision: ReviewDecision::Ask {
                reason: "Ask First: approve this action before it runs. Exact commands are remembered for this profile and working directory. Approval expires after 60 seconds.".to_owned(),
                deadline: Instant::now() + APPROVAL_TIMEOUT,
            },
            environment, authorization_fingerprint: None, cancel,
        });
        self.handle_not_executed_action(
            action,
            NotExecutedReason::NeedsConfirmation,
            conversation_id,
            ctx,
        );
        ctx.spawn(
            async {
                Timer::after(APPROVAL_TIMEOUT).await;
            },
            move |me, (), ctx| {
                let expired = me
                    .action_reviews
                    .get(&key)
                    .filter(|review| {
                        review.token == token
                            && matches!(review.decision, ReviewDecision::Ask { .. })
                    })
                    .map(|review| review.action.clone());
                if let Some(action) = expired {
                    me.reject_reviewed_action(
                        &action,
                        conversation_id,
                        APPROVAL_EXPIRED_MESSAGE,
                        ctx,
                    );
                }
            },
        );
    }

    pub(crate) fn has_permission_review(&self, conversation_id: AIConversationId) -> bool {
        self.action_reviews.iter().any(|((owner, _), review)| {
            *owner == conversation_id && matches!(review.decision, ReviewDecision::Assessing)
        })
    }

    pub(super) fn is_reviewing_action(
        &self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> bool {
        self.action_reviews
            .get(&(conversation_id, action_id.clone()))
            .is_some_and(|review| matches!(review.decision, ReviewDecision::Assessing))
    }

    pub(crate) fn review_reason(
        &self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> Option<String> {
        match &self
            .action_reviews
            .get(&(conversation_id, action_id.clone()))?
            .decision
        {
            ReviewDecision::Ask { reason, .. } => Some(reason.clone()),
            ReviewDecision::Assessing | ReviewDecision::Allow => None,
        }
    }

    pub(crate) fn review_display(
        &self,
        conversation_id: AIConversationId,
        action_id: &AIAgentActionId,
    ) -> Option<ReviewDisplay> {
        let key = (conversation_id, action_id.clone());
        if let Some(review) = self.action_reviews.get(&key) {
            let summary = review_summary(&review.action);
            return Some(match &review.decision {
                ReviewDecision::Assessing => ReviewDisplay {
                    state: ReviewDisplayState::Assessing,
                    summary,
                    detail: "Checking local permissions…".to_string(),
                },
                ReviewDecision::Ask { reason, .. } => ReviewDisplay {
                    state: ReviewDisplayState::AwaitingApproval,
                    summary,
                    detail: shorten_for_review(reason, 320),
                },
                ReviewDecision::Allow => ReviewDisplay {
                    state: ReviewDisplayState::Approved,
                    summary,
                    detail: shorten_for_review(&self.review_outcomes.get(&key)?.detail, 320),
                },
            });
        }
        self.review_outcomes.get(&key).map(|outcome| ReviewDisplay {
            state: if outcome.approved {
                ReviewDisplayState::Approved
            } else {
                ReviewDisplayState::Denied
            },
            summary: outcome.summary.clone(),
            detail: shorten_for_review(&outcome.detail, 320),
        })
    }

    fn reject_reviewed_action(
        &mut self,
        action: &AIAgentAction,
        conversation_id: AIConversationId,
        reason: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        let key = (conversation_id, action.id.clone());
        let Some(review) = self.action_reviews.remove(&key) else {
            return;
        };
        if self.provider_tool_execution_ref(conversation_id, &action.id) != review.execution_ref {
            return;
        }
        let Some(queue) = self.pending_actions.get_mut(&conversation_id) else {
            return;
        };
        let Some(index) = queue.iter().position(|pending| pending == action) else {
            return;
        };
        let action = queue.remove(index).unwrap();
        self.review_outcomes.insert(
            key.clone(),
            ReviewOutcome {
                summary: review_summary(&action),
                detail: reason.to_string(),
                approved: false,
            },
        );
        // Automatic denials still need a complete, correlated permission lifecycle;
        // they never emit a user-facing confirmation request.
        if !self.pending_permissions.contains(&key) {
            ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
                action_id: action.id.clone(),
                execution_ref: review.execution_ref.clone(),
                event: ToolEvent::PermissionRequested {
                    request: galaxy_agent_core::PermissionRequest {
                        id: permission_request_id(&action.id),
                        call_id: action.id.to_string(),
                        kind: permission_kind_for_action(&action.action),
                        reason: Some(reason.to_string()),
                    },
                },
            });
        }
        self.pending_permissions.remove(&key);
        #[cfg(not(target_family = "wasm"))]
        log_tool_event(
            ctx,
            RemoteLogLevel::Info,
            "Galaxy action denied",
            serde_json::json!({
                "event": "tool_permission_denied",
                "reason": reason,
                "conversation_id": conversation_id.to_string(),
                "action_id": action.id.to_string(),
                "timeout_seconds": APPROVAL_TIMEOUT.as_secs(),
            }),
        );
        self.permission_denial_reasons
            .insert(key.clone(), reason.to_string());
        self.denied_permissions.insert(key);
        ctx.emit(BlocklistAIActionEvent::ToolLifecycle {
            action_id: action.id.clone(),
            execution_ref: review.execution_ref.clone(),
            event: ToolEvent::PermissionResolved {
                request_id: permission_request_id(&action.id),
                call_id: action.id.to_string(),
                decision: PermissionDecision::Denied {
                    reason: Some(reason.to_string()),
                },
            },
        });
        self.cancel_pending_action(conversation_id, action, None, false, ctx);
        self.try_to_execute_available_actions(conversation_id, ctx);
    }
}

#[cfg(test)]
#[path = "risk_review_tests.rs"]
mod tests;
