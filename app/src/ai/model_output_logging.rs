//! Model response diagnostics for the opt-in full-session log.
//!
//! Model output can contain source code, file contents, reasoning, and tool arguments. Keep these
//! records behind the local filesystem logging setting rather than relying only on log levels,
//! which can also be changed through `RUST_LOG`.

const MODEL_OUTPUT_LOG_TARGET: &str = "galaxy::ai::model_output";

pub(crate) fn log(record: serde_json::Value) {
    if !galaxy_logging::session_log_file_path().is_ok_and(|path| path.is_some()) {
        return;
    }

    match serde_json::to_string(&record) {
        Ok(record) => log::info!(target: MODEL_OUTPUT_LOG_TARGET, "{record}"),
        Err(error) => {
            log::warn!(target: MODEL_OUTPUT_LOG_TARGET, "Failed to serialize model output: {error}")
        }
    }
}
