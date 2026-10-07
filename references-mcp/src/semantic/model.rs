use super::SemanticError;
use super::tokenizer_wrapper::Encoding;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, HiddenAct};

/// Embedded model weights (FP16 safetensors, ~224 MB).
const MODEL_BYTES: &[u8] = include_bytes!("../../models/model.safetensors");

/// Embedding dimension for paraphrase-multilingual-MiniLM-L12-v2.
const EMBEDDING_DIM: usize = 384;

/// Maximum batch size for inference to bound memory usage.
const MAX_BATCH_SIZE: usize = 32;

/// BERT-based sentence embedder using candle.
pub(super) struct BertEmbedder {
    model: BertModel,
    device: Device,
}

impl BertEmbedder {
    /// Load model from embedded safetensors bytes.
    pub fn new() -> Result<Self, SemanticError> {
        let device = Device::Cpu;

        let config = Config {
            vocab_size: 250037,
            hidden_size: 384,
            num_hidden_layers: 12,
            num_attention_heads: 12,
            intermediate_size: 1536,
            hidden_act: HiddenAct::Gelu,
            max_position_embeddings: 512,
            type_vocab_size: 2,
            layer_norm_eps: 1e-12,
            pad_token_id: 0,
            ..Default::default()
        };

        let vb = VarBuilder::from_buffered_safetensors(MODEL_BYTES.to_vec(), DType::F32, &device)
            .map_err(|e| SemanticError::Model(format!("failed to load model weights: {e}")))?;

        let model = BertModel::load(vb, &config)
            .map_err(|e| SemanticError::Model(format!("failed to build BERT model: {e}")))?;

        Ok(Self { model, device })
    }

    /// Compute L2-normalised embeddings for a batch of tokenised inputs.
    ///
    /// Each `Encoding` provides `token_ids` and `attention_mask`. Inputs are
    /// padded to the longest sequence in the batch.
    pub fn embed_batch(&self, encodings: &[Encoding]) -> Result<Vec<Vec<f32>>, SemanticError> {
        if encodings.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(encodings.len());

        // Process in chunks to limit peak memory.
        for chunk in encodings.chunks(MAX_BATCH_SIZE) {
            let batch_embs = self.embed_chunk(chunk)?;
            all_embeddings.extend(batch_embs);
        }

        Ok(all_embeddings)
    }

    fn embed_chunk(&self, encodings: &[Encoding]) -> Result<Vec<Vec<f32>>, SemanticError> {
        let batch_size = encodings.len();
        let max_len = encodings
            .iter()
            .map(|e| e.token_ids.len())
            .max()
            .unwrap_or(0);

        if max_len == 0 {
            return Ok(vec![vec![0.0; EMBEDDING_DIM]; batch_size]);
        }

        // Build padded tensors.
        let mut token_ids_flat = vec![0u32; batch_size * max_len]; // pad_token_id = 0
        let mut attention_mask_flat = vec![0u32; batch_size * max_len];

        for (i, enc) in encodings.iter().enumerate() {
            let offset = i * max_len;
            for (j, &tid) in enc.token_ids.iter().enumerate() {
                token_ids_flat[offset + j] = tid;
            }
            for (j, &am) in enc.attention_mask.iter().enumerate() {
                attention_mask_flat[offset + j] = am;
            }
        }

        let token_ids = Tensor::from_vec(token_ids_flat, (batch_size, max_len), &self.device)?;
        let attention_mask =
            Tensor::from_vec(attention_mask_flat, (batch_size, max_len), &self.device)?;
        let token_type_ids = token_ids.zeros_like()?;

        // Forward pass → [batch_size, seq_len, hidden_size]
        let hidden_states =
            self.model
                .forward(&token_ids, &token_type_ids, Some(&attention_mask))?;

        // Mean pooling: average hidden states over non-padding positions.
        let mask_f32 = attention_mask.to_dtype(DType::F32)?;
        let mask_expanded = mask_f32.unsqueeze(2)?.broadcast_as(hidden_states.shape())?;
        let sum = (hidden_states * mask_expanded)?.sum(1)?;
        let count = mask_f32.sum(1)?.unsqueeze(1)?.broadcast_as(sum.shape())?;
        let mean_pooled = (sum / count)?;

        // L2 normalise.
        let norms = mean_pooled
            .sqr()?
            .sum_keepdim(1)?
            .sqrt()?
            .broadcast_as(mean_pooled.shape())?;
        let normalised = (mean_pooled / norms)?;

        // Convert to Vec<Vec<f32>>.
        let data = normalised.to_vec2::<f32>()?;
        Ok(data)
    }
}
