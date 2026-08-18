use std::sync::Arc;
use std::task::Poll;

use async_channel::unbounded;
use futures::channel::oneshot;
use futures::{pin_mut, poll};
use parking_lot::FairMutex;
use warpui::{App, EntityId};

use super::{
    command_for_execution, selected_block_owns_alt_screen, terminal_busy_execution_error,
    ActionResult, BlockSelector, ShellCommandExecutor,
};
use crate::ai::agent::{
    AIAgentActionId, AIAgentActionResultType, RequestCommandOutputResult, ShellCommandDelay,
};
use crate::terminal::event::{
    BlockCompletedEvent, BlockMetadataReceivedEvent, BlockType, BlockWorkingDirectoryUpdatedEvent,
};
use crate::terminal::model::block::{BlockId, BlockMetadata};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model::session::Sessions;
use crate::terminal::model::terminal_model::{BlockIndex, TerminalModel};
use crate::terminal::model_events::{ModelEvent, ModelEventDispatcher};
use crate::terminal::shell::ShellType;
use crate::AIConversationId;

#[test]
fn wait_commands_disable_implicit_posix_pagers_without_masking_exit_status() {
    let command = "git log -8 --oneline && false";
    let decorated = command_for_execution(command, Some(ShellType::Zsh), true);

    assert_eq!(
        decorated,
        "(export PAGER=cat GIT_PAGER=cat GH_PAGER=cat AWS_PAGER=cat SYSTEMD_PAGER=cat; git log -8 --oneline && false)"
    );
    assert!(!decorated.contains("| command cat"));
    assert_eq!(
        command_for_execution(command, Some(ShellType::Zsh), false),
        command
    );
}

#[test]
fn terminal_busy_is_an_execution_error_for_the_unstarted_command() {
    let result = terminal_busy_execution_error("cargo test", "sleep 120");

    assert!(matches!(
        result,
        AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::ExecutionError { command, message }
        ) if command == "cargo test"
            && message == "terminal is busy running command 'sleep 120'"
    ));
}

#[test]
fn targeted_poll_uses_alt_screen_only_for_its_owning_block() {
    let active_block_id = BlockId::new();
    let selected_block_id = BlockId::new();

    assert!(!selected_block_owns_alt_screen(
        true,
        &active_block_id,
        &selected_block_id
    ));
    assert!(selected_block_owns_alt_screen(
        true,
        &active_block_id,
        &active_block_id
    ));
    assert!(!selected_block_owns_alt_screen(
        false,
        &active_block_id,
        &active_block_id
    ));
}

/// Locks in the contract that `ShellCommandExecutor`'s requested-command finish
/// detector reacts only to `BlockMetadataReceived` (precmd) and not to
/// `BlockWorkingDirectoryUpdated` (OSC 7). The detector relies on
/// `BlockMetadataReceived` firing exactly once per block; OSC 7 can fire many
/// times per block, so wiring it into the detector would resolve the wait
/// future before the requested command actually finishes.
#[test]
fn block_working_directory_updated_does_not_drain_finish_senders() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_model_events_tx, model_events_rx) = unbounded();
        let model_event_dispatcher =
            app.add_model(|ctx| ModelEventDispatcher::new(model_events_rx, sessions.clone(), ctx));
        let active_session = app.add_model(|ctx| {
            ActiveSession::new(sessions.clone(), model_event_dispatcher.clone(), ctx)
        });
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        let executor = app.add_model(|ctx| {
            ShellCommandExecutor::new(
                active_session,
                terminal_model.clone(),
                &model_event_dispatcher,
                terminal_view_id,
                ctx,
            )
        });

        let block_id = BlockId::new();
        let selector = BlockSelector::Id(block_id);
        let (tx, _rx) = oneshot::channel::<()>();
        executor.update(&mut app, |executor, _ctx| {
            executor.block_finished_senders.insert(selector, vec![tx]);
        });
        assert_eq!(
            app.read(|ctx| executor.as_ref(ctx).block_finished_senders.len()),
            1
        );

        // OSC 7 update — must NOT drain or resolve the finish sender.
        model_event_dispatcher.update(&mut app, |_dispatcher, ctx| {
            ctx.emit(ModelEvent::BlockWorkingDirectoryUpdated(
                BlockWorkingDirectoryUpdatedEvent {
                    block_metadata: BlockMetadata::new(None, Some("/tmp/new".to_string())),
                    block_index: BlockIndex::zero(),
                    is_for_in_band_command: false,
                    is_done_bootstrapping: true,
                },
            ));
        });
        assert_eq!(
            app.read(|ctx| executor.as_ref(ctx).block_finished_senders.len()),
            1,
            "BlockWorkingDirectoryUpdated must not touch block_finished_senders — \
             that map is reserved for precmd (BlockMetadataReceived)"
        );

        // An unrelated precmd cannot resolve this selector, so its waiter must survive.
        model_event_dispatcher.update(&mut app, |_dispatcher, ctx| {
            ctx.emit(ModelEvent::BlockMetadataReceived(
                BlockMetadataReceivedEvent {
                    block_metadata: BlockMetadata::new(None, Some("/tmp/precmd".to_string())),
                    block_index: BlockIndex::zero(),
                    is_after_in_band_command: false,
                    is_done_bootstrapping: true,
                },
            ));
        });
        assert_eq!(
            app.read(|ctx| executor.as_ref(ctx).block_finished_senders.len()),
            1,
            "BlockMetadataReceived must retain unresolved finish senders"
        );
    });
}

#[test]
fn force_refresh_block_reports_and_resolves_matching_poll() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_model_events_tx, model_events_rx) = unbounded();
        let model_event_dispatcher =
            app.add_model(|ctx| ModelEventDispatcher::new(model_events_rx, sessions.clone(), ctx));
        let active_session = app.add_model(|ctx| {
            ActiveSession::new(sessions.clone(), model_event_dispatcher.clone(), ctx)
        });
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        terminal_model
            .lock()
            .simulate_long_running_block("sleep 120", "still running");
        let block_id = terminal_model.lock().active_block_id().clone();
        let executor = app.add_model(|ctx| {
            ShellCommandExecutor::new(
                active_session,
                terminal_model.clone(),
                &model_event_dispatcher,
                terminal_view_id,
                ctx,
            )
        });

        let (tx, mut rx) = oneshot::channel();
        executor.update(&mut app, |executor, _| {
            executor
                .force_refresh_senders
                .insert(BlockSelector::Id(block_id.clone()), vec![tx]);
            assert!(executor.force_refresh_block(&block_id));
            assert!(!executor.force_refresh_block(&block_id));
        });

        assert!(matches!(rx.try_recv(), Ok(Some(()))));

        let (tx, _rx) = oneshot::channel();
        executor.update(&mut app, |executor, _| {
            executor
                .force_refresh_senders
                .insert(BlockSelector::Id(block_id.clone()), vec![tx]);
        });
        terminal_model.lock().finish_block();
        assert!(executor.update(&mut app, |executor, _| {
            !executor.force_refresh_block(&block_id)
        }));
    });
}

#[test]
fn requested_command_waiter_survives_early_metadata_and_resolves_after_association() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_model_events_tx, model_events_rx) = unbounded();
        let model_event_dispatcher =
            app.add_model(|ctx| ModelEventDispatcher::new(model_events_rx, sessions.clone(), ctx));
        let active_session = app.add_model(|ctx| {
            ActiveSession::new(sessions.clone(), model_event_dispatcher.clone(), ctx)
        });
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        let executor = app.add_model(|ctx| {
            ShellCommandExecutor::new(
                active_session,
                terminal_model.clone(),
                &model_event_dispatcher,
                terminal_view_id,
                ctx,
            )
        });
        let action_id = AIAgentActionId::from("requested-command".to_string());
        let result_future = executor.update(&mut app, |executor, _| {
            executor.action_result_future(
                BlockSelector::RequestedCommandId(action_id.clone()),
                Some(ShellCommandDelay::OnCompletion),
            )
        });
        pin_mut!(result_future);

        model_event_dispatcher.update(&mut app, |_dispatcher, ctx| {
            ctx.emit(ModelEvent::BlockMetadataReceived(
                BlockMetadataReceivedEvent {
                    block_metadata: BlockMetadata::new(None, Some("/tmp/early".to_string())),
                    block_index: BlockIndex::zero(),
                    is_after_in_band_command: false,
                    is_done_bootstrapping: true,
                },
            ));
        });
        assert!(matches!(poll!(&mut result_future), Poll::Pending));

        terminal_model
            .lock()
            .simulate_long_running_block("printf done", "done");
        let block_id = terminal_model.lock().active_block_id().clone();
        terminal_model
            .lock()
            .block_list_mut()
            .active_block_mut()
            .set_agent_interaction_mode_for_requested_command(
                action_id,
                None,
                AIConversationId::new(),
            );
        terminal_model.lock().finish_block();
        model_event_dispatcher.update(&mut app, |_dispatcher, ctx| {
            ctx.emit(ModelEvent::BlockCompleted(block_completed_event(
                block_id.clone(),
            )));
        });

        assert!(matches!(
            result_future.await,
            ActionResult::CommandFinished {
                block_id: result_block_id,
                ..
            } if result_block_id == block_id
        ));
    });
}

#[test]
fn duplicate_completion_polls_for_same_block_both_resolve_on_block_completed() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_model_events_tx, model_events_rx) = unbounded();
        let model_event_dispatcher =
            app.add_model(|ctx| ModelEventDispatcher::new(model_events_rx, sessions.clone(), ctx));
        let active_session = app.add_model(|ctx| {
            ActiveSession::new(sessions.clone(), model_event_dispatcher.clone(), ctx)
        });
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        terminal_model
            .lock()
            .simulate_long_running_block("sleep 1", "finished");
        let block_id = terminal_model.lock().active_block_id().clone();
        let executor = app.add_model(|ctx| {
            ShellCommandExecutor::new(
                active_session,
                terminal_model.clone(),
                &model_event_dispatcher,
                terminal_view_id,
                ctx,
            )
        });

        let first = executor.update(&mut app, |executor, _| {
            executor.action_result_future(
                BlockSelector::Id(block_id.clone()),
                Some(ShellCommandDelay::OnCompletion),
            )
        });
        let second = executor.update(&mut app, |executor, _| {
            executor.action_result_future(
                BlockSelector::Id(block_id.clone()),
                Some(ShellCommandDelay::OnCompletion),
            )
        });
        pin_mut!(first);
        pin_mut!(second);
        assert!(matches!(poll!(&mut first), Poll::Pending));
        assert!(matches!(poll!(&mut second), Poll::Pending));

        terminal_model.lock().finish_block();
        model_event_dispatcher.update(&mut app, |_dispatcher, ctx| {
            ctx.emit(ModelEvent::BlockCompleted(block_completed_event(
                block_id.clone(),
            )));
        });

        let first_result = first.await;
        let second_result = second.await;
        assert!(matches!(first_result, ActionResult::CommandFinished { .. }));
        assert!(matches!(
            second_result,
            ActionResult::CommandFinished { .. }
        ));
    });
}

fn block_completed_event(block_id: BlockId) -> BlockCompletedEvent {
    BlockCompletedEvent {
        block_latency_data: None,
        block_type: BlockType::Restored,
        num_secrets_obfuscated: 0,
        block_index: BlockIndex::zero(),
        block_id,
        session_id: None,
        restored_block_was_local: None,
    }
}

#[test]
fn force_refresh_wakes_on_completion_poll_with_preempted_snapshot() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_model_events_tx, model_events_rx) = unbounded();
        let model_event_dispatcher =
            app.add_model(|ctx| ModelEventDispatcher::new(model_events_rx, sessions.clone(), ctx));
        let active_session = app.add_model(|ctx| {
            ActiveSession::new(sessions.clone(), model_event_dispatcher.clone(), ctx)
        });
        let terminal_model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        terminal_model
            .lock()
            .simulate_long_running_block("sleep 120", "still running");
        let block_id = terminal_model.lock().active_block_id().clone();
        let executor = app.add_model(|ctx| {
            ShellCommandExecutor::new(
                active_session,
                terminal_model,
                &model_event_dispatcher,
                terminal_view_id,
                ctx,
            )
        });

        let result_future = executor.update(&mut app, |executor, _| {
            executor.action_result_future(
                BlockSelector::Id(block_id.clone()),
                Some(ShellCommandDelay::OnCompletion),
            )
        });
        assert!(executor.update(&mut app, |executor, _| {
            executor.force_refresh_block(&block_id)
        }));
        let result = result_future.await;

        assert!(matches!(
            result,
            ActionResult::LongRunningCommandSnapshot {
                block_id: result_block_id,
                is_preempted: true,
                ..
            } if result_block_id == block_id
        ));
    });
}
