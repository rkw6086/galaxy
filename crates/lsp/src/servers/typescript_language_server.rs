use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "local_fs")]
use anyhow::Context;
use async_trait::async_trait;
#[cfg(feature = "local_fs")]
use serde_json::Value;

use crate::language_server_candidate::{LanguageServerCandidate, LanguageServerMetadata};
#[cfg(feature = "local_fs")]
use crate::supported_servers::CustomBinaryConfig;
use crate::CommandBuilder;

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub struct TypeScriptLanguageServerCandidate {
    client: Arc<http_client::Client>,
}

impl TypeScriptLanguageServerCandidate {
    pub fn new(client: Arc<http_client::Client>) -> Self {
        Self { client }
    }

    /// Resolves `initializationOptions` for `typescript-language-server`.
    ///
    /// The server requires a valid TypeScript installation to function. It searches for
    /// `tsserver` in this order:
    /// 1. The workspace's `node_modules/typescript/lib` (local install)
    /// 2. A globally-installed TypeScript package (via `npm root -g`)
    ///
    /// Returns `initializationOptions.tsserver.path` pointing to the TypeScript lib directory
    /// so the language server can locate `tsserver.js`.
    #[cfg(feature = "local_fs")]
    pub async fn resolve_initialization_options(
        workspace_root: &Path,
        path_env_var: Option<&str>,
    ) -> Option<Value> {
        // 1. Check workspace-local node_modules
        let local_ts_lib = workspace_root.join("node_modules/typescript/lib");
        if local_ts_lib.join("tsserver.js").is_file() {
            log::info!(
                "typescript-language-server: using workspace typescript at {}",
                local_ts_lib.display()
            );
            return Some(serde_json::json!({
                "tsserver": {
                    "path": local_ts_lib.to_string_lossy()
                }
            }));
        }

        // 2. Check global npm install location
        if let Some(path_env) = path_env_var {
            let mut cmd = command::r#async::Command::new("npm");
            cmd.env("PATH", path_env);
            cmd.args(["root", "-g"]);
            if let Ok(output) = cmd.output().await {
                if output.status.success() {
                    let global_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    let global_ts_lib =
                        std::path::PathBuf::from(&global_root).join("typescript/lib");
                    if global_ts_lib.join("tsserver.js").is_file() {
                        log::info!(
                            "typescript-language-server: using global typescript at {}",
                            global_ts_lib.display()
                        );
                        return Some(serde_json::json!({
                            "tsserver": {
                                "path": global_ts_lib.to_string_lossy()
                            }
                        }));
                    }
                }
            }
        }

        // 3. Try npx-managed typescript location (npx caches packages)
        if let Some(path_env) = path_env_var {
            let mut cmd = command::r#async::Command::new("npx");
            cmd.env("PATH", path_env);
            cmd.args([
                "--yes",
                "--package",
                "typescript",
                "node",
                "-e",
                "console.log(require('typescript').sys.getExecutingFilePath())",
            ]);
            // Set cwd to workspace so npx resolves in the right context
            cmd.current_dir(workspace_root);
            if let Ok(output) = cmd.output().await {
                if output.status.success() {
                    let tsserver_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !tsserver_path.is_empty() {
                        // The executing file path is something like .../typescript/lib/tsserver.js
                        // We need the parent directory (the lib dir)
                        if let Some(lib_dir) = std::path::Path::new(&tsserver_path).parent() {
                            if lib_dir.join("tsserver.js").is_file() {
                                log::info!(
                                    "typescript-language-server: using npx-resolved typescript at {}",
                                    lib_dir.display()
                                );
                                return Some(serde_json::json!({
                                    "tsserver": {
                                        "path": lib_dir.to_string_lossy()
                                    }
                                }));
                            }
                        }
                    }
                }
            }
        }

        log::warn!(
            "typescript-language-server: could not find a TypeScript installation for {}",
            workspace_root.display()
        );
        None
    }

    /// Returns a CustomBinaryConfig that runs the server via npx.
    /// npx handles downloading/caching the package automatically.
    #[cfg(feature = "local_fs")]
    pub async fn find_installed_binary_config(
        path_env_var: Option<&str>,
    ) -> Option<CustomBinaryConfig> {
        // npx is available wherever npm/node is — verify it exists
        let path_env = path_env_var?;
        let mut cmd = command::r#async::Command::new("npx");
        cmd.env("PATH", path_env);
        cmd.arg("--version");
        let output = cmd.output().await.ok()?;
        if !output.status.success() {
            return None;
        }

        // Use npx to run typescript-language-server
        // --yes skips the install prompt, npx handles download/caching
        Some(CustomBinaryConfig {
            binary_path: "npx".into(),
            prepend_args: vec!["--yes".into(), "typescript-language-server".into()],
        })
    }
}

#[async_trait]
#[cfg(feature = "local_fs")]
impl LanguageServerCandidate for TypeScriptLanguageServerCandidate {
    async fn should_suggest_for_repo(&self, path: &Path, _executor: &CommandBuilder) -> bool {
        path.join("package.json").exists()
            || path.join("tsconfig.json").exists()
            || path.join("jsconfig.json").exists()
    }

    async fn is_installed_in_data_dir(&self, executor: &CommandBuilder) -> bool {
        // npx is our "installed" state — if npx is available, we can run the server
        Self::find_installed_binary_config(executor.path_env_var())
            .await
            .is_some()
    }

    async fn is_installed_on_path(&self, executor: &CommandBuilder) -> bool {
        executor
            .command("typescript-language-server")
            .arg("--version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        // With npx, there's nothing to install — npx downloads on first use.
        // Just verify that npx is available.
        let output = executor
            .command("npx")
            .arg("--version")
            .output()
            .await
            .context("npx not found — is Node.js installed?")?;

        if !output.status.success() {
            anyhow::bail!("npx is not working. Ensure Node.js and npm are installed.");
        }

        // Pre-warm: ensure the typescript package is available so the LSP server
        // doesn't fail at initialization time with "Could not find a valid TypeScript
        // installation". This uses `npm list` to check workspace-local or global installs,
        // and falls back to installing typescript globally if not found.
        let has_typescript = {
            let output = executor
                .command("npm")
                .args(["list", "typescript", "--depth=0"])
                .output()
                .await;
            output.map(|o| o.status.success()).unwrap_or(false)
        };

        if !has_typescript {
            log::info!("typescript not found locally, installing globally for LSP support");
            let output = executor
                .command("npm")
                .args(["install", "-g", "typescript"])
                .output()
                .await
                .context("Failed to install typescript globally")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!("Failed to install typescript globally: {stderr}");
                // Non-fatal: the workspace might have it in node_modules already
            }
        }

        log::info!("typescript-language-server will run via npx (no pre-install needed)");
        Ok(())
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        let version =
            node_runtime::fetch_npm_package_version(&self.client, "typescript-language-server")
                .await
                .context("Failed to fetch typescript-language-server version from npm registry")?;

        Ok(LanguageServerMetadata {
            version,
            url: None,
            digest: None,
        })
    }
}

#[async_trait]
#[cfg(not(feature = "local_fs"))]
impl LanguageServerCandidate for TypeScriptLanguageServerCandidate {
    async fn should_suggest_for_repo(&self, _path: &Path, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_on_path(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        todo!()
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        todo!()
    }
}
