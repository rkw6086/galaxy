use std::path::Path;
#[cfg(feature = "local_fs")]
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

#[cfg(feature = "local_fs")]
use crate::install::{
    fetch_latest_metadata_from_github_dynamic_asset, install_from_github, AssetKind,
};
use crate::language_server_candidate::{LanguageServerCandidate, LanguageServerMetadata};
use crate::CommandBuilder;

#[cfg(feature = "local_fs")]
const SERVER_NAME: &str = "tombi";

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub struct TombiCandidate {
    client: Arc<http_client::Client>,
}

impl TombiCandidate {
    pub fn new(client: Arc<http_client::Client>) -> Self {
        Self { client }
    }

    #[cfg(feature = "local_fs")]
    pub async fn find_installed_binary_in_data_dir() -> Option<PathBuf> {
        let install_root = galaxy_core::paths::data_dir().join(SERVER_NAME);
        let entries = std::fs::read_dir(install_root).ok()?;

        for entry in entries.flatten() {
            let version_dir = entry.path();
            if !version_dir.is_dir() {
                continue;
            }

            let Some(binary_path) = find_binary(&version_dir) else {
                continue;
            };
            if binary_is_working(&binary_path).await {
                return Some(binary_path);
            }
        }

        None
    }
}

#[cfg(feature = "local_fs")]
fn asset_target() -> anyhow::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => anyhow::bail!("Unsupported platform for Tombi: {os}/{arch}"),
    }
}

#[cfg(feature = "local_fs")]
fn asset_name(tag: &str) -> anyhow::Result<String> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    Ok(format!(
        "tombi-cli-{version}-{}.{extension}",
        asset_target()?
    ))
}

#[cfg(feature = "local_fs")]
fn find_binary(install_dir: &Path) -> Option<PathBuf> {
    let binary_name = if cfg!(windows) {
        "tombi.exe"
    } else {
        SERVER_NAME
    };
    let direct_path = install_dir.join(binary_name);
    if direct_path.is_file() {
        return Some(direct_path);
    }

    std::fs::read_dir(install_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            path.is_dir().then(|| path.join(binary_name))
        })
        .find(|path| path.is_file())
}

#[cfg(feature = "local_fs")]
async fn binary_is_working(binary_path: &Path) -> bool {
    if !binary_path.is_file() {
        return false;
    }

    let mut command = command::r#async::Command::new(binary_path);
    command.arg("--version");
    command
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[async_trait]
#[cfg(feature = "local_fs")]
impl LanguageServerCandidate for TombiCandidate {
    async fn should_suggest_for_repo(&self, path: &Path, _executor: &CommandBuilder) -> bool {
        let Ok(entries) = std::fs::read_dir(path) else {
            return false;
        };

        entries.flatten().any(|entry| {
            let file_path = entry.path();
            file_path.is_file()
                && file_path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    == Some("toml")
        })
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        Self::find_installed_binary_in_data_dir().await.is_some()
    }

    async fn is_installed_on_path(&self, executor: &CommandBuilder) -> bool {
        executor
            .command(SERVER_NAME)
            .arg("--version")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    async fn install(
        &self,
        metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        let asset_name = asset_name(&metadata.version)?;
        let asset_kind = AssetKind::from_filename(&asset_name)
            .ok_or_else(|| anyhow::anyhow!("Unsupported archive format for asset: {asset_name}"))?;
        let binary_path = install_from_github(
            &self.client,
            &metadata,
            SERVER_NAME,
            asset_kind,
            Some(find_binary),
        )
        .await?;

        if !binary_is_working(&binary_path).await {
            anyhow::bail!(
                "Installed Tombi binary at {} failed its version check",
                binary_path.display()
            );
        }

        Ok(())
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        let target = asset_target()?;
        let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
        fetch_latest_metadata_from_github_dynamic_asset(
            &self.client,
            "tombi-toml",
            "tombi",
            |tag| {
                let version = tag.strip_prefix('v').unwrap_or(tag);
                format!("tombi-cli-{version}-{target}.{extension}")
            },
        )
        .await
    }
}

#[async_trait]
#[cfg(not(feature = "local_fs"))]
impl LanguageServerCandidate for TombiCandidate {
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
        anyhow::bail!("Tombi installation is unavailable without local filesystem support")
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        anyhow::bail!("Tombi metadata is unavailable without local filesystem support")
    }
}
