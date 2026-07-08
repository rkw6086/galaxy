use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::Local;

const CRASH_LOG_DIR: &str = "crash-logs";
const MAX_CRASH_LOGS: usize = 20;

fn crash_log_dir() -> Option<PathBuf> {
    galaxy_core::paths::galaxy_home_config_dir().map(|dir| dir.join(CRASH_LOG_DIR))
}

pub fn log_crash(
    error_type: &str,
    error_message: &str,
    model_id: &str,
    message_count: usize,
    context_tokens: Option<u32>,
) {
    let Some(dir) = crash_log_dir() else {
        log::warn!("[crash-log] Could not determine crash log directory");
        return;
    };

    if let Err(e) = fs::create_dir_all(&dir) {
        log::warn!("[crash-log] Failed to create crash log directory: {e}");
        return;
    }

    rotate_logs(&dir);

    let timestamp = Local::now();
    let filename = format!("crash_{}.log", timestamp.format("%Y%m%d_%H%M%S"));
    let path = dir.join(&filename);

    let content = format!(
        "=== Galaxy Crash Log ===\n\
         Timestamp: {}\n\
         Error Type: {}\n\
         Model: {}\n\
         Message Count: {}\n\
         Context Tokens: {}\n\
         \n\
         Error Details:\n\
         {}\n\
         ========================\n",
        timestamp.format("%Y-%m-%d %H:%M:%S %Z"),
        error_type,
        model_id,
        message_count,
        context_tokens
            .map(|t| t.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        error_message,
    );

    match OpenOptions::new().create(true).write(true).open(&path) {
        Ok(mut file) => {
            if let Err(e) = file.write_all(content.as_bytes()) {
                log::warn!("[crash-log] Failed to write crash log: {e}");
            } else {
                log::info!("[crash-log] Wrote crash log to {}", path.display());
            }
        }
        Err(e) => {
            log::warn!("[crash-log] Failed to open crash log file: {e}");
        }
    }
}

fn rotate_logs(dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut logs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash_") && n.ends_with(".log"))
        })
        .collect();

    logs.sort();

    while logs.len() >= MAX_CRASH_LOGS {
        if let Some(oldest) = logs.first() {
            let _ = fs::remove_file(oldest);
            logs.remove(0);
        }
    }
}
