//! The arguments of the tools every AEM FileVault format server offers the
//! same way, as types: each is both the tool's input schema and what its
//! arguments parse into. Shared by `u2s-aem-mcp` and `u2s-aem-ubs-mcp`,
//! which both decode a FileVault package and search a fragment library.
//!
//! A `///` on a field is the description the caller reads: prompt text.

use schemars::JsonSchema;
use serde::Deserialize;

/// `decode`: the decode contract `u2s_mcp::manifest::ToolRole::Decode`
/// pins, for a FileVault package.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct DecodeArgs {
    /// A u2s blob handle naming the FileVault package to decode.
    #[schemars(length(min = 1))]
    pub artifact_blob: Option<String>,
    /// A filesystem path to the package, for conformance test vectors.
    #[schemars(length(min = 1))]
    pub artifact_path: Option<String>,
    // The two hints the decode contract lets the platform send; a FileVault
    // package is always a ZIP, so neither changes what is read.
    /// A hint; ignored -- this decoder only ever reads a ZIP.
    #[schemars(length(min = 1))]
    pub media_type: Option<String>,
    /// A hint; ignored.
    #[schemars(length(min = 1))]
    pub filename: Option<String>,
}

/// `fragment_search`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FragmentSearchArgs {
    /// Keywords to search for in fragment titles and previews.
    pub query: String,
}
