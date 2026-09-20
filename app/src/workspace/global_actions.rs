use std::path::PathBuf;
use std::time::Duration;

use ::settings::ToggleableSetting;
use galaxy_core::execution_mode::AppExecutionMode;
use galaxy_graphql::mutations::create_anonymous_user::AnonymousUserType;
use galaxyui::r#async::{SpawnedFutureHandle, Timer};
use galaxyui::windowing::WindowManager;
use galaxyui::{AppContext, Entity, ModelContext, SingletonEntity, TypedActionView};

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::AIAgentExchangeId;
use crate::app_state::get_app_state;
use crate::network::NetworkStatus;
use crate::persistence::ModelEvent;
use crate::root_view::OpenPath;
use crate::server::server_api::ServerApiProvider;
use crate::terminal::alt_screen_reporting::AltScreenReporting;
use crate::undo_close::UndoCloseStack;
use crate::workspace::cross_window_tab_drag::CrossWindowTabDrag;
use crate::workspace::{Workspace, WorkspaceAction};
use crate::{auth, GlobalResourceHandlesProvider};

const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_millis(250);

pub(crate) struct SessionSaveCoordinator {
    pending_save: bool,
    timer_generation: u64,
    timer: Option<SpawnedFutureHandle>,
}

impl SessionSaveCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            pending_save: false,
            timer_generation: 0,
            timer: None,
        }
    }

    fn request_save(&mut self, ctx: &mut ModelContext<Self>) {
        if !session_save_is_enabled(ctx) {
            return;
        }

        if self.timer.is_none() {
            self.enqueue_snapshot(ctx);
        } else {
            self.pending_save = true;
        }
        self.restart_timer(ctx);
    }

    fn save_urgently(&mut self, ctx: &mut ModelContext<Self>) {
        self.timer_generation = self.timer_generation.wrapping_add(1);
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
        self.pending_save = false;

        if session_save_is_enabled(ctx) {
            self.enqueue_snapshot(ctx);
        }
    }

    fn restart_timer(&mut self, ctx: &mut ModelContext<Self>) {
        if let Some(timer) = self.timer.take() {
            timer.abort();
        }
        self.timer_generation = self.timer_generation.wrapping_add(1);
        let generation = self.timer_generation;
        self.timer = Some(ctx.spawn(
            async move { Timer::after(SESSION_SAVE_DEBOUNCE).await },
            move |coordinator, _, ctx| {
                if coordinator.timer_generation != generation {
                    return;
                }
                coordinator.timer = None;
                if std::mem::take(&mut coordinator.pending_save) {
                    coordinator.enqueue_snapshot(ctx);
                }
            },
        ));
    }

    fn enqueue_snapshot(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(model_event_sender) = GlobalResourceHandlesProvider::as_ref(ctx)
            .get()
            .model_event_sender
            .clone()
        else {
            return;
        };
        let app_state = get_app_state(ctx);
        if let Err(err) = model_event_sender.send(ModelEvent::Snapshot(app_state)) {
            log::error!("Error trying to send model event {err:?}");
        }
    }
}

impl Entity for SessionSaveCoordinator {
    type Event = ();
}

impl SingletonEntity for SessionSaveCoordinator {}

fn session_save_is_enabled(ctx: &AppContext) -> bool {
    AppExecutionMode::as_ref(ctx).can_save_session() && !CrossWindowTabDrag::as_ref(ctx).is_active()
}

/// Specifies where a forked conversation should be opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ForkedConversationDestination {
    /// Open the forked conversation in a new pane (split to the right).
    #[default]
    SplitPane,
    /// Open the forked conversation in the current pane, replacing the current view.
    CurrentPane,
    /// Open the forked conversation in a new tab.
    NewTab,
}

impl ForkedConversationDestination {
    /// Fork destination from an Enter (`false`) / Cmd-or-Ctrl+Enter (`true`) trigger: Enter
    /// opens a new split pane, Cmd/Ctrl+Enter opens a new tab. Shared by all fork-style commands.
    pub fn for_fork_trigger(cmd_or_ctrl_enter: bool) -> Self {
        if cmd_or_ctrl_enter {
            Self::NewTab
        } else {
            Self::SplitPane
        }
    }

    pub fn is_new_tab(&self) -> bool {
        matches!(self, Self::NewTab)
    }

    pub fn is_split_pane(&self) -> bool {
        matches!(self, Self::SplitPane)
    }

    pub fn is_current_pane(&self) -> bool {
        matches!(self, Self::CurrentPane)
    }
}

/// Specifies the exchange at which to fork an AI conversation.
#[derive(Debug, Clone, Copy)]
pub struct ForkFromExchange {
    pub exchange_id: AIAgentExchangeId,
    /// When true, the fork stops immediately after this exchange without extending
    /// to the next user query boundary.
    pub fork_from_exact_exchange: bool,
}

/// Parameters for forking an AI conversation.
pub struct ForkAIConversationParams {
    pub conversation_id: AIConversationId,
    /// When Some, fork from the given response (or exchange if `fork_from_exact_exchange` is true).
    pub fork_from_exchange: Option<ForkFromExchange>,
    pub summarize_after_fork: bool,
    pub summarization_prompt: Option<String>,
    pub initial_prompt: Option<String>,
    pub destination: ForkedConversationDestination,
}

/// DEPRECATED. Global actions are being phased out.
/// Do not add any more global actions; use typed actions instead.
pub fn init_global_actions(app: &mut AppContext) {
    app.add_global_action("workspace:toggle_mouse_reporting", toggle_mouse_reporting);
    app.add_global_action("workspace:toggle_scroll_reporting", toggle_scroll_reporting);
    app.add_global_action("workspace:toggle_focus_reporting", toggle_focus_reporting);
    app.add_global_action("workspace:save_app", save_app);
    app.add_global_action("workspace:fork_ai_conversation", fork_ai_conversation);
    app.add_global_action(
        "workspace:summarize_ai_conversation",
        summarize_ai_conversation,
    );
    app.add_global_action(
        "workspace:toggle_debug_network_status",
        toggle_debug_network_status,
    );
    app.add_global_action(
        "workspace:debug_create_anonymous_user",
        create_anonymous_user,
    );
    app.add_global_action("workspace:open_repository", open_repository);
    app.add_global_action("app:undo_close", undo_close);
    app.add_global_action("app:maybe_log_out", trigger_maybe_log_out);
    app.add_global_action("app:log_out", trigger_log_out);
}

fn toggle_mouse_reporting(_: &(), ctx: &mut AppContext) {
    AltScreenReporting::handle(ctx).update(ctx, |reporting, ctx| {
        reporting
            .mouse_reporting_enabled
            .toggle_and_save_value(ctx)
            .expect("MouseReportingEnabled failed to serialize");
    });
}

fn toggle_scroll_reporting(_: &(), ctx: &mut AppContext) {
    AltScreenReporting::handle(ctx).update(ctx, |reporting, ctx| {
        reporting
            .scroll_reporting_enabled
            .toggle_and_save_value(ctx)
            .expect("ScrollReportingEnabled failed to serialize");
    });
}

fn toggle_focus_reporting(_: &(), ctx: &mut AppContext) {
    AltScreenReporting::handle(ctx).update(ctx, |reporting, ctx| {
        reporting
            .focus_reporting_enabled
            .toggle_and_save_value(ctx)
            .expect("FocusReportingEnabled failed to serialize");
    });
}

fn save_app(_: &(), ctx: &mut AppContext) {
    SessionSaveCoordinator::handle(ctx).update(ctx, |coordinator, ctx| {
        coordinator.request_save(ctx);
    });
}

pub(crate) fn save_app_urgently(ctx: &mut AppContext) {
    SessionSaveCoordinator::handle(ctx).update(ctx, |coordinator, ctx| {
        coordinator.save_urgently(ctx);
    });
}

fn toggle_debug_network_status(_: &(), ctx: &mut AppContext) {
    NetworkStatus::handle(ctx).update(ctx, move |me, ctx| {
        let is_reachable = me.is_online();
        let new_is_reachable = !is_reachable;
        if new_is_reachable {
            log::info!("Manually toggled network status to be reachable");
        } else {
            log::info!("Manually toggled network status to be not reachable");
        }
        me.reachability_changed(new_is_reachable, ctx)
    });
}

fn create_anonymous_user(_: &(), ctx: &mut AppContext) {
    log::info!("Creating anonymous user");
    let anonymous_user_type = AnonymousUserType::NativeClientAnonymousUser;
    let auth_client =
        ServerApiProvider::handle(ctx).read(ctx, |provider, _ctx| provider.get_auth_client());
    let result =
        galaxyui::r#async::block_on(auth_client.create_anonymous_user(None, anonymous_user_type));
    match result {
        Ok(user) => log::info!("Successfully created anonymous user {user:?}"),
        Err(err) => log::error!("Failed to create anonymous user: {err:?}"),
    }
}

/// Reopens the last closed item (window or tab).
fn undo_close(_: &(), ctx: &mut AppContext) {
    UndoCloseStack::handle(ctx).update(ctx, |stack, ctx| {
        stack.undo_close(ctx);
    });
}

fn trigger_maybe_log_out(_: &(), ctx: &mut AppContext) {
    auth::maybe_log_out(ctx)
}

/// Dispatches an action to the active workspace, if one exists.
fn dispatch_to_active_workspace(ctx: &mut AppContext, action: WorkspaceAction) {
    if let Some(window_id) = WindowManager::as_ref(ctx).active_window() {
        if let Some(workspaces) = ctx.views_of_type::<Workspace>(window_id) {
            if let Some(workspace) = workspaces.into_iter().next() {
                workspace.update(ctx, |workspace, ctx| {
                    workspace.handle_action(&action, ctx);
                });
            }
        }
    }
}

fn open_repository(path: &String, ctx: &mut AppContext) {
    if WindowManager::as_ref(ctx).active_window().is_some() {
        // There's an active window, dispatch to its workspace
        dispatch_to_active_workspace(
            ctx,
            WorkspaceAction::OpenRepository {
                path: Some(path.clone()),
            },
        );
    } else {
        // No active window, create a new one with the repository path
        let path_buf = PathBuf::from(path);
        ctx.dispatch_global_action("root_view:open_new_from_path", &OpenPath { path: path_buf });
    }
}

fn fork_ai_conversation(params: &ForkAIConversationParams, ctx: &mut AppContext) {
    dispatch_to_active_workspace(
        ctx,
        WorkspaceAction::ForkAIConversation {
            conversation_id: params.conversation_id,
            fork_from_exchange: params.fork_from_exchange,
            summarize_after_fork: params.summarize_after_fork,
            summarization_prompt: params.summarization_prompt.clone(),
            initial_prompt: params.initial_prompt.clone(),
            initial_attachments: vec![],
            destination: params.destination,
        },
    );
}

fn summarize_ai_conversation(prompt: &Option<String>, ctx: &mut AppContext) {
    dispatch_to_active_workspace(
        ctx,
        WorkspaceAction::SummarizeAIConversation {
            prompt: prompt.clone(),
            initial_prompt: None,
        },
    );
}

fn trigger_log_out(_: &(), ctx: &mut AppContext) {
    auth::log_out(ctx)
}
