//! Local, opt-in inspection of Galaxy's AI traffic and execution lifecycle.
//!
//! Payloads describe Galaxy's provider-neutral requests/results, not HTTP wire
//! traffic. No transport headers or provider credentials are captured here.

#[cfg(not(target_family = "wasm"))]
pub(crate) mod view;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};

/// Limits bound both retention and the work spent serializing a single event.
pub(crate) const MAX_PAYLOAD_CHARS: usize = 256 * 1024;
const MAX_ITEMS: usize = 2_000;
const MAX_RETAINED_BYTES: usize = 16 * 1024 * 1024;
const MAX_LABELS: usize = 4_096;

static RECORDING: AtomicBool = AtomicBool::new(false);
static STORE: LazyLock<Mutex<ActivityStore>> = LazyLock::new(Mutex::default);

#[derive(Clone)]
pub(crate) struct ActivityEntry {
    pub id: u64,
    pub timestamp: DateTime<Utc>,
    pub session_id: Option<String>,
    pub session_label: String,
    pub category: String,
    pub event: String,
    pub summary: String,
    pub model: Option<String>,
    pub duration_ms: Option<u64>,
    pub detail: String,
}

pub(crate) struct ActivitySnapshot {
    pub recording: bool,
    pub revision: u64,
    pub dropped: usize,
    pub entries: Vec<Arc<ActivityEntry>>,
}

#[derive(Default)]
struct ActivityStore {
    entries: VecDeque<Arc<ActivityEntry>>,
    labels: HashMap<String, String>,
    label_order: VecDeque<String>,
    sessions: HashMap<String, String>,
    correlation_order: VecDeque<String>,
    retained_bytes: usize,
    next_id: u64,
    revision: u64,
    dropped: usize,
    recording: bool,
    generation: u64,
}

pub(crate) fn is_recording() -> bool {
    RECORDING.load(Ordering::Relaxed)
}

pub(crate) fn set_recording(recording: bool) {
    if let Ok(mut store) = STORE.lock() {
        store.set_recording(recording);
        RECORDING.store(recording, Ordering::Relaxed);
    }
}

/// Stops neither running work nor recording; it only clears retained diagnostics.
pub(crate) fn clear() {
    if let Ok(mut store) = STORE.lock() {
        store.clear();
    }
}

pub(crate) fn snapshot() -> ActivitySnapshot {
    let store = STORE.lock().unwrap_or_else(|error| error.into_inner());
    ActivitySnapshot {
        recording: store.recording,
        revision: store.revision,
        dropped: store.dropped,
        entries: store.entries.iter().cloned().collect(),
    }
}

/// Titles are bounded separately from payloads, and retained across Clear so
/// subsequent tool/background events keep their owning session's name.
pub(crate) fn register_session(session_id: impl Into<String>, label: impl Into<String>) {
    if !is_recording() {
        return;
    }
    let session_id = session_id.into();
    if session_id.is_empty() || session_id.len() > 256 {
        return;
    }
    let label = bounded_text(&label.into(), 200);
    if label.trim().is_empty() {
        return;
    }
    if let Ok(mut store) = STORE.lock() {
        if store.recording {
            store.register_session(session_id, label);
        }
    }
}

pub(crate) fn record(event: &str, summary: impl Into<String>, context: Value) {
    if !is_recording() {
        return;
    }
    record_serializable(event, summary, context, &());
}

/// Serialize by reference with early termination, without first cloning a
/// conversation or materializing an unbounded serde_json::Value.
pub(crate) fn record_serializable(
    event: &str,
    summary: impl Into<String>,
    context: Value,
    payload: &(impl Serialize + ?Sized),
) {
    if !is_recording() {
        return;
    }
    let Some(generation) = STORE
        .lock()
        .ok()
        .and_then(|store| store.capture_generation())
    else {
        return;
    };
    let timestamp = Utc::now();
    #[derive(Serialize)]
    struct Capture<'a, T: Serialize + ?Sized> {
        context: &'a Value,
        payload: &'a T,
    }
    let captured =
        super::remote_logging::prefix_bounded_payload_context(MAX_PAYLOAD_CHARS, |writer| {
            serde_json::to_writer_pretty(
                writer,
                &Capture {
                    context: &context,
                    payload,
                },
            )
        });
    let mut detail = captured["payload"].as_str().unwrap_or_default().to_owned();
    if captured["truncated"].as_bool() == Some(true) {
        detail.push_str(
            "\n\n[Capture truncated at 262,144 characters; remaining payload was not serialized.]",
        );
    }
    if let Some(error) = captured.get("serialization_error") {
        detail = format!("Capture could not be serialized: {error}");
    }
    let Ok(mut store) = STORE.lock() else {
        return;
    };
    store.push_captured(
        generation,
        event,
        summary.into(),
        &context,
        detail,
        timestamp,
    );
}

impl ActivityStore {
    fn set_recording(&mut self, recording: bool) {
        if self.recording != recording {
            self.generation = self.generation.wrapping_add(1);
            self.recording = recording;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.retained_bytes = 0;
        self.dropped = 0;
        self.generation = self.generation.wrapping_add(1);
        self.revision = self.revision.wrapping_add(1);
    }

    fn capture_generation(&self) -> Option<u64> {
        self.recording.then_some(self.generation)
    }

    fn register_session(&mut self, session_id: String, label: String) {
        let label = format!(
            "{} · {}",
            bounded_text(&label, 185),
            session_id.chars().take(8).collect::<String>()
        );
        if self.labels.get(&session_id) == Some(&label) {
            return;
        }
        if !self.labels.contains_key(&session_id) {
            if self.labels.len() >= MAX_LABELS {
                if let Some(oldest) = self.label_order.pop_front() {
                    self.labels.remove(&oldest);
                }
            }
            self.label_order.push_back(session_id.clone());
        }
        self.labels.insert(session_id.clone(), label.clone());
        for entry in &mut self.entries {
            if entry.session_id.as_deref() == Some(&session_id) {
                self.retained_bytes -= entry_size(entry);
                Arc::make_mut(entry).session_label.clone_from(&label);
                self.retained_bytes += entry_size(entry);
            }
        }
        self.evict();
        self.revision = self.revision.wrapping_add(1);
    }

    fn push_captured(
        &mut self,
        generation: u64,
        event: &str,
        summary: String,
        context: &Value,
        detail: String,
        timestamp: DateTime<Utc>,
    ) {
        // Clear and pause/resume invalidate captures that started before the
        // transition, even when serialization finishes after recording resumes.
        if self.capture_generation() != Some(generation) {
            return;
        }
        let mut session_id = session_id(context);
        let correlation = correlation_keys(context);
        if session_id.is_none() {
            session_id = correlation
                .iter()
                .find_map(|key| self.sessions.get(key).cloned());
        }
        if let Some(id) = &session_id {
            for key in correlation {
                if !self.sessions.contains_key(&key) {
                    if self.sessions.len() >= MAX_LABELS {
                        if let Some(oldest) = self.correlation_order.pop_front() {
                            self.sessions.remove(&oldest);
                        }
                    }
                    self.correlation_order.push_back(key.clone());
                }
                self.sessions.insert(key, id.clone());
            }
        }
        let session_label = session_id.as_ref().map_or_else(
            || "App background activity".to_owned(),
            |id| {
                self.labels.get(id).cloned().unwrap_or_else(|| {
                    format!("Session {}", id.chars().take(8).collect::<String>())
                })
            },
        );
        let entry = Arc::new(ActivityEntry {
            id: self.next_id,
            timestamp,
            session_id,
            session_label,
            category: category(event).to_owned(),
            event: bounded_text(event, 160),
            summary: bounded_text(&summary, 400),
            model: context
                .get("model_id")
                .or_else(|| context.get("model"))
                .and_then(Value::as_str)
                .map(|model| bounded_text(model, 200)),
            duration_ms: context
                .get("elapsed_ms")
                .or_else(|| context.get("duration_ms"))
                .and_then(Value::as_u64),
            detail,
        });
        self.next_id = self.next_id.wrapping_add(1);
        self.retained_bytes += entry_size(&entry);
        self.entries.push_back(entry);
        self.evict();
        self.revision = self.revision.wrapping_add(1);
    }

    fn evict(&mut self) {
        while self.entries.len() > MAX_ITEMS || self.retained_bytes > MAX_RETAINED_BYTES {
            if let Some(evicted) = self.entries.pop_front() {
                self.retained_bytes -= entry_size(&evicted);
                self.dropped += 1;
            }
        }
    }
}

/// Entry retention counts metadata as well as captured content. The two
/// identity registries are separately bounded by count and string length.
fn entry_size(entry: &ActivityEntry) -> usize {
    std::mem::size_of::<ActivityEntry>()
        + entry.session_id.as_ref().map_or(0, String::capacity)
        + entry.session_label.capacity()
        + entry.category.capacity()
        + entry.event.capacity()
        + entry.summary.capacity()
        + entry.model.as_ref().map_or(0, String::capacity)
        + entry.detail.capacity()
}

fn correlation_keys(context: &Value) -> Vec<String> {
    ["stream_id", "request_id", "provider_run_id", "run_id"]
        .into_iter()
        .filter_map(|key| {
            let value = context.get(key)?.as_str()?;
            // Overlong identifiers cannot be allowed to grow the registry or
            // be truncated into another session's correlation key.
            (!value.is_empty() && value.len() <= 256).then(|| format!("{key}:{value}"))
        })
        .collect()
}

fn session_id(context: &Value) -> Option<String> {
    [
        context.get("conversation_id"),
        context.pointer("/tool/conversation_id"),
        context.pointer("/identifiers/client_conversation_id"),
        context.get("parent_conversation_id"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find(|id| !id.is_empty() && id.len() <= 256)
    .map(str::to_owned)
}

fn category(event: &str) -> &'static str {
    if event.contains("compact") || event.contains("summar") {
        "Compaction"
    } else if event.contains("risk_review")
        || event.contains("risk_assessment")
        || event.contains("permission")
    {
        "Permissions"
    } else if event.contains("tool") || event.contains("action") {
        "Tools"
    } else if event.contains("command") || event.contains("job") {
        "Background jobs"
    } else if event.contains("model") || event.contains("llm") {
        "Completions"
    } else {
        "Lifecycle"
    }
}

fn bounded_text(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Reuses existing operational metadata while keeping remote logging consent
/// and credentials independent from this local inspector.
pub(crate) fn record_log(record: &super::remote_logging::RemoteLogRecord) {
    if !is_recording() {
        return;
    }
    let event = record
        .context
        .get("event")
        .and_then(Value::as_str)
        .unwrap_or("activity");
    record_serializable(
        event,
        &record.message,
        json!({
            "conversation_id": session_id(&record.context),
            "stream_id": record.context.get("stream_id"),
            "request_id": record.context.get("request_id"),
            "provider_run_id": record.context.get("provider_run_id"),
            "run_id": record.context.get("run_id"),
            "duration_ms": record.context.get("duration_ms"),
            "model_id": record.context.get("model_id"),
            "elapsed_ms": record.context.get("elapsed_ms"),
            "level": record.level.as_str(),
        }),
        &record.context,
    );
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
