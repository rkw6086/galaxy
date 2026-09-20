use galaxyui::App;

use super::*;
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::AIAgentActionId;
use crate::terminal::model::block::AgentInteractionMetadata;

struct RecordingEventLoopSender(Arc<FairMutex<Vec<Message>>>);

impl EventLoopSender for RecordingEventLoopSender {
    fn send(&self, message: Message) -> Result<(), EventLoopSendError> {
        self.0.lock().push(message);
        Ok(())
    }
}

#[test]
fn queued_agent_command_claims_only_the_block_at_dispatch() {
    App::test((), |mut app| async move {
        let sessions = app.add_model(|_| Sessions::new_for_test());
        let (_events_tx, events_rx) = async_channel::unbounded();
        let dispatcher =
            app.add_model(|ctx| ModelEventDispatcher::new(events_rx, sessions.clone(), ctx));
        let line_editor =
            app.add_model(|ctx| LineEditorStatus::new(dispatcher.clone(), sessions.clone(), ctx));
        let (_commands_tx, commands_rx) = async_channel::unbounded();
        let model = Arc::new(FairMutex::new(TerminalModel::mock(None, None)));
        let writes = Arc::new(FairMutex::new(Vec::new()));
        let controller = app.add_model(|ctx| {
            PtyController::new(
                RecordingEventLoopSender(writes.clone()),
                dispatcher,
                line_editor,
                sessions,
                commands_rx,
                model.clone(),
                ctx,
            )
        });
        let conversation_id = AIConversationId::new();
        let previous_action = AIAgentActionId::from("previous-command".to_string());
        let next_action = AIAgentActionId::from("follow-up-command".to_string());
        let previous_block = {
            let mut model = model.lock();
            model.simulate_long_running_block("echo previous", "previous output");
            model
                .block_list_mut()
                .active_block_mut()
                .set_agent_interaction_mode_for_requested_command(
                    previous_action.clone(),
                    None,
                    conversation_id,
                );
            model.active_block_id().clone()
        };
        controller.update(&mut app, |controller, ctx| {
            controller.write_command(
                &ExecuteCommandEvent {
                    command: "echo follow-up".into(),
                    session_id: 1.into(),
                    workflow_id: None,
                    workflow_command: None,
                    should_add_command_to_history: false,
                    source: CommandExecutionSource::AI {
                        metadata: AgentInteractionMetadata::new_hidden(
                            next_action.clone(),
                            conversation_id,
                        ),
                    },
                },
                ShellType::Zsh,
                ctx,
            );
            assert_eq!(controller.pending_writes.len(), 1);
            controller.cancel_queued_agent_command(&previous_action);
            assert_eq!(controller.pending_writes.len(), 1);
            let write = controller.pending_writes.pop_front().unwrap();
            let PtyWrite::Command {
                execution: Some(event),
                ..
            } = &write
            else {
                panic!("expected queued agent command");
            };
            let event = event.clone();
            controller.pending_writes.push_back(write);
            controller.cancel_queued_agent_command(&next_action);
            assert!(controller.pending_writes.is_empty());
            assert!(!controller.is_user_command_executing);
            controller.write_command(&event, ShellType::Zsh, ctx);
        });
        assert!(writes.lock().is_empty());
        {
            let mut model = model.lock();
            assert_eq!(
                model
                    .block_list()
                    .active_block()
                    .requested_command_action_id(),
                Some(&previous_action)
            );
            assert!(model
                .block_list()
                .block_for_ai_action_id(&next_action)
                .is_none());
            model.finish_block();
        }
        // The line editor becomes ready only after the preceding block has finished.
        // Dispatch the queued write at that boundary, as the Active event normally does.
        controller.update(&mut app, |controller, ctx| {
            let write = controller.pending_writes.pop_front().unwrap();
            controller.send_write_to_event_loop(write, ctx);
        });
        let model = model.lock();
        let new_block = model
            .block_list()
            .block_for_ai_action_id(&next_action)
            .unwrap();
        assert_ne!(new_block.id(), &previous_block);
        assert_eq!(new_block.ai_conversation_id(), Some(conversation_id));
        assert_eq!(
            model
                .block_list()
                .block_with_id(&previous_block)
                .unwrap()
                .requested_command_action_id(),
            Some(&previous_action)
        );
        assert_eq!(writes.lock().len(), 1);
    });
}
