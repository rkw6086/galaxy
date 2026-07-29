use std::time::Duration;

use galaxy_terminal::model::escape_sequences;

use super::{get_action_icon, get_action_loading_text, should_retain_task_output_message};
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionType, AIAgentOutputMessageType,
    AIAgentPtyWriteMode, AIAgentText, ShellCommandDelay,
};
use crate::terminal::model::block::BlockId;
use crate::ui_components::icons::Icon;

#[test]
fn command_output_poll_has_visible_monitor_status() {
    let action = AIAgentActionType::ReadShellCommandOutput {
        block_id: BlockId::new(),
        delay: Some(ShellCommandDelay::Duration(Duration::from_secs(2))),
    };

    assert_eq!(
        get_action_loading_text(action.clone()).as_deref(),
        Some("Checking the running command output…")
    );
    assert_eq!(get_action_icon(action), Some(Icon::ClockRefresh));
}

#[test]
fn typed_interrupt_has_distinct_visible_status() {
    let action = AIAgentActionType::WriteToLongRunningShellCommand {
        block_id: BlockId::new(),
        input: vec![escape_sequences::C0::ETX].into(),
        mode: AIAgentPtyWriteMode::Raw,
    };

    assert!(action.is_shell_command_interrupt());
    assert_eq!(
        get_action_loading_text(action.clone()).as_deref(),
        Some("Interrupting the running command with Ctrl+C…")
    );
    assert_eq!(get_action_icon(action), Some(Icon::Stop));
}

#[test]
fn transcript_retains_prior_text_but_only_latest_tool_activity() {
    let text = AIAgentOutputMessageType::Text(AIAgentText { sections: vec![] });
    assert!(should_retain_task_output_message(&text, false));

    let poll = AIAgentOutputMessageType::Action(AIAgentAction {
        id: AIAgentActionId::from("poll".to_string()),
        task_id: TaskId::new("cli-task".to_string()),
        action: AIAgentActionType::ReadShellCommandOutput {
            block_id: BlockId::new(),
            delay: None,
        },
        requires_result: true,
        tool_name: Some("read_shell_command_output".to_string()),
    });
    assert!(!should_retain_task_output_message(&poll, false));
    assert!(should_retain_task_output_message(&poll, true));
}
