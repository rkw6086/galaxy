use command::blocking::Command;
use image::GenericImageView;
use objc2_core_graphics::{CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess};

use super::util::main_display_scale_factor;
use crate::{CapturedWindow, ScreenshotParams, Target};

const SCREEN_CAPTURE_PERMISSION_ERROR: &str = "macOS Screen Recording access is unavailable for Galaxy. Enable Galaxy in System Settings > Privacy & Security > Screen Recording (or Screen & System Audio Recording), then quit and reopen Galaxy. If already enabled after an update or reinstall, toggle Galaxy off and on and reopen it.";

/// Called only when the user initiates computer use, so macOS can register Galaxy and offer its
/// native permission prompt. A rejected request must not proceed to screen capture or input.
pub(super) fn request_access() -> Result<(), String> {
    // Some macOS versions need a relaunch after approval. The actor checks access again before
    // doing any work and returns the same recovery instructions if approval is not effective yet.
    if CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() {
        Ok(())
    } else {
        Err(SCREEN_CAPTURE_PERMISSION_ERROR.to_string())
    }
}

/// Checks without prompting before every action batch, including batches without a screenshot.
pub(super) fn check_access() -> Result<(), String> {
    if CGPreflightScreenCaptureAccess() {
        Ok(())
    } else {
        Err(SCREEN_CAPTURE_PERMISSION_ERROR.to_string())
    }
}

/// Captures a screenshot according to `params`, using the built-in macOS `screencapture` CLI.
///
/// When the params target a window, that specific window is captured (without raising it) and the
/// returned [`CapturedWindow`] describes the image so window-local coordinates can be mapped onto
/// it. Otherwise the main display is captured and the second tuple element is `None`.
pub fn take(
    params: ScreenshotParams,
) -> Result<(crate::Screenshot, Option<CapturedWindow>), String> {
    match params.target {
        Target::Window { window_id, .. } => take_window(window_id, params),
        Target::Screen => Ok((take_screen(params)?, None)),
    }
}

/// Captures the main display, optionally restricted to a region (legacy behavior).
fn take_screen(params: ScreenshotParams) -> Result<crate::Screenshot, String> {
    let output_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temporary directory for screenshot: {e}"))?;
    let output_path = output_dir.path().join("screenshot.png");

    let mut cmd = Command::new("/usr/sbin/screencapture");
    cmd.args([
        "-x",    // Do not play sounds.
        "-tpng", // Capture to PNG format.
        "-m",    // Only capture the main display (not all displays).
    ]);

    if let Some(region) = params.region {
        region.validate()?;
        // -R x,y,w,h captures a specific rectangle in point coordinates.
        // Convert from physical pixel coordinates to point coordinates.
        let scale = main_display_scale_factor();
        let x = (region.top_left.x() as f64 / scale) as i32;
        let y = (region.top_left.y() as f64 / scale) as i32;
        let w = ((region.bottom_right.x() - region.top_left.x()) as f64 / scale) as i32;
        let h = ((region.bottom_right.y() - region.top_left.y()) as f64 / scale) as i32;
        cmd.arg("-R").arg(format!("{x},{y},{w},{h}"));
    }

    let output = cmd
        .arg(&output_path)
        .output()
        .map_err(|e| format!("Failed to run screencapture: {e}"))?;

    check_status(&output, Target::Screen)?;

    crate::screenshot_utils::load_and_process_screenshot(&output_path, params)
}

/// Captures a single window by its `CGWindowID` without raising it, returning the processed image
/// plus metadata describing the captured pixels.
fn take_window(
    window_id: u32,
    params: ScreenshotParams,
) -> Result<(crate::Screenshot, Option<CapturedWindow>), String> {
    let output_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temporary directory for screenshot: {e}"))?;
    let output_path = output_dir.path().join("window.png");

    let output = Command::new("/usr/sbin/screencapture")
        .args([
            "-x",    // Do not play sounds.
            "-tpng", // Capture to PNG format.
            "-o",    // Omit the window's drop shadow.
        ])
        // -l <windowid> captures only the window with the given id, even when it is not frontmost.
        .arg("-l")
        .arg(window_id.to_string())
        .arg(&output_path)
        .output()
        .map_err(|e| format!("Failed to run screencapture: {e}"))?;

    check_status(&output, params.target)?;

    let image = image::ImageReader::open(&output_path)
        .map_err(|e| format!("Failed to open screenshot file: {e}"))?
        .decode()
        .map_err(|e| format!("Failed to decode screenshot: {e}"))?;
    let (full_width_px, full_height_px) = image.dimensions();
    let image = if let Some(region) = params.region {
        region.validate()?;
        if region.bottom_right.x() as u32 > full_width_px
            || region.bottom_right.y() as u32 > full_height_px
        {
            return Err(format!(
                "Screenshot region ({}, {}) to ({}, {}) is outside window {window_id} dimensions {full_width_px}x{full_height_px}.",
                region.top_left.x(),
                region.top_left.y(),
                region.bottom_right.x(),
                region.bottom_right.y(),
            ));
        }
        image.crop_imm(
            region.top_left.x() as u32,
            region.top_left.y() as u32,
            (region.bottom_right.x() - region.top_left.x()) as u32,
            (region.bottom_right.y() - region.top_left.y()) as u32,
        )
    } else {
        image
    };
    let screenshot = crate::screenshot_utils::process_screenshot(image, params)?;

    // The captured metadata refers to the native (pre-downscale) capture, so window-local pixel
    // coordinates sent by the agent map directly onto the captured window image.
    let captured = CapturedWindow {
        window_id,
        width_px: screenshot.original_width as i32,
        height_px: screenshot.original_height as i32,
    };
    Ok((screenshot, Some(captured)))
}

/// Returns an error describing a failed `screencapture` invocation.
fn check_status(output: &std::process::Output, target: Target) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = if stderr.trim().is_empty() {
        output.status.to_string()
    } else {
        format!("{}: {}", output.status, stderr.trim())
    };
    // A capture can still fail with permission granted, for example when the Mac is locked or
    // the selected window has closed. Preserve the OS error rather than labelling it a denial.
    let recovery = match target {
        Target::Screen => "Make sure the Mac is unlocked and has an active display.",
        Target::Window { .. } => "Make sure the selected window is still open and available.",
    };
    Err(format!(
        "macOS screen capture failed ({detail}). {recovery} If Screen Recording was recently enabled or Galaxy was reinstalled, quit and reopen Galaxy; verify that Screen Recording is enabled for the current Galaxy application."
    ))
}
