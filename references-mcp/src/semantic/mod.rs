mod model;
mod tokenizer_wrapper;

use model::BertEmbedder;
use tokenizer_wrapper::TokenizerWrapper;

/// Cosine similarity threshold for semantic matching.
/// Below this, two texts are considered not matching.
pub const SEMANTIC_THRESHOLD: f32 = 0.7;

/// Cross-lingual sentence embedding matcher using a multilingual BERT model.
///
/// Wraps `paraphrase-multilingual-MiniLM-L12-v2` via candle for pure-Rust
/// inference (no native dependencies). Model weights and tokenizer vocabulary
/// are compiled into the binary via `include_bytes!`.
pub struct SemanticMatcher {
    embedder: BertEmbedder,
    tokenizer: TokenizerWrapper,
}

impl SemanticMatcher {
    /// Load the model and tokenizer from embedded bytes.
    ///
    /// This is relatively expensive (~200ms on a modern CPU), so the matcher
    /// should be constructed once and reused for the lifetime of a pipeline run.
    pub fn new() -> Result<Self, SemanticError> {
        let embedder = BertEmbedder::new()?;
        let tokenizer = TokenizerWrapper::new()?;
        Ok(Self {
            embedder,
            tokenizer,
        })
    }

    /// Compute sentence embeddings for a batch of texts.
    ///
    /// Returns one L2-normalised 384-dim vector per input text. Empty texts
    /// produce a zero vector.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, SemanticError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let encodings = self.tokenizer.encode_batch(texts)?;
        self.embedder.embed_batch(&encodings)
    }

    /// Compute a single sentence embedding.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, SemanticError> {
        let mut results = self.embed_batch(&[text])?;
        Ok(results.pop().unwrap_or_default())
    }

    /// Cosine similarity between two L2-normalised embeddings (= dot product).
    pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
        debug_assert_eq!(a.len(), b.len());
        a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
    }

    /// Find the best-matching candidate above the given threshold.
    ///
    /// Returns `(index, similarity)` of the best match, or `None` if no
    /// candidate exceeds the threshold.
    pub fn find_best_match(
        query: &[f32],
        candidates: &[Vec<f32>],
        threshold: f32,
    ) -> Option<(usize, f32)> {
        candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (i, Self::cosine_similarity(query, c)))
            .filter(|(_, sim)| *sim >= threshold)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    }
}

/// Errors from semantic matching operations.
#[derive(Debug)]
pub enum SemanticError {
    Model(String),
    Tokenizer(String),
}

impl std::fmt::Display for SemanticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(msg) => write!(f, "semantic model error: {msg}"),
            Self::Tokenizer(msg) => write!(f, "semantic tokenizer error: {msg}"),
        }
    }
}

impl std::error::Error for SemanticError {}

impl From<candle_core::Error> for SemanticError {
    fn from(e: candle_core::Error) -> Self {
        Self::Model(e.to_string())
    }
}
