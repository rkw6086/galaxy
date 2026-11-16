//! Bounded, opt-in forwarding of application warnings and errors.
//!
//! The logger callback never waits for the uploader or reads application state.
//! Configuration is published separately on the settings-owning thread.

use std::cell::Cell;
use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;

use super::{RemoteLogConfig, RemoteLogPayload};

const MAX_QUEUED_RECORDS: usize = 256;
const MAX_QUEUED_BYTES: usize = 1024 * 1024;
const MAX_FORMATTED_CHARS: usize = 16 * 1024;
const MAX_LOCAL_DIAGNOSTICS: usize = 3;

static UPLOADER: OnceLock<Arc<Uploader>> = OnceLock::new();

thread_local! {
    // Also remains set for the uploader's entire lifetime: HTTP/TLS diagnostics
    // from its current-thread runtime must not enqueue another remote request.
    static CAPTURE_SUPPRESSED: Cell<bool> = const { Cell::new(false) };
}

struct CaptureGuard<'a>(&'a Cell<bool>);

impl Drop for CaptureGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

struct QueuedRecord {
    config: Arc<RemoteLogConfig>,
    generation: u64,
    payload: RemoteLogPayload,
    bytes: usize,
}

#[derive(Default)]
struct QueueState {
    config: Option<Arc<RemoteLogConfig>>,
    queue: VecDeque<QueuedRecord>,
    queued_bytes: usize,
    in_flight: bool,
}

struct Uploader {
    state: Mutex<QueueState>,
    wake: Condvar,
    generation: AtomicU64,
    worker_available: AtomicBool,
    dropped_records: AtomicU64,
    failed_uploads: AtomicU64,
    local_diagnostics: AtomicUsize,
    instance_id: String,
}

impl Uploader {
    fn new() -> Arc<Self> {
        let uploader = Arc::new(Self {
            state: Mutex::new(QueueState::default()),
            wake: Condvar::new(),
            generation: AtomicU64::new(0),
            worker_available: AtomicBool::new(true),
            dropped_records: AtomicU64::new(0),
            failed_uploads: AtomicU64::new(0),
            local_diagnostics: AtomicUsize::new(0),
            instance_id: uuid::Uuid::new_v4().to_string(),
        });
        let worker = Arc::clone(&uploader);
        if let Err(error) = std::thread::Builder::new()
            .name("galaxy-remote-errors".to_owned())
            .spawn(move || worker.run())
        {
            uploader.worker_available.store(false, Ordering::Release);
            uploader.local_diagnostic(&format!(
                "could not start application log uploader: {}",
                super::sanitize_error(error)
            ));
        }
        uploader
    }

    fn run(self: Arc<Self>) {
        CAPTURE_SUPPRESSED.with(|suppressed| suppressed.set(true));
        // Ensure flush also terminates promptly if the worker exits or unwinds.
        struct WorkerGuard(Arc<Uploader>);
        impl Drop for WorkerGuard {
            fn drop(&mut self) {
                self.0.worker_available.store(false, Ordering::Release);
                let mut state = self
                    .0
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                add_count(&self.0.dropped_records, state.queue.len() as u64);
                state.queue.clear();
                state.queued_bytes = 0;
                state.in_flight = false;
            }
        }
        let _guard = WorkerGuard(Arc::clone(&self));
        // Do not initialize a runtime or HTTP client during early startup.
        // Until the first actual upload, the worker only waits for its queue.
        let runtime = OnceLock::new();
        let client = OnceLock::new();
        let mut reported_drops = false;
        loop {
            let record = {
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                while state.queue.is_empty() {
                    state = self
                        .wake
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                // Error diagnostics must not wait behind a warning storm.
                // Taking the first error preserves FIFO order within levels.
                let next = state
                    .queue
                    .iter()
                    .position(|record| record.payload.level == "error")
                    .unwrap_or(0);
                let record = state.queue.remove(next).expect("nonempty upload queue");
                state.queued_bytes -= record.bytes;
                state.in_flight = true;
                record
            };

            let dropped = self.dropped_records.load(Ordering::Relaxed);
            if dropped > 0 && !reported_drops {
                reported_drops = true;
                self.local_diagnostic(&format!(
                    "application log records were dropped ({dropped} so far); totals accompany subsequent records"
                ));
            }

            // Never send queued content with another endpoint or credentials.
            // A configuration change after this check may leave this one
            // already-starting request in flight; it cannot retarget it.
            if self.generation.load(Ordering::Acquire) == record.generation {
                let runtime = match runtime.get_or_init(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                }) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        add_count(&self.failed_uploads, 1);
                        self.local_diagnostic(&format!(
                            "could not initialize application log uploader: {}",
                            super::sanitize_error(error)
                        ));
                        return;
                    }
                };
                // Keep this pool independent of AI uploads. A pooled client's
                // connection drivers belong to the runtime that created them.
                let client = match client.get_or_init(|| {
                    runtime.block_on(async {
                        reqwest::Client::builder()
                            .timeout(super::REMOTE_LOG_TIMEOUT)
                            .build()
                    })
                }) {
                    Ok(client) => client,
                    Err(error) => {
                        add_count(&self.failed_uploads, 1);
                        self.local_diagnostic(&format!(
                            "could not initialize application log HTTP client: {}",
                            sanitized(error, &record.config.api_key)
                        ));
                        return;
                    }
                };
                if self.generation.load(Ordering::Acquire) == record.generation {
                    let config = (*record.config).clone();
                    if let Err(error) = runtime.block_on(super::send_remote_log_with_client(
                        client,
                        config,
                        record.payload,
                    )) {
                        add_count(&self.failed_uploads, 1);
                        self.local_diagnostic(&format!(
                            "application log upload failed (no retry): {}",
                            sanitized(error, &record.config.api_key)
                        ));
                    }
                } else {
                    add_count(&self.dropped_records, 1);
                }
            } else {
                add_count(&self.dropped_records, 1);
            }

            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .in_flight = false;
        }
    }

    fn local_diagnostic(&self, message: &str) {
        if self
            .local_diagnostics
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < MAX_LOCAL_DIAGNOSTICS).then_some(count + 1)
            })
            .is_ok()
        {
            // Do not route uploader failures through log::log!: that would
            // recursively create the same request that just failed.
            eprintln!("[remote-logging] {message}");
        }
    }
}

fn add_count(counter: &AtomicU64, count: u64) {
    let _ = counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(count))
    });
}

/// Called on settings initialization and updates, outside the logger callback.
pub(super) fn configure(config: Option<RemoteLogConfig>) {
    let uploader = match (&config, UPLOADER.get()) {
        (None, None) => return,
        (_, Some(uploader)) => uploader,
        (Some(_), None) => UPLOADER.get_or_init(Uploader::new),
    };
    let mut state = uploader
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.config.as_deref() == config.as_ref() {
        return;
    }
    uploader.generation.fetch_add(1, Ordering::AcqRel);
    add_count(&uploader.dropped_records, state.queue.len() as u64);
    state.queue.clear();
    state.queued_bytes = 0;
    state.config = config.map(Arc::new);
    drop(state);
    uploader.wake.notify_one();
}

/// Runs on arbitrary logging threads; never blocks on queue access or upload.
pub(super) fn capture(record: &log::Record<'_>) {
    if record.level() > log::Level::Warn
        || record.target().contains("::remote_logging")
        || record.target().starts_with("remote-logging")
    {
        return;
    }
    let Some(uploader) = UPLOADER.get() else {
        return;
    };
    let _ = CAPTURE_SUPPRESSED.try_with(|suppressed| {
        if suppressed.replace(true) {
            return;
        }
        let _guard = CaptureGuard(suppressed);
        capture_inner(uploader, record);
    });
}

fn capture_inner(uploader: &Uploader, record: &log::Record<'_>) {
    if !uploader.worker_available.load(Ordering::Acquire) {
        add_count(&uploader.dropped_records, 1);
        return;
    }
    let (config, generation) = match uploader.state.try_lock() {
        Ok(state) => {
            let Some(config) = &state.config else {
                return;
            };
            if (state.queue.len() >= MAX_QUEUED_RECORDS || state.queued_bytes >= MAX_QUEUED_BYTES)
                && (record.level() != log::Level::Error
                    || !state
                        .queue
                        .iter()
                        .any(|queued| queued.payload.level == "warn"))
            {
                add_count(&uploader.dropped_records, 1);
                return;
            }
            (
                Arc::clone(config),
                uploader.generation.load(Ordering::Acquire),
            )
        }
        Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
            add_count(&uploader.dropped_records, 1);
            return;
        }
    };

    let timestamp = Utc::now().to_rfc3339();
    let message = sanitized(record.args(), &config.api_key);
    let thread = std::thread::current();
    let mut context = super::enrich_context(json!({
        "event": "application_log",
        "error": message,
        "target": sanitized(record.target(), &config.api_key),
        "module": record.module_path().map(|value| sanitized(value, &config.api_key)),
        "file": record.file().map(|value| sanitized(value, &config.api_key)),
        "line": record.line(),
        "thread": {
            "name": thread.name().map(|value| sanitized(value, &config.api_key)),
            "id": format!("{:?}", thread.id()),
        },
        "process_id": std::process::id(),
        "process_instance_id": uploader.instance_id,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    }));
    // Preserve occurrence time, rather than replacing it at upload time.
    context["timestamp"] = json!(timestamp);
    let mut payload = RemoteLogPayload {
        level: match record.level() {
            log::Level::Error => "error",
            _ => "warn",
        },
        message,
        service: super::REMOTE_LOG_SERVICE,
        context,
    };

    let mut state = match uploader.state.try_lock() {
        Ok(state) => state,
        Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => {
            add_count(&uploader.dropped_records, 1);
            return;
        }
    };
    if uploader.generation.load(Ordering::Acquire) != generation
        || state.config.is_none()
        || !uploader.worker_available.load(Ordering::Acquire)
    {
        add_count(&uploader.dropped_records, 1);
        return;
    }
    let bytes = loop {
        payload.context["remote_logging"] = json!({
            "dropped_records_total": uploader.dropped_records.load(Ordering::Relaxed),
            "failed_uploads_total": uploader.failed_uploads.load(Ordering::Relaxed),
            "queue_max_records": MAX_QUEUED_RECORDS,
            "queue_max_bytes": MAX_QUEUED_BYTES,
        });
        let bytes = match serde_json::to_vec(&payload) {
            Ok(serialized) => serialized.len(),
            Err(_) => {
                add_count(&uploader.dropped_records, 1);
                return;
            }
        };
        if bytes > MAX_QUEUED_BYTES {
            add_count(&uploader.dropped_records, 1);
            return;
        }
        if state.queue.len() < MAX_QUEUED_RECORDS
            && bytes <= MAX_QUEUED_BYTES.saturating_sub(state.queued_bytes)
        {
            break bytes;
        }

        // Only an error may replace queued warnings. Never discard an error
        // to admit a warning, and keep the oldest retained error first.
        if record.level() == log::Level::Error {
            if let Some(oldest_warning) = state
                .queue
                .iter()
                .position(|queued| queued.payload.level == "warn")
            {
                let removed = state.queue.remove(oldest_warning).unwrap();
                state.queued_bytes -= removed.bytes;
                add_count(&uploader.dropped_records, 1);
                continue;
            }
        }
        add_count(&uploader.dropped_records, 1);
        return;
    };
    state.queued_bytes += bytes;
    state.queue.push_back(QueuedRecord {
        config,
        generation,
        payload,
        bytes,
    });
    drop(state);
    uploader.wake.notify_one();
}

/// Bound formatting before redaction. Scrub the exact configured logger key as
/// well as known credential patterns, including when it has no field label.
fn sanitized(value: impl fmt::Display, api_key: &str) -> String {
    struct Prefix {
        text: String,
        remaining: usize,
        truncated: bool,
    }
    impl fmt::Write for Prefix {
        fn write_str(&mut self, value: &str) -> fmt::Result {
            if let Some((boundary, _)) = value.char_indices().nth(self.remaining) {
                self.text.push_str(&value[..boundary]);
                self.remaining = 0;
                self.truncated = true;
                return Err(fmt::Error);
            }
            self.text.push_str(value);
            self.remaining -= value.chars().count();
            Ok(())
        }
    }
    let mut prefix = Prefix {
        text: String::new(),
        remaining: MAX_FORMATTED_CHARS,
        truncated: false,
    };
    let _ = write!(&mut prefix, "{value}");
    if prefix.truncated && !api_key.is_empty() {
        // The formatting limit might split an otherwise exact key match.
        // Remove enough of the final prefix to avoid retaining that fragment.
        let mut end = prefix.text.len().saturating_sub(api_key.len());
        while !prefix.text.is_char_boundary(end) {
            end -= 1;
        }
        prefix.text.truncate(end);
    }
    let text = if api_key.is_empty() {
        prefix.text
    } else {
        prefix.text.replace(api_key, "[redacted]")
    };
    super::sanitize_error(text)
}

/// Best-effort bounded drain for normal exit and panic reporting. No joining
/// or blocking mutex acquisition is allowed on these shutdown paths.
pub(super) fn flush(timeout: Duration) {
    if !matches!(CAPTURE_SUPPRESSED.try_with(Cell::get), Ok(false)) {
        return;
    }
    let Some(uploader) = UPLOADER.get() else {
        return;
    };
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return;
    };
    uploader.wake.notify_one();
    loop {
        if !uploader.worker_available.load(Ordering::Acquire) {
            return;
        }
        if let Ok(state) = uploader.state.try_lock() {
            if state.queue.is_empty() && !state.in_flight {
                return;
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(2)));
    }
}
