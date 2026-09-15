mod generation;
mod model_loader;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result};
use candle_core::{DType, Tensor};
use candle_transformers::models::llama::{Cache, Config, Llama, LlamaConfig};
pub use generation::GenerationConfig;
use tokenizers::Tokenizer;
use tokio::sync::Mutex;

/// A token that can be used to cancel an in-progress generation.
/// Clone it and pass to `generate_cancellable`, then call `cancel()` to
/// interrupt the generation loop between tokens.
#[derive(Clone)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

const HF_REPO: &str = "HuggingFaceTB/SmolLM2-135M-Instruct";
const MODEL_FILENAME: &str = "model.safetensors";
const TOKENIZER_FILENAME: &str = "tokenizer.json";
const CONFIG_FILENAME: &str = "config.json";

#[derive(Debug, Clone, Copy)]
pub enum Device {
    Cpu,
    #[cfg(feature = "metal")]
    Metal,
    #[cfg(feature = "cuda")]
    Cuda(usize),
}

impl Device {
    pub fn best_available() -> Self {
        #[cfg(feature = "metal")]
        {
            Device::Metal
        }
        #[cfg(all(feature = "cuda", not(feature = "metal")))]
        {
            Device::Cuda(0)
        }
        #[cfg(not(any(feature = "metal", feature = "cuda")))]
        {
            Device::Cpu
        }
    }

    fn to_candle_device(self) -> Result<candle_core::Device> {
        match self {
            Device::Cpu => Ok(candle_core::Device::Cpu),
            #[cfg(feature = "metal")]
            Device::Metal => Ok(candle_core::Device::new_metal(0)?),
            #[cfg(feature = "cuda")]
            Device::Cuda(ordinal) => Ok(candle_core::Device::new_cuda(*ordinal)?),
        }
    }
}

pub struct InferenceEngine {
    model: Arc<Mutex<Llama>>,
    cache: Arc<Mutex<Cache>>,
    config: Config,
    tokenizer: Tokenizer,
    device: candle_core::Device,
}

impl InferenceEngine {
    pub async fn new(device: Device) -> Result<Self> {
        let candle_device = device.to_candle_device()?;
        let model_dir = model_loader::ensure_model_available().await?;

        let config_path = model_dir.join(CONFIG_FILENAME);
        let config_str = tokio::fs::read_to_string(&config_path)
            .await
            .with_context(|| format!("reading config from {}", config_path.display()))?;
        let llama_config: LlamaConfig =
            serde_json::from_str(&config_str).context("parsing model config")?;
        let config = llama_config.into_config(false);

        let model_path = model_dir.join(MODEL_FILENAME);
        let vb = unsafe {
            candle_nn::VarBuilder::from_mmaped_safetensors(
                &[model_path],
                DType::F32,
                &candle_device,
            )?
        };

        let model = Llama::load(vb, &config).context("loading SmolLM2 model")?;
        let cache = Cache::new(true, DType::F32, &config, &candle_device)?;

        let tokenizer_path = model_dir.join(TOKENIZER_FILENAME);
        let tokenizer_bytes = tokio::fs::read(&tokenizer_path)
            .await
            .with_context(|| format!("reading tokenizer from {}", tokenizer_path.display()))?;
        let tokenizer =
            Tokenizer::from_bytes(&tokenizer_bytes).map_err(|e| anyhow::anyhow!("{e}"))?;

        log::info!(
            "Local inference engine initialized (device: {:?}, model: {})",
            device,
            HF_REPO
        );

        Ok(Self {
            model: Arc::new(Mutex::new(model)),
            cache: Arc::new(Mutex::new(cache)),
            config,
            tokenizer,
            device: candle_device,
        })
    }

    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    pub fn device(&self) -> &candle_core::Device {
        &self.device
    }

    /// Score fixed, single-token labels in one forward pass. This avoids free-form
    /// generation and parsing failures for small classification tasks.
    pub async fn classify_labels(
        &self,
        prompt: &str,
        labels: [&str; 2],
        cancel: &CancellationToken,
    ) -> Result<[f32; 2]> {
        let mut label_ids = Vec::with_capacity(2);
        for label in labels {
            let encoding = self
                .tokenizer
                .encode(label, false)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            anyhow::ensure!(
                encoding.get_ids().len() == 1,
                "classification label must be one token: {label}"
            );
            label_ids.push(encoding.get_ids()[0] as usize);
        }
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let model = self.model.lock().await;
        let mut cache = self.cache.lock().await;
        anyhow::ensure!(!cancel.is_cancelled(), "classification cancelled");
        *cache = Cache::new(true, DType::F32, &self.config, &self.device)?;
        let input = Tensor::new(encoding.get_ids(), &self.device)?.unsqueeze(0)?;
        let logits = model
            .forward(&input, 0, &mut cache)?
            .squeeze(0)?
            .to_dtype(DType::F32)?
            .to_vec1::<f32>()?;
        anyhow::ensure!(!cancel.is_cancelled(), "classification cancelled");
        let first = logits[label_ids[0]];
        let second = logits[label_ids[1]];
        anyhow::ensure!(
            first.is_finite() && second.is_finite(),
            "non-finite classification scores"
        );
        let maximum = first.max(second);
        let first = (first - maximum).exp();
        let second = (second - maximum).exp();
        Ok([first / (first + second), second / (first + second)])
    }

    pub async fn generate_cancellable(
        &self,
        prompt: &str,
        config: &GenerationConfig,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let input_ids = encoding.get_ids().to_vec();
        let mut tokens = input_ids.clone();

        let eos_token_id = self
            .tokenizer
            .token_to_id("</s>")
            .or_else(|| self.tokenizer.token_to_id("<|endoftext|>"))
            .or_else(|| self.tokenizer.token_to_id("<|im_end|>"));

        let model = self.model.lock().await;
        let mut cache = self.cache.lock().await;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled before generation started");
        }

        // Reset cache for new generation
        *cache = Cache::new(true, DType::F32, &self.config, &self.device)?;

        // Prefill: process all input tokens at once
        let input_tensor = Tensor::new(input_ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let logits = model.forward(&input_tensor, 0, &mut cache)?;

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled during prefill");
        }

        // Sample first generated token from the last logits position
        let first_logits = logits.squeeze(0)?;
        let next_token = generation::sample(&first_logits, config)?;

        if Some(next_token) == eos_token_id {
            let generated_tokens = &tokens[input_ids.len()..];
            let output = self
                .tokenizer
                .decode(generated_tokens, true)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(output.trim().to_string());
        }
        tokens.push(next_token);

        // Decode: generate one token at a time, checking cancellation between tokens
        for _i in 1..config.max_tokens {
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled during generation");
            }

            let pos = tokens.len() - 1;
            let next_input = Tensor::new(&[*tokens.last().unwrap()], &self.device)?.unsqueeze(0)?;
            let logits = model.forward(&next_input, pos, &mut cache)?;

            let next_logits = logits.squeeze(0)?;
            let next_token = generation::sample(&next_logits, config)?;

            if Some(next_token) == eos_token_id {
                break;
            }

            tokens.push(next_token);
        }

        let generated_tokens = &tokens[input_ids.len()..];
        let output = self
            .tokenizer
            .decode(generated_tokens, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        Ok(output.trim().to_string())
    }

    pub async fn generate(&self, prompt: &str, config: &GenerationConfig) -> Result<String> {
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let input_ids = encoding.get_ids().to_vec();
        let mut tokens = input_ids.clone();

        let eos_token_id = self
            .tokenizer
            .token_to_id("</s>")
            .or_else(|| self.tokenizer.token_to_id("<|endoftext|>"))
            .or_else(|| self.tokenizer.token_to_id("<|im_end|>"));

        let model = self.model.lock().await;
        let mut cache = self.cache.lock().await;

        // Reset cache for new generation
        *cache = Cache::new(true, DType::F32, &self.config, &self.device)?;

        // Prefill: process all input tokens at once
        let input_tensor = Tensor::new(input_ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let logits = model.forward(&input_tensor, 0, &mut cache)?;

        // Sample first generated token from the last logits position
        let first_logits = logits.squeeze(0)?;
        let next_token = generation::sample(&first_logits, config)?;

        if Some(next_token) == eos_token_id {
            let generated_tokens = &tokens[input_ids.len()..];
            let output = self
                .tokenizer
                .decode(generated_tokens, true)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            return Ok(output.trim().to_string());
        }
        tokens.push(next_token);

        // Decode: generate one token at a time
        for _i in 1..config.max_tokens {
            let pos = tokens.len() - 1;
            let next_input = Tensor::new(&[*tokens.last().unwrap()], &self.device)?.unsqueeze(0)?;
            let logits = model.forward(&next_input, pos, &mut cache)?;

            let next_logits = logits.squeeze(0)?;
            let next_token = generation::sample(&next_logits, config)?;

            if Some(next_token) == eos_token_id {
                break;
            }

            tokens.push(next_token);
        }

        let generated_tokens = &tokens[input_ids.len()..];
        let output = self
            .tokenizer
            .decode(generated_tokens, true)
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        Ok(output.trim().to_string())
    }

    pub fn model_dir() -> PathBuf {
        model_loader::model_cache_dir()
    }

    pub async fn is_model_downloaded() -> bool {
        let dir = Self::model_dir();
        tokio::fs::metadata(dir.join(MODEL_FILENAME)).await.is_ok()
            && tokio::fs::metadata(dir.join(TOKENIZER_FILENAME))
                .await
                .is_ok()
            && tokio::fs::metadata(dir.join(CONFIG_FILENAME)).await.is_ok()
    }
}
