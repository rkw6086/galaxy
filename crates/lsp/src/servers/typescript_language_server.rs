use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "local_fs")]
use anyhow::Context;
use async_trait::async_trait;
#[cfg(feature = "local_fs")]
use command::r#async::Command;

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
