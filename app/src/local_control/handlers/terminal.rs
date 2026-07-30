//! Race-safe control of commands in existing visible terminal sessions.
#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;

use ::local_control::protocol::{
    TargetSelector, TerminalExecuteParams, TerminalInterruptParams, TerminalStatusResult,
};
use ::local_control::remote_command::is_potential_remote_ssh_command;
use ::local_control::{ActionKind, ControlError, ErrorCode, InstanceId};
use chrono::{DateTime, Local};
use serde_json::json;
use warpui::ModelContext;

use crate::ai::agent::redaction::redact_secrets;
use crate::local_control::resolver::{decode_params, target_pane_group, target_session_pane_id};
use crate::local_control::LocalControlBridge;
use crate::terminal::model::escape_sequences::C0;
use crate::terminal::view::TerminalView;

const MAX_TERMINAL_COMMAND_BYTES: usize = 64 * 1024;
const MAX_COMMAND_SUMMARY_CHARS: usize = 1_024;
const MAX_BLOCK_ID_BYTES: usize = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveBlockSnapshot {
    block_id: String,
    is_executing: bool,
    is_command_pending: bool,
    is_long_running: bool,
    is_agent_in_control: bool,
    is_potential_remote_ssh: bool,
    running_for_ms: Option<u64>,
    command_summary: Option<String>,
}

impl ActiveBlockSnapshot {
    fn is_idle(&self) -> bool {
        !self.is_executing && !self.is_command_pending && !self.is_long_running
    }

    fn has_running_command(&self) -> bool {
        self.is_executing || self.is_command_pending || self.is_long_running
    }
}

pub(crate) fn handle(
    instance_id: &Option<InstanceId>,
    action: ActionKind,
    params: &serde_json::Value,
    target: &TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    match action {
        ActionKind::TerminalStatus => terminal_status(target, ctx),
        ActionKind::TerminalExecute => {
            let TerminalExecuteParams { command } = decode_params(params)?;
            validate_terminal_command(&command)?;
            terminal_execute(instance_id, target, command, ctx)
        }
        ActionKind::TerminalInterrupt => {
            let TerminalInterruptParams { block_id } = decode_params(params)?;
            validate_block_id(&block_id)?;
            terminal_interrupt(instance_id, target, block_id, ctx)
        }
        _ => Err(ControlError::new(
            ErrorCode::UnsupportedAction,
            format!("{} is not a terminal control action", action.as_str()),
        )),
    }
}

fn terminal_status(
    target: &TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let (session_id, terminal_view) = resolve_terminal(ActionKind::TerminalStatus, target, ctx)?;
    let snapshot = terminal_view.read(ctx, |terminal_view, ctx| {
        ensure_terminal_session_local(
            ActionKind::TerminalStatus,
            terminal_view.active_session_is_local(ctx),
        )?;
        let snapshot = active_block_snapshot(terminal_view);
        ensure_active_block_is_local(ActionKind::TerminalStatus, &snapshot)?;
        Ok(snapshot)
    })?;
    let is_idle = snapshot.is_idle();
    serde_json::to_value(TerminalStatusResult {
        action: ActionKind::TerminalStatus,
        session_id,
        active_block_id: snapshot.block_id,
        is_executing: snapshot.is_executing,
        is_command_pending: snapshot.is_command_pending,
        is_long_running: snapshot.is_long_running,
        is_agent_in_control: snapshot.is_agent_in_control,
        is_idle,
        running_for_ms: snapshot.running_for_ms,
        command_summary: snapshot.command_summary,
    })
    .map_err(|error| {
        ControlError::with_details(
            ErrorCode::Internal,
            "failed to serialize terminal status",
            error.to_string(),
        )
    })
}

fn terminal_execute(
    instance_id: &Option<InstanceId>,
    target: &TargetSelector,
    command: String,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let (session_id, terminal_view) = resolve_terminal(ActionKind::TerminalExecute, target, ctx)?;
    terminal_view.update(ctx, |terminal_view, ctx| {
        ensure_terminal_session_local(
            ActionKind::TerminalExecute,
            terminal_view.active_session_is_local(ctx),
        )?;
        let snapshot = active_block_snapshot(terminal_view);
        ensure_active_block_is_local(ActionKind::TerminalExecute, &snapshot)?;
        ensure_terminal_idle(&snapshot)?;
        let pending_input = terminal_view
            .input()
            .read(ctx, |input, ctx| input.buffer_text(ctx));
        ensure_terminal_input_empty(&pending_input)?;

        terminal_view.write_to_pty(terminal_command_bytes(command), ctx);

        Ok(json!({
            "action": ActionKind::TerminalExecute.as_str(),
            "ok": true,
            "instance_id": instance_id.as_ref().map(|id| id.0.as_str()),
            "session_id": session_id,
            "previous_block_id": snapshot.block_id,
        }))
    })
}

fn terminal_interrupt(
    instance_id: &Option<InstanceId>,
    target: &TargetSelector,
    expected_block_id: String,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<serde_json::Value, ControlError> {
    let (session_id, terminal_view) = resolve_terminal(ActionKind::TerminalInterrupt, target, ctx)?;
    terminal_view.update(ctx, |terminal_view, ctx| {
        ensure_terminal_session_local(
            ActionKind::TerminalInterrupt,
            terminal_view.active_session_is_local(ctx),
        )?;
        let snapshot = active_block_snapshot(terminal_view);
        ensure_active_block_is_local(ActionKind::TerminalInterrupt, &snapshot)?;
        ensure_interrupt_target(&snapshot, &expected_block_id)?;
        terminal_view.write_to_pty(vec![C0::ETX], ctx);

        Ok(json!({
            "action": ActionKind::TerminalInterrupt.as_str(),
            "ok": true,
            "instance_id": instance_id.as_ref().map(|id| id.0.as_str()),
            "session_id": session_id,
            "block_id": snapshot.block_id,
        }))
    })
}

fn resolve_terminal(
    action: ActionKind,
    target: &TargetSelector,
    ctx: &mut ModelContext<LocalControlBridge>,
) -> Result<(String, warpui::ViewHandle<TerminalView>), ControlError> {
    let pane_group = target_pane_group(action, target, ctx)?;
    let pane_id = target_session_pane_id(action, target, &pane_group, ctx)?;
    let terminal_view = pane_group
        .read(ctx, |pane_group, ctx| {
            pane_group.terminal_view_from_pane_id(pane_id, ctx)
        })
        .ok_or_else(|| {
            ControlError::new(
                ErrorCode::MissingTarget,
                format!("{} requires an existing terminal session", action.as_str()),
            )
        })?;
    Ok((pane_id.to_string(), terminal_view))
}

fn active_block_snapshot(terminal_view: &TerminalView) -> ActiveBlockSnapshot {
    let model = terminal_view.model.lock();
    let active_block = model.block_list().active_block();
    let is_executing = active_block.is_executing();
    let is_command_pending = active_block.is_command_grid_active();
    let is_long_running = active_block.is_active_and_long_running();
    let mut command = active_block.command_with_secrets_obfuscated(false);
    let is_potential_remote_ssh = is_potential_remote_ssh_command(&command);
    redact_secrets(&mut command);
    ActiveBlockSnapshot {
        block_id: active_block.id().to_string(),
        is_executing,
        is_command_pending,
        is_long_running,
        is_agent_in_control: active_block.is_agent_in_control(),
        is_potential_remote_ssh,
        running_for_ms: elapsed_millis(
            active_block.start_ts(),
            Local::now(),
            is_executing || is_command_pending || is_long_running,
        ),
        command_summary: safe_command_summary(&command),
    }
}

fn elapsed_millis(
    started_at: Option<&DateTime<Local>>,
    now: DateTime<Local>,
    is_running: bool,
) -> Option<u64> {
    if !is_running {
        return None;
    }
    started_at.map(|started_at| {
        now.signed_duration_since(started_at)
            .num_milliseconds()
            .max(0) as u64
    })
}

fn validate_terminal_command(command: &str) -> Result<(), ControlError> {
    if command.trim().is_empty() {
        return Err(ControlError::new(
            ErrorCode::InvalidParams,
            "terminal.execute requires a non-empty command",
        ));
    }
    if command.as_bytes().contains(&0) {
        return Err(ControlError::new(
            ErrorCode::InvalidParams,
            "terminal.execute rejects NUL bytes",
        ));
    }
    if command.len() > MAX_TERMINAL_COMMAND_BYTES {
        return Err(ControlError::new(
            ErrorCode::InvalidParams,
            format!("terminal.execute command exceeds the {MAX_TERMINAL_COMMAND_BYTES}-byte limit"),
        ));
    }
    Ok(())
}

fn terminal_command_bytes(command: String) -> Vec<u8> {
    let mut bytes = command.into_bytes();
    bytes.push(C0::CR);
    bytes
}

fn validate_block_id(block_id: &str) -> Result<(), ControlError> {
    if block_id.is_empty()
        || block_id.len() > MAX_BLOCK_ID_BYTES
        || block_id.as_bytes().contains(&0)
    {
        return Err(ControlError::new(
            ErrorCode::InvalidParams,
            "terminal.interrupt requires a valid non-empty active block_id",
        ));
    }
    Ok(())
}

fn ensure_terminal_idle(snapshot: &ActiveBlockSnapshot) -> Result<(), ControlError> {
    if snapshot.is_idle() {
        return Ok(());
    }
    Err(ControlError::new(
        ErrorCode::TargetStateConflict,
        format!(
            "terminal.execute requires an idle terminal; active block {} is still running",
            snapshot.block_id
        ),
    ))
}

fn ensure_terminal_input_empty(input: &str) -> Result<(), ControlError> {
    if input.is_empty() {
        return Ok(());
    }
    Err(ControlError::new(
        ErrorCode::TargetStateConflict,
        "terminal.execute will not overwrite pending user input; clear or submit the terminal input first",
    ))
}

fn ensure_active_block_is_local(
    action: ActionKind,
    snapshot: &ActiveBlockSnapshot,
) -> Result<(), ControlError> {
    if !snapshot.has_running_command() || !snapshot.is_potential_remote_ssh {
        return Ok(());
    }
    Err(ControlError::new(
        ErrorCode::TargetStateConflict,
        format!(
            "{} is unavailable because the target terminal's active command may be an SSH-backed remote session",
            action.as_str()
        ),
    ))
}

fn ensure_terminal_session_local(
    action: ActionKind,
    active_session_is_local: Option<bool>,
) -> Result<(), ControlError> {
    match active_session_is_local {
        Some(true) => Ok(()),
        Some(false) => Err(ControlError::new(
            ErrorCode::TargetStateConflict,
            format!(
                "{} is unavailable because the target terminal's active session is remote",
                action.as_str()
            ),
        )),
        None => Err(ControlError::new(
            ErrorCode::TargetStateConflict,
            format!(
                "{} requires an active terminal session whose locality Galaxy can verify",
                action.as_str()
            ),
        )),
    }
}

fn ensure_interrupt_target(
    snapshot: &ActiveBlockSnapshot,
    expected_block_id: &str,
) -> Result<(), ControlError> {
    if snapshot.block_id != expected_block_id {
        return Err(ControlError::new(
            ErrorCode::StaleTarget,
            format!(
                "terminal.interrupt expected block {expected_block_id}, but the active block is {}",
                snapshot.block_id
            ),
        ));
    }
    if !snapshot.has_running_command() {
        return Err(ControlError::new(
            ErrorCode::TargetStateConflict,
            format!(
                "terminal.interrupt block {} is not executing",
                snapshot.block_id
            ),
        ));
    }
    Ok(())
}

fn safe_command_summary(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }

    let mut summary = command
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(MAX_COMMAND_SUMMARY_CHARS)
        .collect::<String>();
    if command.chars().count() > MAX_COMMAND_SUMMARY_CHARS {
        summary.push('…');
    }
    Some(summary)
}
