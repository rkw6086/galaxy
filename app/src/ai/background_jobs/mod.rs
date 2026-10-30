//! Agent-requested commands run independently of a conversation's current model turn.

mod log_format;
pub(crate) mod remote;
mod runtime;
pub mod view;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use galaxy_util::host_id::HostId;
use galaxyui::r#async::Timer;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::agent::conversation::AIConversationId;
use crate::ai::agent::AIAgentActionId;
use crate::terminal::shell::ShellLaunchData;

pub type BackgroundJobId = Uuid;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_LOG_VIEW_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackgroundJobStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl BackgroundJobStatus {
    pub fn is_terminal(self) -> bool {
        self != Self::Running
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackgroundJob {
    pub id: BackgroundJobId,
    pub conversation_id: AIConversationId,
    pub action_id: AIAgentActionId,
    pub command: String,
    pub working_directory: PathBuf,
    pub project_root: Option<PathBuf>,
    pub host_id: Option<HostId>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: BackgroundJobStatus,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub completion_delivered: bool,
    /// Full logs live in the temporary directory, independently of durable job metadata.
    #[serde(skip)]
    pub log_path: PathBuf,
    /// A live completion snapshot; never presented as an available full log file.
    #[serde(skip)]
    pub output: String,
    #[serde(skip)]
    pub output_truncated: bool,
    #[serde(skip)]
    pub log_bytes: u64,
}

impl BackgroundJob {
    pub fn scope_directory(&self) -> &Path {
        self.project_root
            .as_deref()
            .unwrap_or(&self.working_directory)
    }

    pub fn completion_message(&self) -> String {
        let status = match self.status {
            BackgroundJobStatus::Running => "running",
            BackgroundJobStatus::Succeeded => "succeeded",
            BackgroundJobStatus::Failed => "failed",
            BackgroundJobStatus::Cancelled => "cancelled",
        };
        let code = self
            .exit_code
            .map_or_else(|| "unavailable".to_owned(), |code| code.to_string());
        let truncation = if self.output_truncated {
            format!("\nLog output is truncated to the final {MAX_OUTPUT_BYTES} bytes of {} bytes. Full logs, while retained: {}\n", self.log_bytes, self.log_path.display())
        } else {
            String::new()
        };
        let error = self
            .error
            .as_ref()
            .map_or_else(String::new, |error| format!("\nDetails: {error}\n"));
        format!(
            "Background job {} {status}.\nWorking directory: {}\nProject/directory scope: {}\nCommand:\n{}\nExit code: {code}\n{error}{truncation}\nCommand logs:\n{}",
            self.id, self.working_directory.display(), self.scope_directory().display(), self.command, self.output,
        )
    }
}

pub struct BackgroundJobLaunch {
    pub conversation_id: AIConversationId,
    pub action_id: AIAgentActionId,
    pub command: String,
    pub working_directory: PathBuf,
    pub shell: Option<ShellLaunchData>,
    pub environment: Option<HashMap<String, String>>,
    pub host_id: Option<HostId>,
}

#[derive(Clone, Debug)]
pub enum BackgroundJobsEvent {
    Changed,
    Completed { id: BackgroundJobId },
}

#[derive(Serialize, Deserialize)]
struct SavedHistory {
    version: u32,
    jobs: Vec<BackgroundJob>,
}

struct TailUpdate {
    id: BackgroundJobId,
    output: String,
    total_bytes: u64,
}

pub struct BackgroundJobsModel {
    jobs: HashMap<BackgroundJobId, BackgroundJob>,
    running: HashMap<BackgroundJobId, Arc<runtime::ProcessControl>>,
    remote_cancellations: HashMap<BackgroundJobId, async_channel::Sender<()>>,
    unreconciled_remote_jobs: HashSet<BackgroundJobId>,
    pending_completions: HashSet<BackgroundJobId>,
    unpublished_completions: HashSet<BackgroundJobId>,
    interrupted_conversations: HashSet<AIConversationId>,
    history_directory: PathBuf,
    log_directory: PathBuf,
    tail_updates: async_channel::Sender<TailUpdate>,
    load_error: Option<String>,
    #[cfg(test)]
    _test_storage: tempfile::TempDir,
}

impl BackgroundJobsModel {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        #[cfg(not(test))]
        {
            use remote_server::manager::{RemoteServerManager, RemoteServerManagerEvent};

            let manager = RemoteServerManager::handle(ctx);
            ctx.subscribe_to_model(&manager, |me, _, event, ctx| {
                if let RemoteServerManagerEvent::HostConnected { host_id } = event {
                    remote::reattach_host(host_id, me, ctx);
                }
            });
        }
        #[cfg(test)]
        let test_storage = tempfile::tempdir().expect("background job test storage");
        #[cfg(test)]
        let history_directory = test_storage.path().join("history");
        #[cfg(not(test))]
        let history_directory = galaxy_core::paths::data_dir().join("background-jobs");
        #[cfg(test)]
        let log_directory = test_storage.path().join("logs");
        #[cfg(not(test))]
        let log_directory = log_directory();
        let (tail_updates, updates) = async_channel::bounded(128);
        ctx.spawn_stream_local(
            updates,
            |me, update: TailUpdate, ctx| {
                if let Some(job) = me
                    .jobs
                    .get_mut(&update.id)
                    .filter(|job| job.status == BackgroundJobStatus::Running)
                {
                    job.output = update.output;
                    job.log_bytes = update.total_bytes;
                    job.output_truncated = update.total_bytes > MAX_OUTPUT_BYTES as u64;
                    ctx.emit(BackgroundJobsEvent::Changed);
                }
            },
            |_, _| {},
        );

        let (jobs, interrupted_conversations, load_error) =
            load_history(&history_directory, &log_directory);
        let unreconciled_remote_jobs = jobs
            .values()
            .filter(|job| job.host_id.is_some() && job.status == BackgroundJobStatus::Running)
            .map(|job| job.id)
            .collect();
        let model = Self {
            jobs,
            running: HashMap::new(),
            remote_cancellations: HashMap::new(),
            unreconciled_remote_jobs,
            pending_completions: HashSet::new(),
            unpublished_completions: HashSet::new(),
            interrupted_conversations,
            history_directory,
            log_directory,
            tail_updates,
            load_error,
            #[cfg(test)]
            _test_storage: test_storage,
        };
        if model.load_error.is_none() {
            if let Err(error) = model.persist() {
                log::warn!("Failed to save restored background job metadata: {error}");
            }
        }
        model
    }

    pub fn records(&self) -> impl Iterator<Item = &BackgroundJob> {
        self.jobs.values()
    }

    pub fn job(&self, id: BackgroundJobId) -> Option<&BackgroundJob> {
        self.jobs.get(&id)
    }

    pub fn has_running_jobs(&self, conversation_id: AIConversationId) -> bool {
        self.jobs.values().any(|job| {
            job.conversation_id == conversation_id && job.status == BackgroundJobStatus::Running
        })
    }

    pub fn has_undelivered_history(&self, conversation_id: AIConversationId) -> bool {
        self.interrupted_conversations.contains(&conversation_id)
    }

    pub fn take_interrupted_conversation(&mut self, conversation_id: AIConversationId) -> bool {
        self.interrupted_conversations.remove(&conversation_id)
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn logs_available(&self, id: BackgroundJobId) -> bool {
        self.jobs.get(&id).is_some_and(|job| {
            fs::symlink_metadata(&job.log_path).is_ok_and(|metadata| metadata.file_type().is_file())
                && fs::File::open(&job.log_path).is_ok()
        })
    }

    pub fn read_logs(&self, id: BackgroundJobId) -> Result<String, String> {
        let job = self.jobs.get(&id).ok_or("Background job not found")?;
        let (text, bytes) = read_tail(&job.log_path, MAX_LOG_VIEW_BYTES)
            .map_err(|error| format!("Job logs are no longer available: {error}"))?;
        if bytes > MAX_LOG_VIEW_BYTES as u64 {
            Ok(format!("Showing the final {MAX_LOG_VIEW_BYTES} bytes of {bytes} bytes. Full log: {}\n\n{text}", job.log_path.display()))
        } else {
            Ok(text)
        }
    }

    pub fn start(
        &mut self,
        launch: BackgroundJobLaunch,
        ctx: &mut ModelContext<Self>,
    ) -> Result<BackgroundJobId, String> {
        self.start_with_id(Uuid::new_v4(), launch, ctx)
    }

    pub(crate) fn start_with_id(
        &mut self,
        id: BackgroundJobId,
        launch: BackgroundJobLaunch,
        ctx: &mut ModelContext<Self>,
    ) -> Result<BackgroundJobId, String> {
        if let Some(error) = &self.load_error {
            return Err(format!("Background job history is unavailable: {error}"));
        }
        if id.is_nil() || self.jobs.contains_key(&id) {
            return Err("Background job identity is invalid or already recorded".to_owned());
        }
        if launch.host_id.is_some() {
            return Err("Background command execution is not available for this remote session. Run the command yourself on that host; Galaxy will not run it on the local machine.".to_owned());
        }
        if launch.command.trim().is_empty() || launch.command.contains('\0') {
            return Err("A nonempty command without NUL characters is required".to_owned());
        }
        let cwd = fs::canonicalize(&launch.working_directory)
            .map_err(|error| format!("Cannot resolve command directory: {error}"))?;
        if !cwd.is_dir() {
            return Err("The command working directory is not a directory".to_owned());
        }
        let mut command = runtime::command_for_launch(&launch)?;
        command.current_dir(&cwd);
        ensure_private_directory(&self.log_directory)
            .map_err(|error| format!("Cannot create temporary job log directory: {error}"))?;
        let log_path = self.log_directory.join(format!("{id}.log"));
        let log = create_private_log(&log_path)
            .map_err(|error| format!("Cannot create job log: {error}"))?;
        // An independent open keeps completed output readable after temporary-file cleanup.
        // Cloning the writer would share its file offset with the child process on Unix.
        let mut log_reader = open_log_reader(&log_path)
            .map_err(|error| format!("Cannot open job log for completion capture: {error}"))?;
        command.stdout(command::Stdio::from(
            log.try_clone().map_err(|error| error.to_string())?,
        ));
        command.stderr(command::Stdio::from(log));
        let job = BackgroundJob {
            id,
            conversation_id: launch.conversation_id,
            action_id: launch.action_id,
            command: launch.command,
            working_directory: cwd.clone(),
            project_root: project_root(&cwd),
            host_id: launch.host_id,
            started_at: Utc::now(),
            finished_at: None,
            status: BackgroundJobStatus::Running,
            exit_code: None,
            error: None,
            completion_delivered: false,
            log_path: log_path.clone(),
            output: String::new(),
            output_truncated: false,
            log_bytes: 0,
        };
        self.interrupted_conversations.remove(&job.conversation_id);
        self.jobs.insert(id, job);
        if let Err(error) = self.persist() {
            self.jobs.remove(&id);
            let _ = fs::remove_file(&log_path);
            return Err(format!(
                "Cannot record background job before starting it: {error}"
            ));
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                self.finish(
                    id,
                    runtime::JobOutcome::launch_error(error.to_string()),
                    ctx,
                );
                return Ok(id);
            }
        };
        let process = match runtime::ProcessControl::new(child.id()) {
            Ok(process) => Arc::new(process),
            Err(error) => {
                let _ = child.kill();
                self.finish(
                    id,
                    runtime::JobOutcome::launch_error(format!(
                        "Cannot own background process tree: {error}"
                    )),
                    ctx,
                );
                return Ok(id);
            }
        };
        self.running.insert(id, Arc::clone(&process));
        let updates = self.tail_updates.clone();
        ctx.spawn(
            async move {
                let _guard = runtime::ProcessGuard(Arc::clone(&process));
                let status = async {
                    #[cfg(unix)]
                    process.wait_for_exit_without_reaping().await?;
                    child.status().await
                };
                futures::pin_mut!(status);
                let mut previous_size = 0;
                loop {
                    let tick = Timer::after(Duration::from_millis(500));
                    futures::pin_mut!(tick);
                    match futures::future::select(status.as_mut(), tick).await {
                        futures::future::Either::Left((result, _)) => {
                            process.cleanup();
                            return runtime::JobOutcome::from_exit(
                                result,
                                process.cancel_requested(),
                                &mut log_reader,
                            );
                        }
                        futures::future::Either::Right(_) => {
                            let size = log_reader.metadata().map_or(0, |metadata| metadata.len());
                            if size != previous_size {
                                previous_size = size;
                                if let Ok((output, total_bytes)) =
                                    read_tail_from_open_file(&mut log_reader, MAX_OUTPUT_BYTES)
                                {
                                    let _ = updates.try_send(TailUpdate {
                                        id,
                                        output,
                                        total_bytes,
                                    });
                                }
                            }
                        }
                    }
                }
            },
            move |me, outcome, ctx| me.finish(id, outcome, ctx),
        );
        ctx.emit(BackgroundJobsEvent::Changed);
        Ok(id)
    }

    /// A cancellation can beat a delayed remote Start request. Persist its identity before
    /// acknowledging cancellation so that request can never launch the command afterward.
    pub(crate) fn cancel_before_start(
        &mut self,
        id: BackgroundJobId,
        launch: BackgroundJobLaunch,
        ctx: &mut ModelContext<Self>,
    ) -> Result<BackgroundJobId, String> {
        if let Some(error) = &self.load_error {
            return Err(format!("Background job history is unavailable: {error}"));
        }
        if id.is_nil() || self.jobs.contains_key(&id) {
            return Err("Background job identity is invalid or already recorded".to_owned());
        }
        if launch.command.trim().is_empty()
            || launch.command.contains('\0')
            || !launch.working_directory.is_absolute()
        {
            return Err(
                "A cancelled launch requires its original command and absolute directory"
                    .to_owned(),
            );
        }
        let cwd = fs::canonicalize(&launch.working_directory).unwrap_or(launch.working_directory);
        let now = Utc::now();
        self.jobs.insert(
            id,
            BackgroundJob {
                id,
                conversation_id: launch.conversation_id,
                action_id: launch.action_id,
                command: launch.command,
                working_directory: cwd.clone(),
                project_root: project_root(&cwd),
                host_id: None,
                started_at: now,
                finished_at: Some(now),
                status: BackgroundJobStatus::Cancelled,
                exit_code: None,
                error: Some("Cancelled before the remote command was started".to_owned()),
                completion_delivered: true,
                log_path: self.log_directory.join(format!("{id}.log")),
                output: String::new(),
                output_truncated: false,
                log_bytes: 0,
            },
        );
        if let Err(error) = self.persist() {
            self.jobs.remove(&id);
            return Err(format!("Cannot record cancellation before launch: {error}"));
        }
        ctx.emit(BackgroundJobsEvent::Changed);
        Ok(id)
    }

    pub fn cancel(
        &mut self,
        id: BackgroundJobId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), String> {
        if self.unreconciled_remote_jobs.contains(&id)
            && !self.remote_cancellations.contains_key(&id)
        {
            return Err("Waiting to reconnect and confirm this job's state on its host before cancellation is available".to_owned());
        }
        if let Some(cancel) = self.remote_cancellations.get(&id) {
            cancel
                .try_send(())
                .map_err(|error| format!("Could not request remote job cancellation: {error}"))?;
            ctx.emit(BackgroundJobsEvent::Changed);
            return Ok(());
        }
        let process = self
            .running
            .get(&id)
            .ok_or("This background job is no longer running")?;
        process
            .cancel()
            .map_err(|error| format!("Could not terminate background job: {error}"))?;
        ctx.emit(BackgroundJobsEvent::Changed);
        Ok(())
    }

    /// Installs a daemon-owned job without resolving its paths on the client machine.
    pub fn register_remote_job(
        &mut self,
        mut job: BackgroundJob,
        cancellation: async_channel::Sender<()>,
        ctx: &mut ModelContext<Self>,
    ) -> Result<BackgroundJobId, String> {
        if let Some(error) = &self.load_error {
            return Err(format!("Background job history is unavailable: {error}"));
        }
        if job.host_id.is_none() {
            return Err("A remote job requires its owning host identity".to_owned());
        }
        if self.jobs.contains_key(&job.id) {
            return Err("This background job is already registered".to_owned());
        }
        ensure_private_directory(&self.log_directory).map_err(|error| error.to_string())?;
        job.log_path = self.log_directory.join(format!("{}.log", job.id));
        create_private_log(&job.log_path).map_err(|error| error.to_string())?;
        job.output.clear();
        job.output_truncated = false;
        job.log_bytes = 0;
        job.completion_delivered = false;
        let id = job.id;
        self.jobs.insert(id, job);
        if let Err(error) = self.persist() {
            if let Some(job) = self.jobs.remove(&id) {
                let _ = fs::remove_file(job.log_path);
            }
            return Err(format!("Cannot persist remote background job: {error}"));
        }
        self.remote_cancellations.insert(id, cancellation);
        ctx.emit(BackgroundJobsEvent::Changed);
        Ok(id)
    }

    /// Reconnects a previously recorded process; never submits the command again.
    pub fn reattach_remote_job(
        &mut self,
        id: BackgroundJobId,
        cancellation: async_channel::Sender<()>,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), String> {
        let job = self
            .jobs
            .get(&id)
            .ok_or("Remote background job not found")?;
        if job.host_id.is_none() || job.status != BackgroundJobStatus::Running {
            return Err("Only a running remote background job can be reconnected".to_owned());
        }
        if self.remote_cancellations.contains_key(&id) {
            return Err("This remote background job is already being monitored".to_owned());
        }
        self.remote_cancellations.insert(id, cancellation);
        ctx.emit(BackgroundJobsEvent::Changed);
        Ok(())
    }

    /// Only a confirmed running process can generate a new completion after restart.
    pub fn arm_remote_completion(
        &mut self,
        id: BackgroundJobId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), String> {
        if !self.unreconciled_remote_jobs.contains(&id) {
            return Ok(());
        }
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or("Remote background job not found")?;
        if job.host_id.is_none() || job.status != BackgroundJobStatus::Running {
            return Err("Only a confirmed running remote job can notify completion".to_owned());
        }
        self.unreconciled_remote_jobs.remove(&id);
        job.completion_delivered = false;
        job.error = None;
        let result = self.persist().map_err(|error| error.to_string());
        ctx.emit(BackgroundJobsEvent::Changed);
        result
    }

    /// The transport supplies only bytes not previously mirrored, using its own log cursor.
    pub fn update_remote_job(
        &mut self,
        id: BackgroundJobId,
        status: BackgroundJobStatus,
        exit_code: Option<i32>,
        mut error: Option<String>,
        log_chunk: &[u8],
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), String> {
        let job = self
            .jobs
            .get_mut(&id)
            .ok_or("Remote background job not found")?;
        if job.host_id.is_none() {
            return Err("Cannot apply a remote update to a local job".to_owned());
        }
        if job.status.is_terminal() {
            return Ok(());
        }
        let capture = (|| -> std::io::Result<_> {
            if !log_chunk.is_empty() {
                let mut options = fs::OpenOptions::new();
                options.append(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.custom_flags(libc::O_NOFOLLOW);
                }
                let mut file = options.open(&job.log_path)?;
                if !file.metadata()?.is_file() {
                    return Err(std::io::Error::other(
                        "Remote job log is not a regular file",
                    ));
                }
                file.write_all(log_chunk)?;
            }
            read_tail(&job.log_path, MAX_OUTPUT_BYTES)
        })();
        let (output, log_bytes) = match capture {
            Ok(captured) => captured,
            Err(capture_error) => {
                // Purging temporary files never changes the remote process's authoritative
                // status or discards its newly received output. Do not recreate the log path.
                let mut output = job.output.clone();
                output.push_str(&String::from_utf8_lossy(log_chunk));
                if output.len() > MAX_OUTPUT_BYTES {
                    let mut start = output.len() - MAX_OUTPUT_BYTES;
                    while !output.is_char_boundary(start) {
                        start += 1;
                    }
                    output.drain(..start);
                }
                let message = format!("The local temporary log is unavailable; captured context output is retained: {capture_error}");
                error = Some(match error {
                    Some(error) => format!("{error} {message}"),
                    None => message,
                });
                (output, job.log_bytes.saturating_add(log_chunk.len() as u64))
            }
        };
        if status.is_terminal() {
            self.remote_cancellations.remove(&id);
            self.finish(
                id,
                runtime::JobOutcome {
                    status,
                    exit_code,
                    error,
                    output,
                    log_bytes,
                },
                ctx,
            );
        } else {
            job.output = output;
            job.log_bytes = log_bytes;
            job.output_truncated = log_bytes > MAX_OUTPUT_BYTES as u64;
            job.error = error;
            ctx.emit(BackgroundJobsEvent::Changed);
        }
        Ok(())
    }

    pub fn pending_completions(&self, conversation_id: AIConversationId) -> Vec<BackgroundJob> {
        let mut pending = self
            .pending_completions
            .iter()
            .filter_map(|id| self.jobs.get(id))
            .filter(|job| job.conversation_id == conversation_id && !job.completion_delivered)
            .cloned()
            .collect::<Vec<_>>();
        pending.sort_by_key(|job| (job.finished_at, job.id));
        pending
    }

    pub fn acknowledge_completion(
        &mut self,
        id: BackgroundJobId,
        ctx: &mut ModelContext<Self>,
    ) -> Result<(), String> {
        let job = self.jobs.get_mut(&id).ok_or("Background job not found")?;
        if !job.status.is_terminal() {
            return Err("A running background job cannot be acknowledged".to_owned());
        }
        job.completion_delivered = true;
        self.pending_completions.remove(&id);
        self.unpublished_completions.remove(&id);
        let result = self.persist().map_err(|error| error.to_string());
        ctx.emit(BackgroundJobsEvent::Changed);
        result
    }

    fn finish(
        &mut self,
        id: BackgroundJobId,
        outcome: runtime::JobOutcome,
        ctx: &mut ModelContext<Self>,
    ) {
        self.running.remove(&id);
        let Some(job) = self.jobs.get_mut(&id) else {
            return;
        };
        job.status = outcome.status;
        job.exit_code = outcome.exit_code;
        job.error = outcome.error;
        job.output = outcome.output;
        job.log_bytes = outcome.log_bytes;
        job.output_truncated = outcome.log_bytes > MAX_OUTPUT_BYTES as u64;
        job.finished_at = Some(Utc::now());
        if self.unreconciled_remote_jobs.remove(&id) {
            // A process that ended while Galaxy was closed is history, not a reason to
            // restart its agent. Retain its actual status and logs without a mailbox item.
            job.completion_delivered = true;
            if let Err(error) = self.persist() {
                log::warn!("Failed to persist reconciled remote job metadata: {error}");
            }
            ctx.emit(BackgroundJobsEvent::Changed);
            return;
        }
        self.unpublished_completions.insert(id);
        self.publish_completion(id, ctx);
    }

    fn publish_completion(&mut self, id: BackgroundJobId, ctx: &mut ModelContext<Self>) {
        if !self.unpublished_completions.contains(&id) {
            return;
        }
        if let Err(error) = self.persist() {
            log::warn!("Failed to persist background job completion: {error}");
            ctx.spawn(Timer::after(Duration::from_secs(5)), move |me, _, ctx| {
                me.publish_completion(id, ctx)
            });
            return;
        }
        self.unpublished_completions.remove(&id);
        self.pending_completions.insert(id);
        ctx.emit(BackgroundJobsEvent::Changed);
        ctx.emit(BackgroundJobsEvent::Completed { id });
    }

    fn persist(&self) -> std::io::Result<()> {
        ensure_private_directory(&self.history_directory)?;
        let mut jobs = self.jobs.values().cloned().collect::<Vec<_>>();
        jobs.sort_by_key(|job| (job.started_at, job.id));
        let mut temporary = tempfile::NamedTempFile::new_in(&self.history_directory)?;
        serde_json::to_writer(&mut temporary, &SavedHistory { version: 1, jobs })
            .map_err(std::io::Error::other)?;
        temporary.flush()?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.history_directory.join("history.json"))
            .map_err(|error| error.error)?;
        Ok(())
    }
}

impl Drop for BackgroundJobsModel {
    fn drop(&mut self) {
        for process in self.running.values() {
            process.cleanup();
        }
    }
}

impl Entity for BackgroundJobsModel {
    type Event = BackgroundJobsEvent;
}
impl SingletonEntity for BackgroundJobsModel {}

fn project_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|parent| parent.join(".git").exists())
        .map(Path::to_path_buf)
}

fn log_directory() -> PathBuf {
    #[cfg(unix)]
    let user = unsafe { libc::geteuid() }.to_string();
    #[cfg(not(unix))]
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_owned());
    std::env::temp_dir().join(format!("galaxy-background-jobs-{user}"))
}

fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_dir() {
        return Err(std::io::Error::other(
            "Job storage must be a directory, not a symlink",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Job storage belongs to another user",
            ));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_log(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).append(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

pub(super) fn read_tail(path: &Path, limit: usize) -> std::io::Result<(String, u64)> {
    let mut file = open_log_reader(path)?;
    read_tail_from_open_file(&mut file, limit)
}

fn open_log_reader(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(super) fn read_tail_from_open_file(
    file: &mut fs::File,
    limit: usize,
) -> std::io::Result<(String, u64)> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("Job log is not a regular file"));
    }
    let length = metadata.len();
    file.seek(SeekFrom::Start(length.saturating_sub(limit as u64)))?;
    let mut bytes = Vec::new();
    file.take(limit as u64).read_to_end(&mut bytes)?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), length))
}

fn load_history(
    directory: &Path,
    logs: &Path,
) -> (
    HashMap<BackgroundJobId, BackgroundJob>,
    HashSet<AIConversationId>,
    Option<String>,
) {
    let path = directory.join("history.json");
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (HashMap::new(), HashSet::new(), None)
        }
        Err(error) => return (HashMap::new(), HashSet::new(), Some(error.to_string())),
    };
    let history = match serde_json::from_slice::<SavedHistory>(&bytes) {
        Ok(history) if history.version == 1 => history,
        Ok(_) => {
            return (
                HashMap::new(),
                HashSet::new(),
                Some("Unsupported background job history version".to_owned()),
            )
        }
        Err(error) => return (HashMap::new(), HashSet::new(), Some(error.to_string())),
    };
    let mut jobs = HashMap::new();
    let mut interrupted = HashSet::new();
    for mut job in history.jobs {
        job.log_path = logs.join(format!("{}.log", job.id));
        if (job.status == BackgroundJobStatus::Running && job.host_id.is_none())
            || (job.status.is_terminal() && !job.completion_delivered)
        {
            interrupted.insert(job.conversation_id);
        }
        if job.status == BackgroundJobStatus::Running {
            if job.host_id.is_some() {
                job.error = Some("Waiting to reconnect and confirm this job's state on its host. The command has not been restarted.".to_owned());
                if let Ok((output, log_bytes)) = read_tail(&job.log_path, MAX_OUTPUT_BYTES) {
                    job.output = output;
                    job.log_bytes = log_bytes;
                    job.output_truncated = log_bytes > MAX_OUTPUT_BYTES as u64;
                }
            } else {
                job.status = BackgroundJobStatus::Cancelled;
                job.finished_at = Some(Utc::now());
                job.error = Some("Galaxy restarted before this job's completion was recorded. The command has not been restarted.".to_owned());
            }
        }
        // A restored record is history, never an instruction to wake an agent.
        job.completion_delivered = true;
        jobs.insert(job.id, job);
    }
    (jobs, interrupted, None)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
