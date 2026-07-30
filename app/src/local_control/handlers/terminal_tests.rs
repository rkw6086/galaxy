use ::local_control::{ActionKind, ErrorCode};
use chrono::{Local, TimeDelta};

use super::{
    elapsed_millis, ensure_active_block_is_local, ensure_interrupt_target, ensure_terminal_idle,
    ensure_terminal_input_empty, ensure_terminal_session_local, safe_command_summary,
    terminal_command_bytes, validate_block_id, validate_terminal_command, ActiveBlockSnapshot,
    MAX_COMMAND_SUMMARY_CHARS, MAX_TERMINAL_COMMAND_BYTES,
};
use crate::terminal::model::escape_sequences::C0;

fn snapshot(block_id: &str) -> ActiveBlockSnapshot {
    ActiveBlockSnapshot {
        block_id: block_id.to_owned(),
        is_executing: false,
        is_command_pending: false,
        is_long_running: false,
        is_agent_in_control: false,
        is_potential_remote_ssh: false,
        running_for_ms: None,
        command_summary: None,
    }
}

#[test]
fn execute_command_validation_rejects_empty_nul_and_oversized_input() {
    assert!(validate_terminal_command("cargo test").is_ok());

    for command in ["", "   ", "echo before\0echo after"] {
        let error = validate_terminal_command(command).expect_err("command is rejected");
        assert_eq!(error.code, ErrorCode::InvalidParams);
    }

    let oversized = "x".repeat(MAX_TERMINAL_COMMAND_BYTES + 1);
    let error = validate_terminal_command(&oversized).expect_err("oversized command is rejected");
    assert_eq!(error.code, ErrorCode::InvalidParams);
}

#[test]
fn execute_requires_an_idle_active_block() {
    let idle = snapshot("block-1");
    assert!(ensure_terminal_idle(&idle).is_ok());

    for busy in [
        ActiveBlockSnapshot {
            is_executing: true,
            ..idle.clone()
        },
        ActiveBlockSnapshot {
            is_command_pending: true,
            ..idle.clone()
        },
        ActiveBlockSnapshot {
            is_long_running: true,
            ..idle.clone()
        },
    ] {
        let error = ensure_terminal_idle(&busy).expect_err("busy terminal is rejected");
        assert_eq!(error.code, ErrorCode::TargetStateConflict);
    }
}

#[test]
fn execute_rejects_pending_user_input_without_treating_whitespace_as_empty() {
    assert!(ensure_terminal_input_empty("").is_ok());

    for pending_input in ["cargo check", " ", "\n"] {
        let error = ensure_terminal_input_empty(pending_input)
            .expect_err("pending terminal input must be preserved");
        assert_eq!(error.code, ErrorCode::TargetStateConflict);
    }
}

#[test]
fn terminal_control_requires_a_verified_local_active_session() {
    for action in [
        ActionKind::TerminalStatus,
        ActionKind::TerminalExecute,
        ActionKind::TerminalInterrupt,
    ] {
        assert!(ensure_terminal_session_local(action, Some(true)).is_ok());

        let error =
            ensure_terminal_session_local(action, Some(false)).expect_err("remote is rejected");
        assert_eq!(error.code, ErrorCode::TargetStateConflict);
        assert!(error.message.contains("active session is remote"));

        let error = ensure_terminal_session_local(action, None)
            .expect_err("an unverified session is rejected");
        assert_eq!(error.code, ErrorCode::TargetStateConflict);
        assert!(error.message.contains("locality Galaxy can verify"));
    }
}

#[test]
fn terminal_control_rejects_a_running_recognized_ssh_command() {
    let mut running_ssh = snapshot("block-ssh");
    running_ssh.is_executing = true;
    running_ssh.is_potential_remote_ssh = true;

    for action in [
        ActionKind::TerminalStatus,
        ActionKind::TerminalExecute,
        ActionKind::TerminalInterrupt,
    ] {
        let error = ensure_active_block_is_local(action, &running_ssh)
            .expect_err("running SSH is rejected");
        assert_eq!(error.code, ErrorCode::TargetStateConflict);
        assert!(error.message.contains("SSH-backed remote session"));
    }

    running_ssh.is_executing = false;
    assert!(ensure_active_block_is_local(ActionKind::TerminalExecute, &running_ssh).is_ok());
}

#[test]
fn interrupt_requires_matching_running_block() {
    let mut running = snapshot("block-2");
    running.is_executing = true;
    assert!(ensure_interrupt_target(&running, "block-2").is_ok());

    let stale =
        ensure_interrupt_target(&running, "block-1").expect_err("stale expected block is rejected");
    assert_eq!(stale.code, ErrorCode::StaleTarget);

    let idle = snapshot("block-2");
    let error =
        ensure_interrupt_target(&idle, "block-2").expect_err("idle block cannot be interrupted");
    assert_eq!(error.code, ErrorCode::TargetStateConflict);
}

#[test]
fn block_id_validation_rejects_empty_or_nul_values() {
    assert!(validate_block_id("session-1-42").is_ok());
    for block_id in ["", "bad\0id"] {
        let error = validate_block_id(block_id).expect_err("invalid block id is rejected");
        assert_eq!(error.code, ErrorCode::InvalidParams);
    }
}

#[test]
fn command_summary_is_bounded_and_omits_empty_commands() {
    assert_eq!(safe_command_summary("  "), None);
    assert_eq!(
        safe_command_summary("  cargo test  "),
        Some("cargo test".to_owned())
    );
    assert_eq!(
        safe_command_summary("printf 'one\\ntwo'\nnext"),
        Some("printf 'one\\ntwo' next".to_owned())
    );

    let command = "x".repeat(MAX_COMMAND_SUMMARY_CHARS + 1);
    let summary = safe_command_summary(&command).expect("non-empty summary");
    assert_eq!(summary.chars().count(), MAX_COMMAND_SUMMARY_CHARS + 1);
    assert!(summary.ends_with('…'));
}

#[test]
fn running_duration_is_present_only_for_an_active_command() {
    let now = Local::now();
    let started_at = now - TimeDelta::seconds(75);

    assert_eq!(elapsed_millis(Some(&started_at), now, true), Some(75_000));
    assert_eq!(elapsed_millis(Some(&started_at), now, false), None);
    assert_eq!(elapsed_millis(None, now, true), None);
}

#[test]
fn execute_appends_a_terminal_enter_sequence() {
    assert_eq!(
        terminal_command_bytes("cargo test".to_owned()),
        [b"cargo test".as_slice(), &[C0::CR]].concat()
    );
}
