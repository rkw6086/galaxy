//! Host-scoped process transport. Network polling belongs to Galaxy, never to a model turn.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use galaxy_core::SessionId;
use galaxy_util::host_id::HostId;
use galaxyui::r#async::Timer;
use galaxyui::{ModelContext, SingletonEntity};
use remote_server::manager::{HostRequestHandle, RemoteServerManager};
use remote_server::proto::background_job_request::Operation;
use remote_server::proto::{BackgroundJobRequest, BackgroundJobResponse};
use uuid::Uuid;

use super::{
    BackgroundJob, BackgroundJobLaunch, BackgroundJobStatus, BackgroundJobsEvent,
    BackgroundJobsModel,
};

pub(crate) fn start(
    transport: Arc<HostRequestHandle>,
    launch: BackgroundJobLaunch,
    session_id: SessionId,
    model: &mut BackgroundJobsModel,
    ctx: &mut ModelContext<BackgroundJobsModel>,
) -> Result<Uuid, String> {
    let host_id = launch
        .host_id
        .clone()
        .ok_or("A remote background job requires a host identity")?;
    if launch.command.trim().is_empty()
        || launch.command.contains('\0')
        || launch.working_directory.as_os_str().is_empty()
    {
        return Err("A remote job requires a nonempty command and working directory".to_owned());
    }
    let id = Uuid::new_v4();
    let request = BackgroundJobRequest {
        operation: Operation::Start as i32,
        job_id: id.to_string(),
        conversation_id: launch.conversation_id.to_string(),
        action_id: launch.action_id.to_string(),
        command: launch.command.clone(),
        working_directory: launch.working_directory.to_string_lossy().into_owned(),
        has_environment: launch.environment.is_some(),
        environment: launch.environment.unwrap_or_default(),
        session_id: session_id.as_u64(),
        ..Default::default()
    };
    let job = BackgroundJob {
        id,
        conversation_id: launch.conversation_id,
        action_id: launch.action_id,
        command: launch.command,
        working_directory: launch.working_directory,
        project_root: None,
        host_id: Some(host_id),
        started_at: Utc::now(),
        finished_at: None,
        status: BackgroundJobStatus::Running,
        exit_code: None,
        error: Some("Waiting for the remote host to confirm this job's launch".to_owned()),
        completion_delivered: false,
        log_path: Default::default(),
        output: String::new(),
        output_truncated: false,
        log_bytes: 0,
    };
    let (cancel_tx, cancel_rx) = async_channel::bounded(1);
    // Persist the identity before external work. The model owns this task independently of
    // the requesting action future, so subsequent user input cannot orphan an accepted Start.
    model.register_remote_job(job.clone(), cancel_tx, ctx)?;
    monitor(job, transport, cancel_rx, 0, Some(request), ctx);
    Ok(id)
}

/// Reconcile daemon-owned processes without launching commands or resuming conversations.
/// A restored terminal snapshot only updates history; a genuinely running snapshot restores
/// live completion ownership for a later terminal event.
pub(crate) fn reattach_host(
    host_id: &HostId,
    model: &mut BackgroundJobsModel,
    ctx: &mut ModelContext<BackgroundJobsModel>,
) {
    let jobs = model
        .records()
        .filter(|job| {
            job.host_id.as_ref() == Some(host_id)
                && !job.status.is_terminal()
                && !model.remote_cancellations.contains_key(&job.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    if jobs.is_empty() {
        return;
    }
    let manager = RemoteServerManager::as_ref(ctx);
    if !manager
        .client_for_host(host_id)
        .is_some_and(|client| client.supports_background_jobs())
    {
        for job in jobs {
            if let Some(job) = model.jobs.get_mut(&job.id) {
                job.error = Some("Reconnect with an updated Galaxy remote server to reconcile this job. Its remote process outcome has not been confirmed.".to_owned());
            }
        }
        ctx.emit(BackgroundJobsEvent::Changed);
        return;
    }
    let transport = Arc::new(manager.host_request_handle(host_id));
    for job in jobs {
        let (cancel_tx, cancel_rx) = async_channel::bounded(1);
        let cursor = job.log_bytes;
        match model.reattach_remote_job(job.id, cancel_tx, ctx) {
            Ok(()) => monitor(job, Arc::clone(&transport), cancel_rx, cursor, None, ctx),
            Err(error) => {
                log::warn!(
                    "Could not reattach remote background job {}: {error}",
                    job.id
                );
            }
        }
    }
}

fn monitor(
    job: BackgroundJob,
    transport: Arc<HostRequestHandle>,
    cancel_rx: async_channel::Receiver<()>,
    mut cursor: u64,
    mut launch_request: Option<BackgroundJobRequest>,
    ctx: &mut ModelContext<BackgroundJobsModel>,
) {
    let id = job.id;
    let conversation_id = job.conversation_id;
    let action_id = job.action_id.clone();
    let foreground = ctx.spawner();
    ctx.spawn(async move {
        let mut cancellation_pending = false;
        let mut launch_sent = false;
        let mut launch_attempts = 0;
        loop {
            if cancel_rx.try_recv().is_ok() {
                cancellation_pending = true;
            }
            if cancellation_pending && launch_request.is_some() && !launch_sent {
                let _ = foreground.spawn(move |model, ctx| {
                    model.remote_cancellations.remove(&id);
                    model.finish(id, super::runtime::JobOutcome {
                        status: BackgroundJobStatus::Cancelled,
                        exit_code: None,
                        output: String::new(),
                        log_bytes: 0,
                        error: Some("Cancelled before the remote launch request was sent".to_owned()),
                    }, ctx);
                }).await;
                return;
            }
            let mut request = BackgroundJobRequest {
                operation: if cancellation_pending { Operation::Cancel } else { Operation::Status } as i32,
                job_id: id.to_string(),
                conversation_id: conversation_id.to_string(),
                action_id: action_id.to_string(),
                command: job.command.clone(),
                working_directory: job.working_directory.to_string_lossy().into_owned(),
                log_offset: cursor,
                ..Default::default()
            };
            if !cancellation_pending && launch_attempts < 3 {
                if let Some(start) = &launch_request {
                    request = start.clone();
                    launch_sent = true;
                    launch_attempts += 1;
                }
            }
            let response = transport.background_job(request);
            let cancellation = cancel_rx.recv();
            futures::pin_mut!(response, cancellation);
            // A lost Start response can wait for the host timeout. Cancellation must use
            // the durable job identity immediately rather than waiting for that response.
            let response = match futures::future::select(response, cancellation).await {
                futures::future::Either::Left((response, _)) => response,
                futures::future::Either::Right((Ok(()), _)) => {
                    cancellation_pending = true;
                    continue;
                }
                futures::future::Either::Right((Err(_), response)) => response.await,
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    // A dropped SSH connection does not prove the remote process stopped.
                    // Keep ownership and retry through the manager's host connection pool.
                    let message = format!("Waiting to reconnect and confirm this job's actual state: {error}");
                    if !record_unconfirmed(&foreground, id, message).await { return; }
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
            };
            if response.job_not_found {
                // A bounded Start retry has no confirmed result. Seal cancellation on the
                // daemon before declaring an outcome; its tombstone also defeats a late Start.
                if launch_request.is_some() { cancellation_pending = true; }
                if !record_unconfirmed(&foreground, id, "The host has no recorded process for this job; its outcome has not been confirmed".to_owned()).await { return; }
                Timer::after(Duration::from_millis(500)).await;
                continue;
            }
            if !response.error.is_empty() {
                if !record_unconfirmed(&foreground, id, format!("Remote job state could not be confirmed: {}", response.error)).await { return; }
                Timer::after(Duration::from_millis(500)).await;
                continue;
            }
            let remote = match parse_response(&response) {
                Ok(remote) if remote.id == id && remote.conversation_id == conversation_id
                    && remote.action_id == action_id && remote.command == job.command => remote,
                Ok(_) => {
                    if !record_unconfirmed(&foreground, id, "Remote job status identity changed; no outcome has been accepted".to_owned()).await { return; }
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
                Err(error) => {
                    if !record_unconfirmed(&foreground, id, error).await { return; }
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
            };
            launch_request = None;
            cancellation_pending = false;
            let mut error = remote.error.clone();
            let chunk = if response.next_log_offset >= cursor
                && response.next_log_offset - cursor == response.log_chunk.len() as u64
            {
                response.log_chunk
            } else {
                error = Some("The remote log was truncated while this job was running; previously captured output is retained.".to_owned());
                Vec::new()
            };
            cursor = response.next_log_offset;
            if !response.logs_available {
                let detail = if response.completion_log_tail_available {
                    "The remote temporary log has been purged; only retained bounded completion evidence and previously captured output remain."
                } else {
                    "No remote temporary log is available; previously captured output is retained."
                };
                error = Some(match error {
                    Some(error) => format!("{error} {detail}"),
                    None => detail.to_owned(),
                });
            }
            let drained = !response.logs_available || cursor >= response.total_log_bytes;
            let terminal = remote.status.is_terminal() && drained;
            let status = if terminal { remote.status } else { BackgroundJobStatus::Running };
            let exit_code = terminal.then_some(remote.exit_code).flatten();
            let available = response.logs_available;
            let completion_tail = (terminal && !available && response.completion_log_tail_available)
                .then(|| (bounded_output(&response.completion_log_tail), response.completion_log_bytes));
            let confirmed_running = remote.status == BackgroundJobStatus::Running;
            let applied = foreground.spawn(move |model, ctx| {
                if confirmed_running {
                    model.arm_remote_completion(id, ctx)?;
                }
                let metadata_changed = model.jobs.get_mut(&id).is_some_and(|job| {
                    let changed = job.working_directory != remote.working_directory || job.project_root != remote.project_root;
                    job.working_directory = remote.working_directory;
                    job.project_root = remote.project_root;
                    changed
                });
                if metadata_changed {
                    if let Err(error) = model.persist() {
                        log::warn!("Could not persist confirmed remote job scope: {error}");
                    }
                }
                let result = if let Some((output, log_bytes)) = completion_tail {
                    model.remote_cancellations.remove(&id);
                    model.finish(id, super::runtime::JobOutcome { status, exit_code, error, output, log_bytes }, ctx);
                    Ok(())
                } else {
                    model.update_remote_job(id, status, exit_code, error, &chunk, ctx)
                };
                // Do not offer an empty placeholder as a log when the source was already purged.
                if !available && model.job(id).is_some_and(|job| std::fs::symlink_metadata(&job.log_path).is_ok_and(|metadata| metadata.file_type().is_file() && metadata.len() == 0)) {
                    if let Some(job) = model.job(id) {
                        let _ = std::fs::remove_file(&job.log_path);
                    }
                    ctx.emit(BackgroundJobsEvent::Changed);
                }
                result
            }).await;
            match applied {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if !record_unconfirmed(&foreground, id, error).await { return; }
                    Timer::after(Duration::from_secs(1)).await;
                    continue;
                }
                Err(_) => return,
            }
            if terminal { return; }
            // Drain a completed job's remaining bytes immediately, without a per-chunk delay.
            if drained { Timer::after(Duration::from_millis(500)).await; }
        }
    }, |_, _, _| {});
}

fn parse_response(response: &BackgroundJobResponse) -> Result<BackgroundJob, String> {
    if !response.error.is_empty() {
        return Err(response.error.clone());
    }
    serde_json::from_str(&response.job_json)
        .map_err(|error| format!("Invalid remote background job response: {error}"))
}

async fn record_unconfirmed(
    foreground: &galaxyui::ModelSpawner<BackgroundJobsModel>,
    id: Uuid,
    error: String,
) -> bool {
    foreground
        .spawn(move |model, ctx| {
            if let Some(job) = model.jobs.get_mut(&id) {
                job.error = Some(error);
                ctx.emit(BackgroundJobsEvent::Changed);
            }
        })
        .await
        .is_ok()
}

fn bounded_output(bytes: &[u8]) -> String {
    let output = String::from_utf8_lossy(bytes);
    let mut start = output.len().saturating_sub(super::MAX_OUTPUT_BYTES);
    while !output.is_char_boundary(start) {
        start += 1;
    }
    output[start..].to_owned()
}
