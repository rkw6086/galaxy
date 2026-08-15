use std::sync::Arc;

use async_channel::unbounded;
use futures::channel::oneshot;
use parking_lot::FairMutex;
use warpui::{App, EntityId};

use super::{command_for_execution, ActionResult, BlockSelector, ShellCommandExecutor};
use crate::ai::agent::ShellCommandDelay;
use crate::terminal::event::{BlockMetadataReceivedEvent, BlockWorkingDirectoryUpdatedEvent};
use crate::terminal::model::block::{BlockId, BlockMetadata};
use crate::terminal::model::session::active_session::ActiveSession;
use crate::terminal::model::session::Sessions;
use crate::terminal::model::terminal_model::{BlockIndex, TerminalModel};
use crate::terminal::model_events::{ModelEvent, ModelEventDispatcher};
use crate::terminal::shell::ShellType;

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
            executor.block_finished_senders.insert(selector, tx);
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

        // Precmd event — the senders map should be drained (and since the
        // block isn't in the terminal model, the sender is dropped).
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
            0,
            "BlockMetadataReceived should drain the finish senders"
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
                .insert(BlockSelector::Id(block_id.clone()), tx);
            assert!(executor.force_refresh_block(&block_id));
            assert!(!executor.force_refresh_block(&block_id));
        });

        assert!(matches!(rx.try_recv(), Ok(Some(()))));

        let (tx, _rx) = oneshot::channel();
        executor.update(&mut app, |executor, _| {
            executor
                .force_refresh_senders
                .insert(BlockSelector::Id(block_id.clone()), tx);
        });
        terminal_model.lock().finish_block();
        assert!(executor.update(&mut app, |executor, _| {
            !executor.force_refresh_block(&block_id)
        }));
    });
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
