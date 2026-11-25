//! Optional, device-local guidance for macOS Full Disk Access.
//!
//! macOS requires the user to grant this permission in System Settings. The
//! stored marker records dismissal of the guidance, never authorization.
//! A conservative filesystem preflight checks effective protected-file access;
//! macOS provides no supported API for querying the Full Disk Access toggle.

use galaxy_core::user_preferences::GetUserPreferences;
use galaxyui::modals::{AlertDialogWithCallbacks, ModalButton};
use galaxyui::AppContext;

const PROMPT_DISMISSED_KEY: &str = "FullDiskAccessPromptDismissed";
const SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProtectedFileAccess {
    Accessible,
    Denied,
    Unknown,
}

/// Checks access after the macOS GUI has opened, unless reminders were disabled.
pub(crate) fn prompt_on_startup(ctx: &mut AppContext) {
    if !cfg!(target_os = "macos") || prompt_dismissed(ctx) {
        return;
    }

    let (sender, receiver) = futures::channel::oneshot::channel();
    ctx.background_executor()
        .spawn(async move {
            let _ = sender.send(protected_file_access());
        })
        .detach();

    let weak_app = ctx.weak_app();
    ctx.foreground_executor()
        .spawn(async move {
            if receiver.await.ok() != Some(ProtectedFileAccess::Denied) {
                return;
            }
            if let Some(mut app) = weak_app.upgrade() {
                app.update(|ctx| {
                    if ctx.window_ids().next().is_some() && !prompt_dismissed(ctx) {
                        show_guidance(ctx);
                    }
                });
            }
        })
        .detach();
}

fn prompt_dismissed(ctx: &AppContext) -> bool {
    match ctx
        .private_user_preferences()
        .read_value(PROMPT_DISMISSED_KEY)
    {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(error) => {
            log::warn!("Could not read Full Disk Access guidance preference: {error}");
            true
        }
    }
}

/// This is an effective-access heuristic, not a supported permission query.
/// Open only the current user's database; never inspect or parse its contents.
/// Missing files and ambiguous failures must not produce a permission prompt.
fn protected_file_access() -> ProtectedFileAccess {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;

        let Some(home) = dirs::home_dir() else {
            return ProtectedFileAccess::Unknown;
        };
        let path = home.join("Library/Application Support/com.apple.TCC/TCC.db");
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => {
                let is_regular_file = file.metadata().is_ok_and(|metadata| metadata.is_file());
                drop(file);
                if is_regular_file {
                    ProtectedFileAccess::Accessible
                } else {
                    ProtectedFileAccess::Unknown
                }
            }
            // EACCES may reflect ordinary POSIX permissions or ACLs, so only
            // EPERM counts as protected-access denial. Neither is proof of the
            // setting's value; all other errors remain unknown.
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => ProtectedFileAccess::Denied,
            Err(_) => ProtectedFileAccess::Unknown,
        }
    }
    #[cfg(not(unix))]
    {
        ProtectedFileAccess::Unknown
    }
}

fn show_guidance(ctx: &mut AppContext) {
    ctx.show_native_platform_modal(AlertDialogWithCallbacks::for_app(
        "Protected-file access is unavailable",
        "Full Disk Access is optional. Enabling it gives Galaxy, including its agents and \
         commands you run, broad access to protected files on this Mac.\n\n\
         To enable it, open System Settings > Privacy & Security > Full Disk Access and \
         turn on Galaxy. If Galaxy is missing, click + and select the installed Galaxy app. \
         macOS may ask you to quit and reopen Galaxy.\n\n\
         You can continue without this permission and reopen these settings from Galaxy's menu later.",
        vec![
            ModalButton::for_app("Open System Settings", open_settings),
            ModalButton::for_app("Not Now", |_| {}),
        ],
        remember_dismissal,
    ));
}

/// Opens the macOS settings page without claiming or changing authorization.
pub(crate) fn open_settings(ctx: &mut AppContext) {
    if cfg!(target_os = "macos") {
        ctx.open_url(SETTINGS_URL);
    }
}

fn remember_dismissal(ctx: &mut AppContext) {
    if let Err(error) = ctx
        .private_user_preferences()
        .write_value(PROMPT_DISMISSED_KEY, true.to_string())
    {
        log::warn!("Could not save Full Disk Access guidance preference: {error}");
    }
}
