//! The content hash that keys references and edit-history sessions alike, so
//! the two cannot drift: `agent::db::document_hash` calls this one.

use sha2::{Digest, Sha256};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Order-independent hash of a set of files: each file is hashed, the digests
/// are sorted, and the result is hashed. File names are ignored.
pub fn document_hash(files: &[(String, Vec<u8>)]) -> String {
    let mut digests: Vec<String> = files.iter().map(|(_, bytes)| sha256_hex(bytes)).collect();
    digests.sort();
    sha256_hex(digests.join("").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_hash_is_order_independent() {
        let a = ("a.xml".to_string(), b"alpha".to_vec());
        let b = ("b.xml".to_string(), b"beta".to_vec());
        assert_eq!(document_hash(&[a.clone(), b.clone()]), document_hash(&[b, a]));
    }

    #[test]
    fn document_hash_differs_on_content() {
        let one = document_hash(&[("f".into(), b"one".to_vec())]);
        let two = document_hash(&[("f".into(), b"two".to_vec())]);
        assert_ne!(one, two);
    }
}
