use std::path::PathBuf;

use anyhow::{Context, Result};

use super::{CONFIG_FILENAME, HF_REPO, MODEL_FILENAME, TOKENIZER_FILENAME};

pub fn model_cache_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "galaxy")
        .map(|dirs| dirs.cache_dir().join("models").join("smollm2-135m"))
        .unwrap_or_else(|| PathBuf::from(".galaxy/models/smollm2-135m"))
}

pub async fn ensure_model_available() -> Result<PathBuf> {
    let cache_dir = model_cache_dir();

    let model_path = cache_dir.join(MODEL_FILENAME);
    let tokenizer_path = cache_dir.join(TOKENIZER_FILENAME);
    let config_path = cache_dir.join(CONFIG_FILENAME);

    if model_path.exists() && tokenizer_path.exists() && config_path.exists() {
        log::debug!("Model already cached at {}", cache_dir.display());
        return Ok(cache_dir);
    }

    log::info!("Downloading SmolLM2-135M model from HuggingFace...");
    tokio::fs::create_dir_all(&cache_dir)
        .await
        .with_context(|| format!("creating cache dir {}", cache_dir.display()))?;

    let api = hf_hub::api::tokio::Api::new().context("initializing HuggingFace API")?;
    let repo = api.model(HF_REPO.to_string());

    let downloaded_model = repo
        .get(MODEL_FILENAME)
        .await
        .context("downloading model weights")?;
    let downloaded_tokenizer = repo
        .get(TOKENIZER_FILENAME)
        .await
        .context("downloading tokenizer")?;
    let downloaded_config = repo
        .get(CONFIG_FILENAME)
        .await
        .context("downloading config")?;

    // hf-hub caches files itself, but we symlink/copy to our canonical location
    // for predictable access
    link_or_copy(&downloaded_model, &model_path).await?;
    link_or_copy(&downloaded_tokenizer, &tokenizer_path).await?;
    link_or_copy(&downloaded_config, &config_path).await?;

    log::info!("Model downloaded and cached at {}", cache_dir.display());
    Ok(cache_dir)
}

async fn link_or_copy(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    if dst.exists() {
        return Ok(());
    }

    // Try symlink first (saves disk space)
    #[cfg(unix)]
    {
        if tokio::fs::symlink(src, dst).await.is_ok() {
            return Ok(());
        }
    }

    // Fall back to copy
    tokio::fs::copy(src, dst)
        .await
        .with_context(|| format!("copying {} to {}", src.display(), dst.display()))?;
    Ok(())
}
