use std::path::{Path, PathBuf};

use chrono::Utc;
use galaxy_core::HostId;
use uuid::Uuid;

use super::{job_matches_scope, JobScopeFilter};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::AIAgentActionId;
use crate::ai::background_jobs::{BackgroundJob, BackgroundJobStatus};

fn job(directory: &str, project: Option<&str>) -> BackgroundJob {
    BackgroundJob {
        id: Uuid::new_v4(),
        conversation_id: AIConversationId::new(),
        action_id: AIAgentActionId::from("background-job-test".to_owned()),
        command: "cargo build".to_owned(),
        working_directory: PathBuf::from(directory),
        project_root: project.map(PathBuf::from),
        host_id: None,
        started_at: Utc::now(),
        finished_at: None,
        status: BackgroundJobStatus::Running,
        exit_code: None,
        error: None,
        completion_delivered: false,
        log_path: PathBuf::new(),
        output: String::new(),
        output_truncated: false,
        log_bytes: 0,
    }
}

#[test]
fn project_history_includes_sibling_directories_but_keeps_hosts_separate() {
    let root = Path::new("/project");
    let cwd = Path::new("/project/app");
    let sibling = job("/project/server", Some("/project"));
    assert!(job_matches_scope(
        &sibling,
        JobScopeFilter::Project,
        Some(cwd),
        Some(root),
        None
    ));
    assert!(!job_matches_scope(
        &sibling,
        JobScopeFilter::Directory,
        Some(cwd),
        Some(root),
        None
    ));
    assert!(!job_matches_scope(
        &job("/other", Some("/other")),
        JobScopeFilter::Project,
        Some(cwd),
        Some(root),
        None
    ));

    let remote = BackgroundJob {
        host_id: Some(HostId::new("remote-host".to_owned())),
        ..sibling
    };
    assert!(!job_matches_scope(
        &remote,
        JobScopeFilter::Project,
        Some(cwd),
        Some(root),
        None
    ));
    assert!(job_matches_scope(
        &remote,
        JobScopeFilter::Project,
        Some(cwd),
        Some(root),
        remote.host_id.as_ref()
    ));
    assert!(job_matches_scope(
        &remote,
        JobScopeFilter::All,
        Some(cwd),
        Some(root),
        None
    ));
}

#[test]
fn directory_history_is_exact_when_no_git_project_exists() {
    let cwd = Path::new("/scratch");
    assert!(job_matches_scope(
        &job("/scratch", None),
        JobScopeFilter::Directory,
        Some(cwd),
        None,
        None
    ));
    assert!(!job_matches_scope(
        &job("/scratch/other", None),
        JobScopeFilter::Directory,
        Some(cwd),
        None,
        None
    ));
    assert!(!job_matches_scope(
        &job("/scratch", None),
        JobScopeFilter::Project,
        Some(cwd),
        None,
        None
    ));
}
