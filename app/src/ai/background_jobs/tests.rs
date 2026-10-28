use std::fs;

use chrono::Utc;

use super::{load_history, read_tail, BackgroundJob, BackgroundJobStatus, SavedHistory};

#[test]
fn history_restore_cancels_unfinished_records_without_restoring_completion_work() {
    let root = tempfile::tempdir().unwrap();
    let job = BackgroundJob {
        id: uuid::Uuid::new_v4(),
        conversation_id: crate::ai::agent::conversation::AIConversationId::new(),
        action_id: "job-test-action".to_owned().into(),
        command: "echo hello".to_owned(),
        working_directory: root.path().to_path_buf(),
        project_root: None,
        host_id: None,
        started_at: Utc::now(),
        finished_at: None,
        status: BackgroundJobStatus::Running,
        exit_code: None,
        error: None,
        completion_delivered: false,
        log_path: root.path().join("untrusted-log-path"),
        output: "not persisted".to_owned(),
        output_truncated: false,
        log_bytes: 0,
    };
    let id = job.id;
    fs::write(
        root.path().join("history.json"),
        serde_json::to_vec(&SavedHistory {
            version: 1,
            jobs: vec![job],
        })
        .unwrap(),
    )
    .unwrap();
    let (loaded, interrupted, error) = load_history(root.path(), &root.path().join("logs"));
    assert!(error.is_none());
    let restored = &loaded[&id];
    assert_eq!(restored.status, BackgroundJobStatus::Cancelled);
    assert!(restored.completion_delivered);
    assert!(interrupted.contains(&restored.conversation_id));
    assert!(restored.output.is_empty());
    assert_eq!(
        restored.log_path,
        root.path().join("logs").join(format!("{id}.log"))
    );
}

#[test]
fn remote_history_preserves_unknown_process_state_until_its_host_reconnects() {
    let root = tempfile::tempdir().unwrap();
    let logs = root.path().join("logs");
    fs::create_dir(&logs).unwrap();
    let id = uuid::Uuid::new_v4();
    fs::write(logs.join(format!("{id}.log")), "previous output\n").unwrap();
    let conversation_id = crate::ai::agent::conversation::AIConversationId::new();
    let job = BackgroundJob {
        id,
        conversation_id,
        action_id: "remote-job-test-action".to_owned().into(),
        command: "make build".to_owned(),
        working_directory: "/remote/project".into(),
        project_root: None,
        host_id: Some(galaxy_util::host_id::HostId::new("test-host".to_owned())),
        started_at: Utc::now(),
        finished_at: None,
        status: BackgroundJobStatus::Running,
        exit_code: None,
        error: None,
        completion_delivered: false,
        log_path: logs.join(format!("{id}.log")),
        output: String::new(),
        output_truncated: false,
        log_bytes: 0,
    };
    fs::write(
        root.path().join("history.json"),
        serde_json::to_vec(&SavedHistory {
            version: 1,
            jobs: vec![job],
        })
        .unwrap(),
    )
    .unwrap();

    let (loaded, interrupted, error) = load_history(root.path(), &logs);
    assert!(error.is_none());
    assert!(!interrupted.contains(&conversation_id));
    let restored = &loaded[&id];
    assert_eq!(restored.status, BackgroundJobStatus::Running);
    assert!(restored.finished_at.is_none());
    assert!(restored.completion_delivered);
    assert!(restored.error.as_deref().unwrap().contains("reconnect"));
    assert_eq!(restored.output, "previous output\n");
    assert_eq!(restored.log_bytes, 16);
}

#[test]
fn log_tail_is_bounded_and_reports_the_full_byte_count() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("job.log");
    fs::write(&path, b"prefix\nlast output").unwrap();
    let (tail, size) = read_tail(&path, 11).unwrap();
    assert_eq!(tail, "last output");
    assert_eq!(size, 18);
}

#[cfg(unix)]
#[test]
fn completion_capture_keeps_output_written_after_temporary_log_is_removed() {
    use std::io::Write;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("job.log");
    let mut writer = super::create_private_log(&path).unwrap();
    let mut reader = super::open_log_reader(&path).unwrap();
    writer.write_all(b"before\n").unwrap();
    assert_eq!(
        super::read_tail_from_open_file(&mut reader, 64).unwrap(),
        ("before\n".to_owned(), 7)
    );
    fs::remove_file(&path).unwrap();
    writer.write_all(b"after\n").unwrap();
    assert!(read_tail(&path, 64).is_err());
    assert_eq!(
        super::read_tail_from_open_file(&mut reader, 64).unwrap(),
        ("before\nafter\n".to_owned(), 13)
    );
}

#[test]
fn corrupt_history_is_reported_instead_of_replaced() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("history.json"), b"broken").unwrap();
    let (jobs, interrupted, error) = load_history(root.path(), root.path());
    assert!(jobs.is_empty());
    assert!(interrupted.is_empty());
    assert!(error.is_some());
    assert_eq!(
        fs::read(root.path().join("history.json")).unwrap(),
        b"broken"
    );
}
