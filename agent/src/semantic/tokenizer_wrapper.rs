use super::SemanticError;

/// Embedded tokenizer vocabulary (tokenizer.json from HuggingFace, ~500 KB).
const TOKENIZER_BYTES: &[u8] = include_bytes!("../../models/tokenizer.json");

/// Maximum token length. MiniLM supports up to 128 sequence positions.
const MAX_LENGTH: usize = 128;

/// Tokenised encoding for a single text input.
pub(super) struct Encoding {
    pub token_ids: Vec<u32>,
    pub attention_mask: Vec<u32>,
}

/// Wrapper around the HuggingFace `tokenizers` crate.
pub(super) struct TokenizerWrapper {
    inner: tokenizers::Tokenizer,
}

impl TokenizerWrapper {
    /// Load the tokenizer from embedded JSON bytes.
    pub fn new() -> Result<Self, SemanticError> {
        let inner = tokenizers::Tokenizer::from_bytes(TOKENIZER_BYTES)
            .map_err(|e| SemanticError::Tokenizer(format!("failed to load tokenizer: {e}")))?;
        Ok(Self { inner })
    }

    /// Encode a single text string with truncation.
    #[allow(dead_code)]
    pub fn encode(&self, text: &str) -> Result<Encoding, SemanticError> {
        let encoding = self
            .inner
            .encode(text, true)
            .map_err(|e| SemanticError::Tokenizer(format!("encode failed: {e}")))?;

        let mut token_ids: Vec<u32> = encoding.get_ids().to_vec();
        let mut attention_mask: Vec<u32> = encoding.get_attention_mask().to_vec();

        // Truncate if necessary.
        if token_ids.len() > MAX_LENGTH {
            token_ids.truncate(MAX_LENGTH);
            attention_mask.truncate(MAX_LENGTH);
        }

        Ok(Encoding {
            token_ids,
            attention_mask,
        })
    }

    /// Encode a batch of texts with truncation.
    pub fn encode_batch(&self, texts: &[&str]) -> Result<Vec<Encoding>, SemanticError> {
        let encodings = self
            .inner
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| SemanticError::Tokenizer(format!("batch encode failed: {e}")))?;

        Ok(encodings
            .into_iter()
            .map(|enc| {
                let mut token_ids = enc.get_ids().to_vec();
                let mut attention_mask = enc.get_attention_mask().to_vec();
                if token_ids.len() > MAX_LENGTH {
                    token_ids.truncate(MAX_LENGTH);
                    attention_mask.truncate(MAX_LENGTH);
                }
                Encoding {
                    token_ids,
                    attention_mask,
                }
            })
            .collect())
    }
}
