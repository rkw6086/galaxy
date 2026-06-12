use anyhow::Result;
use candle_core::Tensor;

#[derive(Clone)]
pub struct GenerationConfig {
    pub max_tokens: usize,
    pub temperature: f64,
    pub top_p: f64,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            max_tokens: 32,
            temperature: 0.7,
            top_p: 0.9,
        }
    }
}

impl GenerationConfig {
    pub fn deterministic() -> Self {
        Self {
            max_tokens: 32,
            temperature: 0.0,
            top_p: 1.0,
        }
    }

    pub fn creative() -> Self {
        Self {
            max_tokens: 64,
            temperature: 0.9,
            top_p: 0.95,
        }
    }
}

pub fn sample(logits: &Tensor, config: &GenerationConfig) -> Result<u32> {
    if config.temperature == 0.0 {
        return greedy(logits);
    }

    let logits = logits.to_dtype(candle_core::DType::F32)?;
    let logits_vec = logits.to_vec1::<f32>()?;

    // Apply temperature
    let scaled: Vec<f64> = logits_vec
        .iter()
        .map(|&x| (x as f64) / config.temperature)
        .collect();

    // Softmax
    let max_val = scaled.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exp: Vec<f64> = scaled.iter().map(|&x| (x - max_val).exp()).collect();
    let sum: f64 = exp.iter().sum();
    let probs: Vec<f64> = exp.iter().map(|&x| x / sum).collect();

    // Top-p (nucleus) sampling
    let mut indexed_probs: Vec<(usize, f64)> = probs.iter().copied().enumerate().collect();
    indexed_probs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut cumulative = 0.0;
    let mut candidates = Vec::new();
    for (idx, prob) in &indexed_probs {
        cumulative += prob;
        candidates.push((*idx, *prob));
        if cumulative >= config.top_p {
            break;
        }
    }

    // Renormalize candidates
    let candidate_sum: f64 = candidates.iter().map(|(_, p)| p).sum();
    let threshold = simple_rng() * candidate_sum;

    let mut acc = 0.0;
    for (idx, prob) in &candidates {
        acc += prob;
        if acc >= threshold {
            return Ok(*idx as u32);
        }
    }

    // Fallback to top candidate
    Ok(candidates[0].0 as u32)
}

fn greedy(logits: &Tensor) -> Result<u32> {
    let logits = logits.to_dtype(candle_core::DType::F32)?;
    let logits_vec = logits.to_vec1::<f32>()?;
    let max_idx = logits_vec
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    Ok(max_idx as u32)
}

fn simple_rng() -> f64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;

    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let hash = hasher.finish();
    (hash as f64) / (u64::MAX as f64)
}
