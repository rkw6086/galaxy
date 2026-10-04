//! Explicit Claude Code setup. Downloads happen only when the user tests a
//! connection; normal launches resolve installed components without network I/O.

use std::path::{Component, Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use command::r#async::Command;
use futures::future::{select, Either};
use galaxy_acp::AcpLaunchConfig;
use tokio::sync::Mutex;

// Keep the adapter and the official native CLI installation reproducible.
// npm verifies package integrity and selects Anthropic's platform binary.
pub(super) const CLAUDE_ACP_VERSION: &str = "0.85.1";
const CLAUDE_CODE_VERSION: &str = "2.1.288";
static SETUP_LOCK: Mutex<()> = Mutex::const_new(());

fn installation_dir() -> PathBuf {
    galaxy_core::paths::data_dir()
        .join("acp")
        .join("claude-code")
}

fn installed_command(command: &str) -> Option<PathBuf> {
    AcpLaunchConfig::new(command)
        .resolve_command()
        .ok()
        .map(|launch| launch.command)
}

fn package_binary(package_dir: &Path) -> Option<PathBuf> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package_dir.join("package.json")).ok()?).ok()?;
    let binary = manifest.get("bin")?.get("claude")?.as_str()?;
    let binary = Path::new(binary);
    if binary.is_absolute()
        || binary
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let binary = package_dir.join(binary);
    binary.is_file().then_some(binary)
}

fn claude_binary() -> Option<PathBuf> {
    installed_command("claude")
        .or_else(|| {
            let home = std::env::var_os("HOME")?;
            let path = PathBuf::from(home).join(".local/bin/claude");
            path.is_file().then_some(path)
        })
        .or_else(|| {
            package_binary(&installation_dir().join("node_modules/@anthropic-ai/claude-code"))
        })
}

fn adapter_script() -> PathBuf {
    installation_dir().join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js")
}

fn managed_adapter_ready(directory: &Path) -> bool {
    directory
        .join(format!("ready-{CLAUDE_ACP_VERSION}"))
        .is_file()
        && directory
            .join("node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js")
            .is_file()
}

pub(super) fn resolve_claude_code_launch() -> Result<AcpLaunchConfig, String> {
    let claude = claude_binary().ok_or_else(|| {
        "Claude Code is not installed. Test the Claude Code connection in Settings to download it."
            .to_owned()
    })?;
    let launch = if let Some(adapter) = installed_command("claude-agent-acp") {
        AcpLaunchConfig::new(adapter)
    } else {
        let script = adapter_script();
        if !managed_adapter_ready(&installation_dir()) {
            return Err("The Claude Code ACP adapter is missing. Test the connection in Settings to download it.".to_owned());
        }
        let managed_node = node_runtime::node_binary_path().map_err(|error| error.to_string())?;
        let node = if managed_node.is_file() {
            managed_node
        } else {
            installed_command("node").ok_or_else(|| "Claude Code's ACP adapter needs Node.js. Test the connection in Settings to install it.".to_owned())?
        };
        AcpLaunchConfig::new(node).args([script.to_string_lossy().into_owned()])
    };
    let mut path = launch
        .command
        .parent()
        .into_iter()
        .map(Path::to_owned)
        .collect::<Vec<_>>();
    if let Ok(node) = node_runtime::node_binary_path() {
        if node.is_file() {
            if let Some(directory) = node.parent() {
                path.insert(0, directory.to_owned());
            }
        }
    }
    if let Some(existing) = std::env::var_os("PATH") {
        path.extend(std::env::split_paths(&existing));
    }
    let path = std::env::join_paths(path).map_err(|error| error.to_string())?;
    Ok(launch
        // The adapter must use this unmodified binary, rather than silently
        // selecting another SDK-bundled CLI. Auth stays entirely in Claude Code.
        .env("CLAUDE_CODE_EXECUTABLE", claude.to_string_lossy())
        .env("PATH", path.to_string_lossy()))
}

/// Prepare missing components and authenticate with Claude Code's own browser
/// flow. Never read its token store or include auth command output in logs.
pub(crate) async fn ensure_claude_code_installed(
    progress: async_channel::Sender<&'static str>,
) -> Result<(), String> {
    let _lock = SETUP_LOCK.lock().await;
    let needs_cli = claude_binary().is_none();
    let needs_adapter = installed_command("claude-agent-acp").is_none()
        && !managed_adapter_ready(&installation_dir());
    if needs_cli || needs_adapter {
        let _ = progress.try_send("Downloading missing Claude Code components…");
        log::info!("Claude Code setup: missing_cli={needs_cli}, missing_adapter={needs_adapter}");
        // Reuse Galaxy's private Node runtime; no global install or administrator
        // permission is needed, even when Node/npm are absent from the user's PATH.
        let client = http_client::Client::new();
        match select(
            Box::pin(node_runtime::install_npm(&client)),
            Box::pin(async_io::Timer::after(Duration::from_secs(300))),
        )
        .await
        {
            Either::Left((result, _)) => {
                result.map_err(|error| {
                    format!("Could not install Claude Code's Node runtime: {error}")
                })?;
            }
            Either::Right(_) => {
                return Err(
                    "Downloading Claude Code's Node runtime timed out. Test again to retry."
                        .to_owned(),
                )
            }
        }
        let node = node_runtime::node_binary_path().map_err(|error| error.to_string())?;
        let npm = node_runtime::npm_binary_path().map_err(|error| error.to_string())?;
        let directory = installation_dir();
        async_fs::create_dir_all(&directory)
            .await
            .map_err(|error| error.to_string())?;
        let mut command = Command::new(&node);
        command
            .arg(npm)
            .args([
                "install",
                "--registry=https://registry.npmjs.org",
                "--ignore-scripts=false",
                "--no-audit",
                "--no-fund",
                "--save-exact",
            ])
            .arg("--prefix")
            .arg(&directory)
            .current_dir(&directory);
        if needs_cli {
            command.arg(format!("@anthropic-ai/claude-code@{CLAUDE_CODE_VERSION}"));
        }
        if needs_adapter {
            command.arg(format!(
                "@agentclientprotocol/claude-agent-acp@{CLAUDE_ACP_VERSION}"
            ));
        }
        let mut paths = vec![node
            .parent()
            .ok_or("Node runtime has no directory")?
            .to_owned()];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path));
        }
        command.env(
            "PATH",
            std::env::join_paths(paths).map_err(|error| error.to_string())?,
        );
        let output = run_command(&mut command, Duration::from_secs(300)).await?;
        if !output.status.success() {
            // Installation runs no model request and has no Claude credentials.
            let error = String::from_utf8_lossy(&output.stderr);
            let detail = error.chars().take(2000).collect::<String>();
            return Err(format!(
                "Could not download Claude Code components: {detail}"
            ));
        }
        if needs_adapter {
            if !adapter_script().is_file() {
                return Err("The Claude Code adapter download completed without its executable. Test again to retry.".to_owned());
            }
            // A cancelled/failed npm install may leave the entrypoint behind.
            // Only a completed install is eligible for future launches.
            async_fs::write(
                directory.join(format!("ready-{CLAUDE_ACP_VERSION}")),
                b"ready",
            )
            .await
            .map_err(|error| error.to_string())?;
        }
    }
    let launch = resolve_claude_code_launch()?;
    let claude = launch
        .env
        .get("CLAUDE_CODE_EXECUTABLE")
        .ok_or("Claude Code executable is missing")?;
    let _ = progress.try_send("Checking Claude Code sign-in…");
    let status = run_command(
        claude_auth_command(claude).args(["auth", "status", "--json"]),
        Duration::from_secs(15),
    )
    .await?;
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout)
        .map_err(|_| "Claude Code could not report its login status. Run `claude auth login`, then test again.".to_owned())?;
    if !status.status.success()
        || status_json
            .get("loggedIn")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        let _ = progress.try_send("Opening Claude Code sign-in in your browser…");
        let login = run_command(
            claude_auth_command(claude).args(["auth", "login", "--claudeai"]),
            Duration::from_secs(300),
        )
        .await?;
        if !login.status.success() {
            return Err("Claude Code sign-in did not complete. Complete sign-in through Claude Code and test again.".to_owned());
        }
    }
    let _ = progress.try_send("Discovering Claude Code models…");
    log::info!("Claude Code setup: components ready; starting ACP discovery");
    Ok(())
}

fn claude_auth_command(claude: &str) -> Command {
    let mut command = Command::new(claude);
    // Match the ACP launch's sanitized environment: an inherited API key or
    // provider redirect must not make setup report a subscription as signed in.
    for variable in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        command.env_remove(variable);
    }
    command
}

async fn run_command(command: &mut Command, timeout: Duration) -> Result<Output, String> {
    command.kill_on_drop(true);
    match select(
        Box::pin(command.output()),
        Box::pin(async_io::Timer::after(timeout)),
    )
    .await
    {
        Either::Left((result, _)) => result.map_err(|error| error.to_string()),
        Either::Right(_) => {
            Err("Claude Code setup timed out. Test the connection again to retry.".to_owned())
        }
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
