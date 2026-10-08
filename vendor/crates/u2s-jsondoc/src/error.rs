//! The one error type every operation in this crate reports through.

use jsonptr::ParseError;

#[derive(Debug, thiserror::Error)]
pub enum JsonDocError {
    #[error("{pointer:?} is not a valid JSON Pointer: {source}")]
    InvalidPointer {
        pointer: String,
        #[source]
        source: ParseError,
    },
    #[error("{pointer}: no such element in the document")]
    NotFound { pointer: String },
}

pub(crate) fn parse_pointer(raw: &str) -> Result<&jsonptr::Pointer, JsonDocError> {
    jsonptr::Pointer::parse(raw).map_err(|source| JsonDocError::InvalidPointer {
        pointer: raw.to_owned(),
        source,
    })
}
