use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
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

const BACKGROUND_ENV_PREFIX: &str = "_GALAXY_BACKGROUND_JOB_";
const POWERSHELL_SETUP_KEY: &str = "_GALAXY_BACKGROUND_JOB_SETUP_KEY";
const NONINTERACTIVE_ENVIRONMENT: &[(&str, &str)] = &[
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
];

// Commands are passed as untouched data to constant setup scripts and execute
// noninteractively in the owned process tree. Bash/fish use a profile bootstrap;
// zsh keeps rc-defined shell state in its noninteractive command worker.
// Private environment shuttles preserve explicit overrides through rc files
// without exposing their values in argv, files, or captured environment output.
const BASH_PROFILE_SETUP: &str = r#"
builtin set +xv
builtin set +m
__galaxy_job_cwd=$1
__galaxy_job_prefix=$2
__galaxy_job_count=$3
builtin shift 3
while [ "$__galaxy_job_count" -gt 0 ]; do
    __galaxy_job_key=$1
    __galaxy_job_slot="$__galaxy_job_prefix$__galaxy_job_key"
    builtin declare -p "$__galaxy_job_slot" >/dev/null 2>&1 || exit 126
    builtin export "$__galaxy_job_key=${!__galaxy_job_slot}" || exit 126
    builtin unset "$__galaxy_job_slot"
    builtin shift
    __galaxy_job_count=$((__galaxy_job_count - 1))
done
# BASH_ENV/ENV would initialize another profile in the noninteractive child.
builtin unset SSH_ASKPASS SUDO_ASKPASS BASH_ENV ENV
builtin cd -- "$__galaxy_job_cwd" || exit 126
builtin exec -- "$@"
"#;

const ZSH_PROFILE_SETUP: &str = r#"
builtin unsetopt XTRACE VERBOSE MONITOR
__galaxy_job_cwd=$1
__galaxy_job_prefix=$2
__galaxy_job_count=$3
builtin shift 3
__galaxy_job_keys=()
while [ "$__galaxy_job_count" -gt 0 ]; do
    __galaxy_job_keys+=("$1")
    builtin shift
    __galaxy_job_count=$((__galaxy_job_count - 1))
done
__galaxy_job_command=$1
builtin set --
# Keep rc-defined functions, aliases and unexported state in the command worker.
# Login files are loaded by zsh; noninteractive shells need an explicit zshrc.
__galaxy_job_rc="${ZDOTDIR:-$HOME}/.zshrc"
if [[ -r "$__galaxy_job_rc" ]]; then
    builtin source "$__galaxy_job_rc"
fi
builtin unsetopt XTRACE VERBOSE MONITOR
builtin set --
for __galaxy_job_key in "${__galaxy_job_keys[@]}"; do
    __galaxy_job_slot="$__galaxy_job_prefix$__galaxy_job_key"
    builtin typeset -p "$__galaxy_job_slot" >/dev/null 2>&1 || exit 126
    builtin export "$__galaxy_job_key=${(P)__galaxy_job_slot}" || exit 126
    builtin unset "$__galaxy_job_slot"
done
builtin unset SSH_ASKPASS SUDO_ASKPASS BASH_ENV ENV
builtin cd -- "$__galaxy_job_cwd" || exit 126
builtin eval -- "$__galaxy_job_command"
"#;

const FISH_PROFILE_SETUP: &str = r#"
builtin set -e fish_trace
builtin set -l __galaxy_job_cwd "$argv[1]"
builtin set -l __galaxy_job_prefix "$argv[2]"
builtin set -l __galaxy_job_count "$argv[3]"
builtin set -e argv[1..3]
while builtin test "$__galaxy_job_count" -gt 0
    builtin set -l __galaxy_job_key "$argv[1]"
    builtin set -l __galaxy_job_slot "$__galaxy_job_prefix$__galaxy_job_key"
    builtin set -q "$__galaxy_job_slot"; or exit 126
    builtin set -gx "$__galaxy_job_key" "$$__galaxy_job_slot"; or exit 126
    builtin set -e "$__galaxy_job_slot"
    builtin set -e argv[1]
    builtin set __galaxy_job_count (builtin math "$__galaxy_job_count - 1")
end
for __galaxy_job_key in SSH_ASKPASS SUDO_ASKPASS BASH_ENV ENV
    builtin set -e "$__galaxy_job_key"
end
builtin cd -- "$__galaxy_job_cwd"; or exit 126
exec $argv
"#;

// PowerShell loads profiles in the noninteractive bootstrap, then starts a
// profile-free command host. A real host boundary preserves the original
// command's exit semantics, including nonterminating errors and explicit exit.
const POWERSHELL_PROFILE_SETUP: &str = r#"
Microsoft.PowerShell.Core\Set-PSDebug -Off
try {
    $__galaxyJobKey = [Environment]::GetEnvironmentVariable('_GALAXY_BACKGROUND_JOB_SETUP_KEY', 'Process')
    if ([String]::IsNullOrEmpty($__galaxyJobKey)) { throw 'Missing background job setup' }
    $__galaxyJobPayload = [Environment]::GetEnvironmentVariable($__galaxyJobKey, 'Process')
    [Environment]::SetEnvironmentVariable($__galaxyJobKey, $null, 'Process')
    [Environment]::SetEnvironmentVariable('_GALAXY_BACKGROUND_JOB_SETUP_KEY', $null, 'Process')
    $__galaxyJob = ConvertFrom-Json -InputObject $__galaxyJobPayload -ErrorAction Stop
    $__galaxyJobPayload = $null
    foreach ($__galaxyJobEntry in $__galaxyJob.environment.PSObject.Properties) {
        [Environment]::SetEnvironmentVariable($__galaxyJobEntry.Name, [String]$__galaxyJobEntry.Value, 'Process')
    }
    [Environment]::SetEnvironmentVariable('SSH_ASKPASS', $null, 'Process')
    [Environment]::SetEnvironmentVariable('SUDO_ASKPASS', $null, 'Process')
    Set-Location -LiteralPath $__galaxyJob.cwd -ErrorAction Stop
    $__galaxyJobProgram = [String]$__galaxyJob.program
    $__galaxyJobCommand = [String]$__galaxyJob.encoded_command
    $__galaxyJob = $null
} catch {
    [Console]::Error.WriteLine('Galaxy could not initialize the background command shell.')
    exit 126
}
$PSNativeCommandUseErrorActionPreference = $false
$LASTEXITCODE = $null
& $__galaxyJobProgram -NoLogo -NoProfile -NonInteractive -EncodedCommand $__galaxyJobCommand
if ($null -eq $LASTEXITCODE) { exit 126 }
exit $LASTEXITCODE
"#;

pub(super) fn command_for_launch(
    launch: &BackgroundJobLaunch,
    approved_directory: &Path,
) -> Result<Command, String> {
    let (program, shell_type) = match &launch.shell {
        Some(ShellLaunchData::Executable { executable_path, shell_type })
        | Some(ShellLaunchData::MSYS2 { executable_path, shell_type }) => (executable_path.clone(), Some(*shell_type)),
        Some(ShellLaunchData::WSL { .. }) => return Err("Background jobs cannot yet launch in this WSL session. Run the command yourself in that session.".to_owned()),
        Some(ShellLaunchData::DockerSandbox { .. }) => return Err("Background jobs cannot run container commands on the host. Run this command yourself inside the sandbox.".to_owned()),
        None => {
            #[cfg(unix)]
            { fallback_unix_shell(std::env::var_os("SHELL")) }
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
    let environment = background_environment(launch.environment.as_ref(), cfg!(windows))?;
    // Precmd currently provides partial metadata (for example KUBECONFIG), not
    // a complete export snapshot. Preserve the host's HOME, PATH and other vars.
    if let Some(overrides) = &launch.environment {
        command.envs(overrides);
    }
    // CI/TERM guards can deliberately skip rc initialization. Apply the full
    // job environment after profiles, while suppressing auth prompts throughout.
    command
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never");
    match shell_type {
        Some(kind @ (ShellType::Bash | ShellType::Zsh | ShellType::Fish)) => {
            configure_profile_bootstrap(
                &mut command,
                launch,
                approved_directory,
                &program,
                kind,
                &environment,
            )?;
        }
        Some(ShellType::PowerShell) => {
            configure_powershell_bootstrap(
                &mut command,
                launch,
                approved_directory,
                &program,
                &environment,
            )?;
        }
        None => {
            command.envs(&environment);
            #[cfg(unix)]
            command.arg("-c").arg(&launch.command);
            #[cfg(windows)]
            {
                use command::windows::CommandExt;
                command.args(["/D", "/Q", "/C"]).raw_arg(&launch.command);
            }
        }
    }
    command.env_remove("SSH_ASKPASS").env_remove("SUDO_ASKPASS");
    command.stdin(command::Stdio::null());
    // On Unix ProcessGuard kills the group before dropping/reaping Child. A separate
    // kill-on-drop path could otherwise signal a PID after ownership was lost (ECHILD).
    #[cfg(not(unix))]
    command.kill_on_drop(true);
    Ok(command)
}

#[cfg(unix)]
fn fallback_unix_shell(host_shell: Option<OsString>) -> (PathBuf, Option<ShellType>) {
    if let Some(shell) = host_shell.filter(|shell| Path::new(shell).is_absolute()) {
        if let Some(kind) = shell.to_str().and_then(ShellType::from_name) {
            return (PathBuf::from(shell), Some(kind));
        }
    }
    (PathBuf::from("/bin/sh"), None)
}

fn background_environment(
    overrides: Option<&HashMap<String, String>>,
    case_insensitive: bool,
) -> Result<HashMap<String, String>, String> {
    let mut environment = overrides.cloned().unwrap_or_default();
    if environment
        .iter()
        .any(|(name, value)| !valid_override_name(name) || value.contains('\0'))
    {
        return Err("The background command has an invalid environment override".to_owned());
    }
    for (name, value) in NONINTERACTIVE_ENVIRONMENT {
        if case_insensitive {
            environment.retain(|key, _| !key.eq_ignore_ascii_case(name));
        }
        environment.insert((*name).to_owned(), (*value).to_owned());
    }
    Ok(environment)
}

fn valid_override_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let valid_start = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    valid_start
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && !name.to_ascii_uppercase().starts_with(BACKGROUND_ENV_PREFIX)
        && !name.to_ascii_lowercase().starts_with("__galaxy_job_")
        && name != "argv"
        && name != "fish_trace"
}

fn configure_profile_bootstrap(
    command: &mut Command,
    launch: &BackgroundJobLaunch,
    approved_directory: &Path,
    program: &Path,
    shell_type: ShellType,
    environment: &HashMap<String, String>,
) -> Result<(), String> {
    let msys = matches!(&launch.shell, Some(ShellLaunchData::MSYS2 { .. }));
    let prefix = format!("{BACKGROUND_ENV_PREFIX}{}_", uuid::Uuid::new_v4().simple());
    let mut keys = environment.keys().collect::<Vec<_>>();
    keys.sort();
    let (setup, inner_flags): (&str, &[&str]) = match shell_type {
        ShellType::Bash => (BASH_PROFILE_SETUP, &["--noprofile", "--norc", "-c"]),
        ShellType::Zsh => (ZSH_PROFILE_SETUP, &[]),
        ShellType::Fish => (FISH_PROFILE_SETUP, &["--no-config", "-c"]),
        ShellType::PowerShell => return Err("Invalid shell bootstrap selection".to_owned()),
    };
    if shell_type == ShellType::Zsh {
        // Source zshrc and execute in one noninteractive process. A second zsh
        // would discard rc-defined functions such as user command wrappers.
        command.args(["-l", "-c", setup]);
        command.arg(shell_path_argument(program, msys)?);
    } else {
        command.args(["-i", "-l"]);
        if shell_type == ShellType::Bash {
            // No terminal is attached: profile initialization must not create
            // separate interactive job-control groups before the owned exec.
            command.arg("+m");
        }
        command.args(["-c", setup]);
        // Bash reserves this as $0; fish uses every following argument as $argv.
        if shell_type == ShellType::Bash {
            command.arg("galaxy-background-job");
        }
    }
    command.arg(shell_path_argument(approved_directory, msys)?);
    command.arg(&prefix).arg(keys.len().to_string());
    for key in keys {
        command.arg(key);
        command.env(format!("{prefix}{key}"), &environment[key]);
    }
    if shell_type != ShellType::Zsh {
        command.arg(shell_path_argument(program, msys)?);
        command.args(inner_flags);
    }
    command.arg(&launch.command);
    Ok(())
}

fn shell_path_argument(path: &Path, msys: bool) -> Result<OsString, String> {
    if !msys {
        return Ok(path.as_os_str().to_owned());
    }
    let value = path
        .to_str()
        .ok_or("The MSYS2 background command path is not valid Unicode")?;
    let value = if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(value).to_owned()
    };
    Ok(galaxy_util::path::convert_windows_path_to_msys2(&value).into())
}

fn configure_powershell_bootstrap(
    command: &mut Command,
    launch: &BackgroundJobLaunch,
    approved_directory: &Path,
    program: &Path,
    environment: &HashMap<String, String>,
) -> Result<(), String> {
    let cwd = approved_directory
        .to_str()
        .ok_or("The PowerShell command directory is not valid Unicode")?;
    let program = program
        .to_str()
        .ok_or("The PowerShell executable path is not valid Unicode")?;
    let payload = serde_json::to_string(&serde_json::json!({
        "program": program,
        "encoded_command": encode_powershell_command(&launch.command),
        "cwd": cwd,
        "environment": environment,
    }))
    .map_err(|_| "Could not prepare the background command environment".to_owned())?;
    let key = format!(
        "{BACKGROUND_ENV_PREFIX}{}_SETUP",
        uuid::Uuid::new_v4().simple()
    );
    command.env(POWERSHELL_SETUP_KEY, &key).env(&key, payload);
    let encoded = encode_powershell_command(POWERSHELL_PROFILE_SETUP);
    command.args(["-NoLogo", "-NonInteractive", "-EncodedCommand", &encoded]);
    Ok(())
}

fn encode_powershell_command(script: &str) -> String {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD.encode(
        script
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod tests;
