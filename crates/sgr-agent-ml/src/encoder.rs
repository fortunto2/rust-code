//! ONNX bi-encoder — load model + tokenizer, encode text → f32 embedding.

use std::path::Path;

use anyhow::{Context, Result};
use ndarray::Array1;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

/// ONNX bi-encoder for text embeddings (e.g. MiniLM-L6-v2, bge-m3).
///
/// Usage:
/// ```ignore
/// let mut enc = OnnxEncoder::load(Path::new("models"))?;
/// let emb = enc.encode("hello world")?;
/// ```
pub struct OnnxEncoder {
    session: Session,
    tokenizer: Tokenizer,
}

impl OnnxEncoder {
    /// Load ONNX model + HF tokenizer from directory.
    ///
    /// Expects `model.onnx` and `tokenizer.json` in `models_dir`.
    pub fn load(models_dir: &Path) -> Result<Self> {
        Self::load_files(
            &models_dir.join("model.onnx"),
            &models_dir.join("tokenizer.json"),
        )
    }

    /// Load from explicit file paths (e.g. nli_model.onnx, nli_tokenizer.json).
    pub fn load_files(model_path: &Path, tokenizer_path: &Path) -> Result<Self> {
        let session = Session::builder()
            .context("failed to create ONNX session builder")?
            .commit_from_file(&model_path)
            .with_context(|| format!("failed to load ONNX model from {}", model_path.display()))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load tokenizer: {}", e))?;

        Ok(Self { session, tokenizer })
    }

    /// Check if model files exist in the given directory.
    pub fn is_available(models_dir: &Path) -> bool {
        models_dir.join("model.onnx").exists() && models_dir.join("tokenizer.json").exists()
    }

    /// Load if available, None with warning otherwise.
    pub fn try_load(models_dir: &Path) -> Option<Self> {
        if Self::is_available(models_dir) {
            match Self::load(models_dir) {
                Ok(enc) => Some(enc),
                Err(e) => {
                    tracing::warn!("Failed to load ONNX encoder: {:#}", e);
                    None
                }
            }
        } else {
            tracing::info!("ONNX model not found at {}", models_dir.display());
            None
        }
    }

    /// Access the tokenizer (e.g. for word-level analysis).
    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    /// Encode text → L2-normalized embedding vector.
    ///
    /// Uses mean pooling over sequence dimension, then L2 normalization.
    pub fn encode(&mut self, text: &str) -> Result<Array1<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("tokenization failed: {}", e))?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();
        let len = ids.len();

        let input_ids = Tensor::from_array(([1i64, len as i64], ids.into_boxed_slice()))?;
        let attention_mask = Tensor::from_array(([1i64, len as i64], mask.into_boxed_slice()))?;
        let token_type_ids = Tensor::from_array(([1i64, len as i64], type_ids.into_boxed_slice()))?;

        let outputs = self.session.run(ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
            "token_type_ids" => token_type_ids,
        ])?;

        // Output shape: [1, seq_len, hidden_dim] — mean pool over seq_len
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        let hidden_dim = *shape.last().context("empty output shape")? as usize;
        let seq_len = if shape.len() >= 2 {
            shape[shape.len() - 2] as usize
        } else {
            1
        };

        let mut embedding = vec![0.0f32; hidden_dim];
        for s in 0..seq_len {
            for d in 0..hidden_dim {
                embedding[d] += data[s * hidden_dim + d];
            }
        }
        for d in 0..hidden_dim {
            embedding[d] /= seq_len as f32;
        }

        // L2 normalize
        let mut result = Array1::from_vec(embedding);
        crate::l2_normalize(&mut result);
        Ok(result)
    }

    /// Encode sentence pair (premise, hypothesis) for cross-encoder / NLI models.
    /// Returns raw logits (caller applies softmax).
    pub fn encode_pair(&mut self, text_a: &str, text_b: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode((text_a, text_b), true)
            .map_err(|e| anyhow::anyhow!("pair tokenization failed: {}", e))?;

        let ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();
        let len = ids.len();

        let input_ids = Tensor::from_array(([1i64, len as i64], ids.into_boxed_slice()))?;
        let attention_mask = Tensor::from_array(([1i64, len as i64], mask.into_boxed_slice()))?;
        let token_type_ids = Tensor::from_array(([1i64, len as i64], type_ids.into_boxed_slice()))?;

        let outputs = self.session.run(ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
            "token_type_ids" => token_type_ids,
        ])?;

        let (_shape, data) = outputs[0].try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    }
}
