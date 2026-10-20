//! Bytes a tool call names either by blob handle or by path on the server's
//! machine, never both: how the verifiers take a package and the format
//! servers take the artifact to decode. The handle is what the u2s stack
//! passes; the path is for conformance test vectors.

use crate::BlobStore;

/// The two argument names a tool uses for the handle and the path, so a
/// refusal names the arguments the caller actually sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceArgs {
    pub blob: &'static str,
    pub path: &'static str,
}

impl SourceArgs {
    /// A verifier's package to install.
    pub const PACKAGE: SourceArgs = SourceArgs {
        blob: "package",
        path: "package_path",
    };
    /// A format server's artifact to decode.
    pub const ARTIFACT: SourceArgs = SourceArgs {
        blob: "artifact_blob",
        path: "artifact_path",
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Blob(String),
    Path(String),
}

/// The bytes one call names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobOrPath {
    pub source: Source,
    args: SourceArgs,
}

impl BlobOrPath {
    /// The source a call's handle and path arguments name. Naming both is
    /// refused rather than resolved by precedence, since the two could name
    /// different bytes.
    pub fn from_parts(
        blob: Option<String>,
        path: Option<String>,
        args: SourceArgs,
    ) -> Result<Self, String> {
        let source = match (blob, path) {
            (Some(_), Some(_)) => {
                return Err(format!(
                    "both {} and {} were given; supply exactly one",
                    args.blob, args.path
                ));
            }
            (Some(handle), None) => Source::Blob(handle),
            (None, Some(path)) => Source::Path(path),
            (None, None) => {
                return Err(format!(
                    "missing required argument: exactly one of {} or {}",
                    args.blob, args.path
                ));
            }
        };
        Ok(BlobOrPath { source, args })
    }

    /// The bytes, from the store or the file.
    pub fn read(&self, store: &BlobStore) -> Result<Vec<u8>, String> {
        match &self.source {
            Source::Blob(handle) => store
                .get(handle)
                .map_err(|err| format!("could not read {} {handle}: {err}", self.args.blob)),
            Source::Path(path) => std::fs::read(path)
                .map_err(|err| format!("could not read {} {path}: {err}", self.args.path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_names_its_bytes_by_handle_or_path_never_both() {
        let some = |s: &str| Some(s.to_owned());
        let args = SourceArgs::ARTIFACT;
        assert_eq!(
            BlobOrPath::from_parts(some("h.zip"), None, args)
                .unwrap()
                .source,
            Source::Blob("h.zip".to_owned())
        );
        assert_eq!(
            BlobOrPath::from_parts(None, some("/p.zip"), args)
                .unwrap()
                .source,
            Source::Path("/p.zip".to_owned())
        );
        let both = BlobOrPath::from_parts(some("h.zip"), some("/p.zip"), args).unwrap_err();
        assert!(
            both.contains("artifact_blob") && both.contains("exactly one"),
            "{both}"
        );
        let neither = BlobOrPath::from_parts(None, None, SourceArgs::PACKAGE).unwrap_err();
        assert!(
            neither.contains("package_path") && neither.contains("exactly one"),
            "{neither}"
        );
    }

    #[test]
    fn a_missing_file_is_named_by_its_argument() {
        let source =
            BlobOrPath::from_parts(None, Some("/nope/p.zip".to_owned()), SourceArgs::PACKAGE)
                .unwrap();
        let dir = std::env::temp_dir().join(format!("u2s-blob-source-{}", std::process::id()));
        let store = BlobStore::in_folder(&dir);
        let err = source.read(&store).unwrap_err();
        assert!(
            err.starts_with("could not read package_path /nope/p.zip"),
            "{err}"
        );
    }
}
