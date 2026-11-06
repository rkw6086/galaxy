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

fn cancellation_fixture(status: BackgroundJobStatus) -> BackgroundJob {
    BackgroundJob {
        id: uuid::Uuid::new_v4(),
        conversation_id: crate::ai::agent::conversation::AIConversationId::new(),
        action_id: "cancellation-test-action".to_owned().into(),
        command: "already completed command".to_owned(),
        working_directory: std::path::PathBuf::new(),
        project_root: None,
        host_id: None,
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        status,
        exit_code: Some(17),
        error: Some("original outcome".to_owned()),
        completion_delivered: true,
        log_path: std::path::PathBuf::new(),
        output: "original output".to_owned(),
        output_truncated: false,
        log_bytes: 15,
    }
}

#[test]
fn cancelling_a_completed_job_preserves_its_outcome_without_signalling_or_redelivery() {
    galaxyui::App::test((), |mut app| async move {
        let jobs = app.add_model(super::BackgroundJobsModel::new);
        for status in [
            BackgroundJobStatus::Succeeded,
            BackgroundJobStatus::Failed,
            BackgroundJobStatus::Cancelled,
        ] {
            jobs.update(&mut app, |jobs, ctx| {
                let job = cancellation_fixture(status);
                let id = job.id;
                let owner = job.conversation_id;
                let finished_at = job.finished_at;
                let (cancel, cancellations) = async_channel::bounded(1);
                jobs.jobs.insert(id, job);
                // Even a stale remote cancellation handle must not be used.
                jobs.remote_cancellations.insert(id, cancel);
                assert_eq!(jobs.cancel(id, ctx), Ok(()));
                assert_eq!(jobs.cancel(id, ctx), Ok(()));
                assert_eq!(
                    cancellations.try_recv(),
                    Err(async_channel::TryRecvError::Empty)
                );
                let job = jobs.job(id).unwrap();
                assert_eq!(job.status, status);
                assert_eq!(job.exit_code, Some(17));
                assert_eq!(job.finished_at, finished_at);
                assert_eq!(job.error.as_deref(), Some("original outcome"));
                assert_eq!(job.output, "original output");
                assert_eq!(job.completion_delivered, true);
                assert_eq!(jobs.pending_completions(owner).len(), 0);
                assert_eq!(jobs.unpublished_completions.contains(&id), false);
            });
        }
    });
}

#[test]
fn cancellation_still_rejects_unknown_and_unowned_running_processes() {
    galaxyui::App::test((), |mut app| async move {
        let jobs = app.add_model(super::BackgroundJobsModel::new);
        jobs.update(&mut app, |jobs, ctx| {
            let unknown = uuid::Uuid::new_v4();
            let (cancel, cancellations) = async_channel::bounded(1);
            jobs.remote_cancellations.insert(unknown, cancel);
            assert_eq!(jobs.cancel(unknown, ctx), Err("Background job not found".to_owned()));
            assert_eq!(cancellations.try_recv(), Err(async_channel::TryRecvError::Empty));

            let mut job = cancellation_fixture(BackgroundJobStatus::Running);
            job.finished_at = None;
            let id = job.id;
            jobs.jobs.insert(id, job);
            // A known Running record alone never authorizes signalling a PID.
            assert_eq!(jobs.cancel(id, ctx), Err("This background job is no longer running".to_owned()));

            jobs.unreconciled_remote_jobs.insert(id);
            assert_eq!(jobs.cancel(id, ctx), Err("Waiting to reconnect and confirm this job's state on its host before cancellation is available".to_owned()));
            assert_eq!(jobs.job(id).unwrap().status, BackgroundJobStatus::Running);
        });
    });
}
