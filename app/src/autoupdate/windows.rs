use std::fs::File;
use std::io::Write as _;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::{fs, io};

use anyhow::{anyhow, bail, Result};
use channel_versions::VersionInfo;
use command::blocking::Command;
use galaxy_core::channel::{Channel, ChannelState};
use galaxyui::AppContext;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use tempfile::{TempDir, TempPath};

use super::{release_assets_directory_url, DownloadReady};
use crate::server::telemetry::TelemetryEvent;
use crate::util::windows::install_dir;

lazy_static! {
    /// The path to the temporary file that stores the installer for the new update.
    static ref INSTALLER_PATH: Arc<Mutex<Option<TempPath>>> = Default::default();
    static ref GALAXY_INSTALLER: Mutex<Option<GalaxyInstaller>> = Mutex::new(None);
}

struct GalaxyInstaller {
    // Keep the ownership lock for the entire candidate lifetime. A fresh
    // process can reclaim a completed handoff only after this handle closes.
    _owner: File,
    directory: Option<TempDir>,
    path: PathBuf,
    version: VersionInfo,
}

impl Drop for GalaxyInstaller {
    fn drop(&mut self) {
        // Cleanup below preserves owner.lock until every other file is gone.
        // TempDir's recursive drop could remove that marker before encountering
        // a still-running installer or scanner's Windows file lock.
        if let Some(directory) = self.directory.take() {
            let _ = directory.keep();
        }
    }
}

fn installer_cache_directory() -> Result<PathBuf> {
    let directory = galaxy_core::paths::cache_dir().join("github-update-installers");
    fs::create_dir_all(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        bail!("Galaxy installer cache must be a regular directory");
    }
    Ok(directory)
}

fn cleanup_completed_galaxy_installers() {
    let Ok(directory) = installer_cache_directory() else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.take(64).flatten() {
        let name = entry.file_name();
        let Some(suffix) = name
            .to_str()
            .and_then(|name| name.strip_prefix("installer-"))
        else {
            continue;
        };
        if !(6..=64).contains(&suffix.len())
            || !suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            continue;
        }
        let owner = path.join("owner.lock");
        let Ok(metadata) = fs::symlink_metadata(&owner) else {
            continue;
        };
        if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
            continue;
        }
        let Ok(lock) = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&owner)
        else {
            continue;
        };
        let Ok(children) = fs::read_dir(&path) else {
            continue;
        };
        let children: Vec<_> = children.take(17).collect();
        if children.len() > 16 {
            continue;
        }
        let regular = children
            .into_iter()
            .map(|entry| {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
                    return Err(io::Error::other("Unexpected installer cache entry"));
                }
                Ok(entry.path())
            })
            .collect::<io::Result<Vec<_>>>();
        let Ok(files) = regular else {
            continue;
        };
        // The executable can remain locked by Inno Setup after Galaxy exits.
        // Preserve the marker on any failure so a later cleanup can retry.
        if files
            .iter()
            .filter(|file| **file != owner)
            .any(|file| fs::remove_file(file).is_err())
        {
            continue;
        }
        drop(lock);
        if fs::remove_file(&owner).is_ok() {
            let _ = fs::remove_dir(&path);
        }
    }
}

/// Download the Inno Setup install wizard, the same one users run on the first Warp install, and
/// place it into the "data dir".
pub(super) async fn download_update_and_cleanup(
    version_info: &VersionInfo,
    _update_id: &str,
    client: &http_client::Client,
) -> Result<DownloadReady> {
    if version_info.galaxy_update.is_some() {
        cleanup_completed_galaxy_installers();
        let directory = tempfile::Builder::new()
            .prefix("installer-")
            .tempdir_in(installer_cache_directory()?)?;
        let path = directory.path().join("GalaxySetup.exe");
        let owner = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(directory.path().join("owner.lock"))?;
        let installer = GalaxyInstaller {
            _owner: owner,
            directory: Some(directory),
            path,
            version: version_info.clone(),
        };
        super::github::download_verified(version_info, &installer.path).await?;
        // Keep the last verified installer until its replacement is complete.
        *GALAXY_INSTALLER.lock() = Some(installer);
        cleanup_completed_galaxy_installers();
        return Ok(DownloadReady::Yes);
    }
    if super::github::is_galaxy_updater() {
        bail!("Galaxy updates require signed release metadata");
    }
    const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

    let installer_file_name = installer_file_name()?;
    let url = format!(
        "{}/{}",
        release_assets_directory_url(ChannelState::channel(), &version_info.version),
        installer_file_name
    );

    // Create a temporary file that we'll write the download into.
    let mut already_exists = false;
    let mut new_installer = tempfile::Builder::new()
        .rand_bytes(0)
        .suffix(&format!("{}-{}", version_info.version, installer_file_name))
        .make(|path| {
            // Treat a 0-byte file as missing.
            let non_empty = path.metadata().map(|m| m.len() > 0).unwrap_or(false);
            already_exists = non_empty;
            if already_exists {
                File::open(path)
            } else {
                File::create(path)
            }
        })?;

    if !already_exists {
        log::info!("Downloading {url} to {}...", new_installer.path().display());

        let response = client
            .get(&url)
            .timeout(DOWNLOAD_TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        new_installer
            .as_file_mut()
            .write_all(&response.bytes().await?)?;
    }

    *INSTALLER_PATH.lock() = Some(new_installer.into_temp_path());

    Ok(DownloadReady::Yes)
}

const UPDATE_LOG_FILENAME: &str = "warp_update.log";

fn autoupdate_log_file() -> Result<PathBuf> {
    galaxy_logging::log_directory().map(|dir| dir.join(UPDATE_LOG_FILENAME))
}

fn parse_exit_code_after_marker(contents_lowercase: &[u8], failed_marker: &[u8]) -> Option<i32> {
    const EXIT_CODE_MARKER: &[u8] = b"exit code: ";

    let failed_pos = memchr::memmem::find(contents_lowercase, failed_marker)?;
    let after_failed = &contents_lowercase[failed_pos..];
    let marker_pos = memchr::memmem::find(after_failed, EXIT_CODE_MARKER)?;
    let after_marker = &after_failed[marker_pos + EXIT_CODE_MARKER.len()..];
    let sign_len = if after_marker.first() == Some(&b'-') {
        1
    } else {
        0
    };
    let digit_len = after_marker[sign_len..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digit_len == 0 {
        return None;
    }
    std::str::from_utf8(&after_marker[..sign_len + digit_len])
        .ok()?
        .parse()
        .ok()
}

/// Parses the taskkill exit code from an Inno Setup log containing a
/// "force-kill failed for" line. Returns `None` if no such line is found or
/// the exit code cannot be parsed.
fn parse_forcekill_exit_code(contents_lowercase: &[u8]) -> Option<i32> {
    const FAILED_MARKER: &[u8] = b"force-kill failed for";
    parse_exit_code_after_marker(contents_lowercase, FAILED_MARKER)
}

/// Parses the PowerShell exit code from an Inno Setup log containing a
/// "minidump-server cleanup failed" line. Returns `None` if no such line is
/// found or the exit code cannot be parsed.
fn parse_minidump_cleanup_exit_code(contents_lowercase: &[u8]) -> Option<i32> {
    const FAILED_MARKER: &[u8] = b"minidump-server cleanup failed";
    parse_exit_code_after_marker(contents_lowercase, FAILED_MARKER)
}

/// Checks the autoupdate log file from a previous update attempt.
/// Sends telemetry for specific known issues.
/// The log file is renamed after processing to avoid duplicate reports on subsequent launches.
pub(super) fn check_and_report_update_errors(ctx: &mut AppContext) {
    let log_path = match autoupdate_log_file() {
        Ok(path) => path,
        Err(e) => {
            log::warn!("Failed to determine autoupdate log file path: {e:#}");
            return;
        }
    };

    // Inno Setup logs use the system's active codepage (often Windows-1252), not UTF-8.
    // We read as raw bytes to avoid silently skipping non-UTF-8 log files.
    let contents = match fs::read(&log_path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            log::info!("No autoupdate logs found");
            return;
        }
        Err(e) => {
            log::warn!("Failed to read autoupdate log file: {e:#}");
            return;
        }
    };

    let contents_lowercase = contents.to_ascii_lowercase();

    let has_unable_to_close = memchr::memmem::find(
        &contents_lowercase,
        b"setup was unable to automatically close all applications",
    )
    .is_some();
    if has_unable_to_close {
        crate::send_telemetry_sync_from_app_ctx!(
            TelemetryEvent::AutoupdateUnableToCloseApplications,
            ctx
        );
    }

    let has_file_in_use = memchr::memmem::find(
        &contents_lowercase,
        b"the process cannot access the file because it is being used by another process",
    )
    .is_some();
    if has_file_in_use {
        crate::send_telemetry_sync_from_app_ctx!(TelemetryEvent::AutoupdateFileInUse, ctx);
    }

    // Fired when the mutex polling loop timed out and a force-kill was attempted.
    let has_mutex_timeout =
        memchr::memmem::find(&contents_lowercase, b"warp mutex still held after timeout").is_some();
    if has_mutex_timeout {
        crate::send_telemetry_sync_from_app_ctx!(TelemetryEvent::AutoupdateMutexTimeout, ctx);
    }

    // Fired when taskkill returned non-zero after the mutex timeout.
    // Exit code 128 means "no matching process found" — the process was already
    // gone when taskkill ran — so suppress that harmless race condition.
    if let Some(exit_code) = parse_forcekill_exit_code(&contents_lowercase) {
        if exit_code != 128 {
            crate::send_telemetry_sync_from_app_ctx!(
                TelemetryEvent::AutoupdateForcekillFailed { exit_code },
                ctx
            );
        }
    }

    // Fired when the PowerShell cleanup of the orphaned minidump server process
    // returned a non-zero exit code.
    if let Some(exit_code) = parse_minidump_cleanup_exit_code(&contents_lowercase) {
        crate::send_telemetry_sync_from_app_ctx!(
            TelemetryEvent::AutoupdateMinidumpCleanupFailed { exit_code },
            ctx
        );
    }

    // Rename the log file after processing. Keep it around so the user can inspect it.
    let reported_path = log_path.with_extension("log.reported");
    if let Err(e) = fs::rename(&log_path, &reported_path) {
        log::warn!("Failed to rename autoupdate log file after reporting: {e:#}");
    }
}

pub(super) fn relaunch() -> Result<()> {
    let install_dir = install_dir()?;
    let installer_path = if super::github::is_galaxy_updater() {
        let pending = GALAXY_INSTALLER.lock();
        let installer = pending
            .as_ref()
            .ok_or_else(|| anyhow!("No verified Galaxy installer"))?;
        super::github::verify_download(&installer.version, &installer.path)?;
        installer.path.clone()
    } else {
        INSTALLER_PATH
            .lock()
            .as_ref()
            .map(|path| path.to_path_buf())
            .ok_or_else(|| anyhow!("No installer path"))?
    };

    let log_arg = match autoupdate_log_file() {
        Ok(dir) => format!("/LOG={}", dir.display()),
        Err(e) => {
            log::warn!("Failed to determine location for autoupdate logs: {e:#}");
            "/LOG".to_string()
        }
    };

    // The Inno Setup install wizard will run without user input. It will re-launch Warp after
    // installing the update files.
    // https://jrsoftware.org/ishelp/index.php?topic=setupcmdline
    Command::new(&installer_path)
        .args([
            // Skip asking the user to confirm.
            "/SP-",
            // Do not prompt the user for anything. Note that we do not use "VERYSILENT" so that a
            // progress bar is still shown. This is useful since the update process may take a few
            // seconds.
            "/SILENT",
            // Do not provide a cancel button on the progress bar page.
            "/NOCANCEL",
            // Indicate that restarting Windows is not necessary.
            "/NORESTART",
            &log_arg,
            "/update=1",
            // Do not forcibly kill Warp via RestartManager. The installer will wait for
            // Warp to exit naturally by polling the single-instance mutex instead.
            "/NOCLOSEAPPLICATIONS",
            &format!("/DIR={}", install_dir.display()),
        ])
        .spawn()?;

    Ok(())
}

fn installer_file_name() -> Result<String> {
    let app_name_prefix = app_name_prefix(ChannelState::channel());

    // For example, on arm64 this is WarpSetup-arm64.exe and on x64 this is
    // WarpSetup.exe.
    if cfg!(target_arch = "aarch64") {
        Ok(format!("{app_name_prefix}Setup-arm64.exe"))
    } else if cfg!(target_arch = "x86_64") {
        Ok(format!("{app_name_prefix}Setup.exe"))
    } else {
        Err(anyhow!(
            "Could not construct setup file name for unsupported architecture"
        ))
    }
}

fn app_name_prefix(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "Galaxy",
        Channel::Preview => "GalaxyPreview",
        Channel::Local => "galaxy-ai",
        Channel::Integration => "integration",
        Channel::Dev => "GalaxyDev",
        Channel::Oss => "galaxy-ai-oss",
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
