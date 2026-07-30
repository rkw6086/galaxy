use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::schema::v1::AuthMethodId;
use agent_client_protocol::AcpAgentConfig;

use crate::{DenyByDefaultPermissionHandler, PermissionHandler};

/// Pinned version of the official Codex ACP adapter.
pub const CODEX_ACP_NPM_VERSION: &str = "1.1.7";

/// Pinned version of OpenCode used by the built-in ACP launch preset.
pub const OPENCODE_NPM_VERSION: &str = "1.18.9";

const DEFAULT_CANCELLATION_GRACE_PERIOD: Duration = Duration::from_secs(5);
const DEFAULT_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
#[cfg(any(windows, test))]
const DEFAULT_WINDOWS_EXECUTABLE_EXTENSIONS: &[&str] = &[".COM", ".EXE", ".BAT", ".CMD"];

/// A built-in, version-pinned ACP agent launch preset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcpAgentPreset {
    /// The official adapter around the OpenAI Codex app server.
    Codex,
    /// OpenCode's native `acp` command.
    OpenCode,
}

impl AcpAgentPreset {
    /// Builds the pinned launch configuration for this preset.
    #[must_use]
    pub fn launch_config(self) -> AcpLaunchConfig {
        match self {
            Self::Codex => AcpLaunchConfig::new("npx")
                .args(vec![
                    "--yes".to_owned(),
                    format!("@agentclientprotocol/codex-acp@{CODEX_ACP_NPM_VERSION}"),
                ])
                .preferred_auth_method("chat-gpt")
                // The adapter owns the browser OAuth flow and persists credentials in
                // Codex's normal auth store. Galaxy only selects the advertised ACP
                // method; it never receives or stores ChatGPT tokens.
                .env("DEFAULT_AUTH_REQUEST", r#"{"methodId":"chat-gpt"}"#)
                // Codex ACP otherwise defaults to workspace-write mode, where
                // in-sandbox edits and commands can bypass ACP permission
                // requests. Start read-only so every mutation is mediated by
                // Galaxy's execution profile (or explicit Run to Completion).
                .env("INITIAL_AGENT_MODE", "read-only"),
            Self::OpenCode => AcpLaunchConfig::new("npx").args(vec![
                "--yes".to_owned(),
                format!("opencode-ai@{OPENCODE_NPM_VERSION}"),
                "acp".to_owned(),
            ]),
        }
    }

    /// Resolves the best available executable for this preset.
    ///
    /// OpenCode's native binary is preferred when installed. The Codex adapter
    /// uses `npx` when available and can run through Bun's Node compatibility
    /// mode. OpenCode's npm wrapper requires Node during installation.
    pub fn resolve_launch_config(self) -> Result<AcpLaunchConfig, String> {
        self.resolve_launch_config_with(executable_on_path)
    }

    fn resolve_launch_config_with(
        self,
        mut resolve: impl FnMut(&str) -> Option<PathBuf>,
    ) -> Result<AcpLaunchConfig, String> {
        match self {
            Self::Codex => {
                let (command, args) = if let Some(command) = resolve("npx") {
                    (
                        command,
                        vec![
                            "--yes".to_owned(),
                            format!("@agentclientprotocol/codex-acp@{CODEX_ACP_NPM_VERSION}"),
                        ],
                    )
                } else if let Some(command) = resolve("bunx") {
                    (
                        command,
                        vec![
                            "--bun".to_owned(),
                            format!("@agentclientprotocol/codex-acp@{CODEX_ACP_NPM_VERSION}"),
                        ],
                    )
                } else {
                    return Err(
                        "Codex ACP requires npx or bunx; install Node.js/npm or Bun, or configure a custom ACP executable"
                            .to_owned(),
                    );
                };
                Ok(AcpLaunchConfig::new(command)
                    .args(args)
                    .preferred_auth_method("chat-gpt")
                    .env("DEFAULT_AUTH_REQUEST", r#"{"methodId":"chat-gpt"}"#)
                    .env("INITIAL_AGENT_MODE", "read-only"))
            }
            Self::OpenCode => {
                if let Some(command) = resolve("opencode") {
                    return Ok(AcpLaunchConfig::new(command).args(["acp"]));
                }
                if let Some(command) = resolve("npx") {
                    return Ok(AcpLaunchConfig::new(command).args([
                        "--yes".to_owned(),
                        format!("opencode-ai@{OPENCODE_NPM_VERSION}"),
                        "acp".to_owned(),
                    ]));
                }
                Err(
                    "OpenCode ACP requires the opencode executable or npx; install OpenCode or Node.js/npm, or configure a custom ACP executable"
                        .to_owned(),
                )
            }
        }
    }
}

/// Comparable ACP agent launch settings.
///
/// Galaxy can retain this value as a fingerprint and restart its manager when
/// the selected executable, arguments, environment, or authentication method
/// changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcpLaunchConfig {
    /// Executable to spawn.
    pub command: PathBuf,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Environment variables added to or overridden in the child process.
    ///
    /// Galaxy clears inherited values outside a small runtime allowlist before
    /// applying these explicit overrides.
    pub env: BTreeMap<String, String>,
    /// Authentication method to select when the agent advertises more than one.
    ///
    /// When unset, Galaxy selects the first method advertised by the agent.
    pub preferred_auth_method: Option<AuthMethodId>,
}

impl AcpLaunchConfig {
    /// Creates launch settings for an executable.
    #[must_use]
    pub fn new(command: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            preferred_auth_method: None,
        }
    }

    /// Replaces the arguments passed to the executable.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Adds or overrides an environment variable in the child process.
    #[must_use]
    pub fn env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(name.into(), value.into());
        self
    }

    /// Selects a specific authentication method from the agent's advertised
    /// methods.
    #[must_use]
    pub fn preferred_auth_method(mut self, method_id: impl Into<AuthMethodId>) -> Self {
        self.preferred_auth_method = Some(method_id.into());
        self
    }

    /// Points the Codex adapter at a particular Codex executable.
    #[must_use]
    pub fn codex_path(self, path: impl AsRef<Path>) -> Self {
        self.env("CODEX_PATH", path.as_ref().to_string_lossy())
    }

    /// Resolves a configured executable through `PATH` and returns a clear
    /// startup error before an ACP worker is created.
    pub fn resolve_command(mut self) -> Result<Self, String> {
        let command = if self.command.components().count() > 1 || self.command.is_absolute() {
            self.command
                .is_file()
                .then(|| self.command.clone())
                .ok_or_else(|| {
                    format!(
                        "ACP executable does not exist or is not a file: {}",
                        self.command.display()
                    )
                })?
        } else {
            let command = self.command.to_string_lossy();
            executable_on_path(&command)
                .ok_or_else(|| format!("ACP executable was not found on PATH: {command}"))?
        };
        self.command = command;
        Ok(self)
    }

    pub(crate) fn to_agent_config(&self) -> AcpAgentConfig {
        AcpAgentConfig::new(self.command.clone())
            .args(self.args.clone())
            .envs(sanitized_environment_overrides(
                std::env::vars_os(),
                &self.env,
            ))
    }
}

fn sanitized_environment_overrides(
    parent_environment: impl IntoIterator<Item = (OsString, OsString)>,
    explicit_environment: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut overrides = BTreeMap::new();
    for (name, _) in parent_environment {
        let Some(name) = name.to_str() else {
            continue;
        };
        if !may_inherit_environment_variable(name) {
            overrides.insert(name.to_owned(), String::new());
        }
    }
    overrides.extend(explicit_environment.clone());
    overrides
}

fn may_inherit_environment_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "SHELL"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "LANG"
            | "LANGUAGE"
            | "LC_ALL"
            | "LC_ADDRESS"
            | "LC_COLLATE"
            | "LC_CTYPE"
            | "LC_IDENTIFICATION"
            | "LC_MEASUREMENT"
            | "LC_MESSAGES"
            | "LC_MONETARY"
            | "LC_NAME"
            | "LC_NUMERIC"
            | "LC_PAPER"
            | "LC_TELEPHONE"
            | "LC_TIME"
            | "TZ"
            | "TERM"
            | "COLORTERM"
            | "NO_COLOR"
            | "FORCE_COLOR"
            | "DISPLAY"
            | "WAYLAND_DISPLAY"
            | "XAUTHORITY"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "NODE_EXTRA_CA_CERTS"
            | "NODE_PATH"
            | "NPM_CONFIG_PREFIX"
            | "BUN_INSTALL"
            | "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "PATHEXT"
            | "PROGRAMDATA"
            | "PROGRAMFILES"
            | "PROGRAMFILES(X86)"
            | "COMMONPROGRAMFILES"
            | "COMMONPROGRAMFILES(X86)"
            | "APPDATA"
            | "LOCALAPPDATA"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "NUMBER_OF_PROCESSORS"
            | "PROCESSOR_ARCHITECTURE"
            | "PROCESSOR_IDENTIFIER"
            | "XDG_CACHE_HOME"
            | "XDG_CONFIG_DIRS"
            | "XDG_CONFIG_HOME"
            | "XDG_DATA_DIRS"
            | "XDG_DATA_HOME"
            | "XDG_RUNTIME_DIR"
            | "XDG_STATE_HOME"
            | "__CF_USER_TEXT_ENCODING"
    )
}

fn executable_on_path(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 || path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }

    let path = std::env::var_os("PATH")?;
    let executable_extensions = platform_executable_extensions();
    find_executable_in_directories(
        command,
        std::env::split_paths(&path),
        &executable_extensions,
    )
}

fn find_executable_in_directories(
    command: &str,
    directories: impl IntoIterator<Item = PathBuf>,
    executable_extensions: &[String],
) -> Option<PathBuf> {
    for directory in directories {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }

        if Path::new(command).extension().is_none() {
            for extension in executable_extensions {
                let candidate = directory.join(format!("{command}{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

#[cfg(windows)]
fn platform_executable_extensions() -> Vec<String> {
    windows_executable_extensions(std::env::var_os("PATHEXT").as_deref())
}

#[cfg(any(windows, test))]
fn windows_executable_extensions(path_extensions: Option<&std::ffi::OsStr>) -> Vec<String> {
    let configured = path_extensions.map_or_else(Vec::new, |path_extensions| {
        let path_extensions = path_extensions.to_string_lossy();
        path_extensions
            .split(';')
            .map(str::trim)
            .filter(|extension| !extension.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });

    if configured.is_empty() {
        DEFAULT_WINDOWS_EXECUTABLE_EXTENSIONS
            .iter()
            .map(ToString::to_string)
            .collect()
    } else {
        configured
    }
}

#[cfg(not(windows))]
fn platform_executable_extensions() -> Vec<String> {
    Vec::new()
}

/// Configuration for an [`AcpSessionManager`](crate::AcpSessionManager).
#[derive(Clone)]
pub struct AcpManagerConfig {
    /// Comparable settings used to launch the ACP agent.
    pub launch: AcpLaunchConfig,
    /// Programmatic client name sent during ACP initialization.
    pub client_name: String,
    /// Client version sent during ACP initialization.
    pub client_version: String,
    /// How long a cancelled prompt may remain active before its subprocess is
    /// torn down.
    pub cancellation_grace_period: Duration,
    /// Maximum time allowed for process startup and ACP initialization.
    ///
    /// Timing out drops the connection future, which tears down the ACP child
    /// process and its process group.
    pub initialization_timeout: Duration,
    /// Maximum time allowed for an agent-owned authentication flow.
    ///
    /// Browser login is intentionally given more time than process startup,
    /// but remains bounded so an abandoned flow cannot strand queued turns or
    /// cancellation requests indefinitely.
    pub authentication_timeout: Duration,
    /// Permission hook. The supplied default denies requests unless a turn
    /// explicitly opts into automatic approval.
    pub permission_handler: Arc<dyn PermissionHandler>,
}

impl AcpManagerConfig {
    /// Creates a manager configuration with safe permission defaults.
    #[must_use]
    pub fn new(launch: AcpLaunchConfig) -> Self {
        Self {
            launch,
            client_name: "galaxy".to_owned(),
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            cancellation_grace_period: DEFAULT_CANCELLATION_GRACE_PERIOD,
            initialization_timeout: DEFAULT_INITIALIZATION_TIMEOUT,
            authentication_timeout: DEFAULT_AUTHENTICATION_TIMEOUT,
            permission_handler: Arc::new(DenyByDefaultPermissionHandler),
        }
    }

    /// Creates a configuration from a pinned built-in preset.
    #[must_use]
    pub fn preset(preset: AcpAgentPreset) -> Self {
        Self::new(preset.launch_config())
    }

    /// Replaces the permission decision hook.
    #[must_use]
    pub fn permission_handler(mut self, handler: Arc<dyn PermissionHandler>) -> Self {
        self.permission_handler = handler;
        self
    }

    /// Replaces the process-startup and ACP-initialization timeout.
    #[must_use]
    pub fn initialization_timeout(mut self, timeout: Duration) -> Self {
        self.initialization_timeout = timeout;
        self
    }

    /// Replaces the agent-owned authentication timeout.
    #[must_use]
    pub fn authentication_timeout(mut self, timeout: Duration) -> Self {
        self.authentication_timeout = timeout;
        self
    }
}

impl fmt::Debug for AcpManagerConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpManagerConfig")
            .field("launch", &self.launch)
            .field("client_name", &self.client_name)
            .field("client_version", &self.client_version)
            .field("cancellation_grace_period", &self.cancellation_grace_period)
            .field("initialization_timeout", &self.initialization_timeout)
            .field("authentication_timeout", &self.authentication_timeout)
            .field("permission_handler", &"<permission handler>")
            .finish()
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
