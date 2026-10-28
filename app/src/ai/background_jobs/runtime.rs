use std::collections::HashMap;
use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
#[cfg(unix)]
use std::time::Duration;

use command::r#async::Command;
#[cfg(unix)]
use galaxyui::r#async::Timer;
use parking_lot::Mutex;

use super::{read_tail_from_open_file, BackgroundJobLaunch, BackgroundJobStatus, MAX_OUTPUT_BYTES};
use crate::terminal::shell::{ShellLaunchData, ShellType};

pub(super) struct ProcessControl {
    #[cfg(not(windows))]
    pid: u32,
    state: Mutex<ProcessState>,
    #[cfg(windows)]
    job: std::os::windows::io::OwnedHandle,
}

struct ProcessState {
    live: bool,
    cancelled: bool,
}

impl ProcessControl {
    pub(super) fn new(pid: u32) -> io::Result<Self> {
        Ok(Self {
            #[cfg(not(windows))]
            pid,
            state: Mutex::new(ProcessState {
                live: true,
                cancelled: false,
            }),
            #[cfg(windows)]
            job: own_windows_process_tree(pid)?,
        })
    }

    pub(super) fn cancel_requested(&self) -> bool {
        self.state.lock().cancelled
    }

    pub(super) fn cancel(&self) -> io::Result<()> {
        // Keep checking ownership and signalling atomic with respect to cleanup/reaping.
        let mut state = self.state.lock();
        if !state.live {
            return Ok(());
        }
        self.terminate_tree()?;
        state.cancelled = true;
        Ok(())
    }

    pub(super) fn cleanup(&self) {
        let mut state = self.state.lock();
        if state.live {
            state.live = false;
            let _ = self.terminate_tree();
        }
    }

    #[cfg(unix)]
    pub(super) async fn wait_for_exit_without_reaping(&self) -> io::Result<()> {
        // Retain the async-process Child without polling status/try_status until cleanup.
        // Its background reaper only takes dropped Child guards. WNOWAIT reserves this
        // child's PID even after exit, so its process-group ID cannot be reused while a
        // cancellation or descendant cleanup can still signal that group.
        loop {
            let exited = {
                // A zero PID distinguishes WNOHANG's "still running" result on every platform.
                let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
                let result = unsafe {
                    libc::waitid(
                        libc::P_PID,
                        self.pid as libc::id_t,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    )
                };
                if result < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        // Another waiter unexpectedly consumed ownership. Never signal the
                        // numeric ID again, including from ProcessGuard's drop path.
                        self.state.lock().live = false;
                    } else {
                        self.cleanup();
                    }
                    return Err(error);
                }
                (unsafe { info.si_pid() }) == self.pid as libc::pid_t
            };
            if exited {
                // Kill any remaining descendants before the caller reaps the group leader.
                self.cleanup();
                return Ok(());
            }
            Timer::after(Duration::from_millis(100)).await;
        }
    }

    fn terminate_tree(&self) -> io::Result<()> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::JobObjects::TerminateJobObject;
            // The handle identifies this job even after its original shell process exits.
            return unsafe { TerminateJobObject(HANDLE(self.job.as_raw_handle()), 1) }
                .map_err(|error| io::Error::other(error.to_string()));
        }
        #[cfg(not(windows))]
        terminate_tree(self.pid)
    }
}

pub(super) struct ProcessGuard(pub Arc<ProcessControl>);
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.0.cleanup();
    }
}

#[cfg(unix)]
fn terminate_tree(pid: u32) -> io::Result<()> {
    // Every job starts a new session/process group; only that owned group is signalled.
    let result = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(windows)]
fn own_windows_process_tree(pid: u32) -> io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    let error = |error: windows::core::Error| io::Error::other(error.to_string());
    let raw_job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(error)?;
    // SAFETY: the new kernel handle is transferred into exactly one OwnedHandle.
    let job = unsafe { OwnedHandle::from_raw_handle(raw_job.0) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            HANDLE(job.as_raw_handle()),
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const std::ffi::c_void,
            std::mem::size_of_val(&limits) as u32,
        )
    }
    .map_err(error)?;
    let raw_process =
        unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid) }.map_err(error)?;
    // SAFETY: OpenProcess returned a new handle, released after assignment.
    let process = unsafe { OwnedHandle::from_raw_handle(raw_process.0) };
    unsafe {
        AssignProcessToJobObject(HANDLE(job.as_raw_handle()), HANDLE(process.as_raw_handle()))
    }
    .map_err(error)?;
    // The child was created suspended, so no command or descendant can escape ownership
    // between CreateProcess and assignment. Closing this job kills it if resuming fails.
    resume_windows_initial_thread(pid)?;
    Ok(job)
}

#[cfg(windows)]
fn resume_windows_initial_thread(pid: u32) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let error = |error: windows::core::Error| io::Error::other(error.to_string());
    let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }.map_err(error)?;
    // SAFETY: the successful snapshot call returned a new owned handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot.0) };
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    unsafe { Thread32First(HANDLE(snapshot.as_raw_handle()), &mut entry) }.map_err(error)?;
    loop {
        if entry.th32OwnerProcessID == pid {
            let raw_thread =
                unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID) }
                    .map_err(error)?;
            // SAFETY: OpenThread returned a new owned handle for this suspended process.
            let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread.0) };
            let previous_count = unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) };
            return match previous_count {
                1 => Ok(()),
                u32::MAX => Err(io::Error::last_os_error()),
                _ => Err(io::Error::other(
                    "Background command's initial thread had an unexpected suspension count",
                )),
            };
        }
        unsafe { Thread32Next(HANDLE(snapshot.as_raw_handle()), &mut entry) }.map_err(error)?;
    }
}

#[cfg(not(any(unix, windows)))]
fn terminate_tree(pid: u32) -> io::Result<()> {
    Err(io::Error::other(format!(
        "Terminating background process {pid} is unsupported"
    )))
}

pub(super) struct JobOutcome {
    pub status: BackgroundJobStatus,
    pub exit_code: Option<i32>,
    pub output: String,
    pub log_bytes: u64,
    pub error: Option<String>,
}

impl JobOutcome {
    pub(super) fn launch_error(error: String) -> Self {
        Self {
            status: BackgroundJobStatus::Failed,
            exit_code: None,
            output: String::new(),
            log_bytes: 0,
            error: Some(error),
        }
    }

    pub(super) fn from_exit(
        result: io::Result<ExitStatus>,
        cancelled: bool,
        log: &mut std::fs::File,
    ) -> Self {
        let (output, log_bytes, log_error) = match read_tail_from_open_file(log, MAX_OUTPUT_BYTES) {
            Ok((output, bytes)) => (output, bytes, None),
            Err(error) => (
                String::new(),
                0,
                Some(format!("Could not read command logs: {error}")),
            ),
        };
        match result {
            Ok(exit) => {
                #[cfg(unix)]
                let code = {
                    use std::os::unix::process::ExitStatusExt;
                    exit.code()
                        .or_else(|| exit.signal().map(|signal| 128 + signal))
                };
                #[cfg(not(unix))]
                let code = exit.code();
                let status = if cancelled {
                    BackgroundJobStatus::Cancelled
                } else if exit.success() {
                    BackgroundJobStatus::Succeeded
                } else {
                    BackgroundJobStatus::Failed
                };
                Self {
                    status,
                    exit_code: code,
                    output,
                    log_bytes,
                    error: log_error,
                }
            }
            Err(error) => Self {
                status: if cancelled {
                    BackgroundJobStatus::Cancelled
                } else {
                    BackgroundJobStatus::Failed
                },
                exit_code: None,
                output,
                log_bytes,
                error: Some(error.to_string()),
            },
        }
    }
}

pub(super) fn command_for_launch(launch: &BackgroundJobLaunch) -> Result<Command, String> {
    let (program, shell_type) = match &launch.shell {
        Some(ShellLaunchData::Executable { executable_path, shell_type })
        | Some(ShellLaunchData::MSYS2 { executable_path, shell_type }) => (executable_path.clone(), Some(*shell_type)),
        Some(ShellLaunchData::WSL { .. }) => return Err("Background jobs cannot yet launch in this WSL session. Run the command yourself in that session.".to_owned()),
        Some(ShellLaunchData::DockerSandbox { .. }) => return Err("Background jobs cannot run container commands on the host. Run this command yourself inside the sandbox.".to_owned()),
        None => {
            #[cfg(unix)]
            { (std::path::PathBuf::from("/bin/sh"), None) }
            #[cfg(windows)]
            { (std::path::PathBuf::from(std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into())), None) }
            #[cfg(not(any(unix, windows)))]
            { return Err("Background shell execution is unavailable on this platform".to_owned()); }
        }
    };
    #[cfg(unix)]
    let mut command = Command::new_with_session(&program);
    #[cfg(not(unix))]
    let mut command = Command::new_with_process_group(&program);
    #[cfg(windows)]
    command.creation_flags(windows::Win32::System::Threading::CREATE_SUSPENDED.0);
    match shell_type {
        Some(ShellType::Bash) => {
            command.args(["--noprofile", "--norc", "-c"]);
        }
        Some(ShellType::Zsh) => {
            command.args(["-f", "-c"]);
        }
        Some(ShellType::Fish) => {
            command.args(["--no-config", "-c"]);
        }
        Some(ShellType::PowerShell) => {
            command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
        }
        None => {
            #[cfg(unix)]
            command.arg("-c");
            #[cfg(windows)]
            {
                use command::windows::CommandExt;
                command.args(["/D", "/Q", "/C"]).raw_arg(&launch.command);
            }
        }
    }
    if shell_type.is_some() || cfg!(unix) {
        command.arg(&launch.command);
    }
    let mut environment = HashMap::from([
        ("CI", "1"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("GH_PAGER", "cat"),
        ("SYSTEMD_PAGER", "cat"),
        ("AWS_PAGER", ""),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GCM_INTERACTIVE", "Never"),
        ("SSH_ASKPASS_REQUIRE", "never"),
    ]);
    if let Some(snapshot) = &launch.environment {
        // The shell integration supplies its complete exported environment. Inheriting
        // the app's environment here would restore variables explicitly unset in that shell.
        command.env_clear().envs(snapshot);
    }
    command.envs(environment.drain());
    command.env_remove("SSH_ASKPASS").env_remove("SUDO_ASKPASS");
    command.stdin(command::Stdio::null());
    // On Unix ProcessGuard kills the group before dropping/reaping Child. A separate
    // kill-on-drop path could otherwise signal a PID after ownership was lost (ECHILD).
    #[cfg(not(unix))]
    command.kill_on_drop(true);
    Ok(command)
}
