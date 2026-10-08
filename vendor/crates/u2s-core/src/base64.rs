//! Standard base64 decoding, hand-rolled so the workspace's shared crate
//! takes no dependency for it.
//!
//! Lives here rather than in a consumer because there are two: `u2s-agent`
//! decodes an inline image out of an MCP tool result before it can reach a
//! model, and `u2s-server`'s normalizer decodes the same payload to write a
//! rendered page into the blob store. One implementation, one set of tests.

/// Why a base64 payload could not be decoded.
///
/// A typed error rather than a `String`: both callers turn this into
/// something a model or an operator reads, and neither should have to match
/// on prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base64Error {
    /// A byte outside the standard alphabet (padding and ASCII whitespace
    /// are tolerated and ignored).
    NotBase64,
    /// A trailing group of one character, which encodes no whole byte.
    Truncated,
}

impl Base64Error {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotBase64 => "not valid base64",
            Self::Truncated => "truncated base64",
        }
    }
}

impl std::fmt::Display for Base64Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for Base64Error {}

/// Decodes standard base64, ignoring ASCII whitespace and `=` padding.
///
/// Padding is ignored rather than validated because the input is a
/// provider's or a tool server's encoding of bytes we are about to hash and
/// store: rejecting a payload whose padding is merely unconventional would
/// fail a render for a reason that does not affect the bytes.
pub fn decode(input: &str) -> Result<Vec<u8>, Base64Error> {
    const INVALID: u8 = 0xFF;
    let mut table = [INVALID; 256];
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (idx, byte) in alphabet.iter().enumerate() {
        table[*byte as usize] = idx as u8;
    }

    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
        .collect();
    if cleaned.iter().any(|byte| table[*byte as usize] == INVALID) {
        return Err(Base64Error::NotBase64);
    }

    let mut out = Vec::with_capacity(cleaned.len() * 3 / 4);
    for chunk in cleaned.chunks(4) {
        if chunk.len() == 1 {
            return Err(Base64Error::Truncated);
        }
        let mut buf = [0u32; 4];
        for (idx, byte) in chunk.iter().enumerate() {
            buf[idx] = u32::from(table[*byte as usize]);
        }
        let packed = (buf[0] << 18) | (buf[1] << 12) | (buf[2] << 6) | buf[3];
        out.push((packed >> 16) as u8);
        if chunk.len() >= 3 {
            out.push((packed >> 8) as u8);
        }
        if chunk.len() == 4 {
            out.push(packed as u8);
        }
    }
    Ok(out)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes standard base64 with `=` padding.
///
/// The counterpart to [`decode`], here for the same reason: the Input
/// Review Agent's opening prompt carries page renders as base64, and the
/// bytes come from the blob store as bytes.
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let packed = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((packed >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((packed >> 12) & 0x3F) as usize] as char);
        // Padding rather than omission: `decode` here tolerates either, but
        // a provider's decoder is not ours to assume about.
        if chunk.len() >= 2 {
            out.push(ALPHABET[((packed >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() == 3 {
            out.push(ALPHABET[(packed & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_every_padding_length() {
        assert_eq!(decode("QUJD").expect("ok"), b"ABC");
        assert_eq!(decode("QUI=").expect("ok"), b"AB");
        assert_eq!(decode("QQ==").expect("ok"), b"A");
        assert_eq!(decode("").expect("ok"), Vec::<u8>::new());
    }

    #[test]
    fn whitespace_and_padding_are_ignored() {
        assert_eq!(decode("QUJD\n").expect("ok"), b"ABC");
        assert_eq!(decode("QU JD").expect("ok"), b"ABC");
        assert_eq!(decode("QUI=\r\n").expect("ok"), b"AB");
    }

    #[test]
    fn a_byte_outside_the_alphabet_is_refused() {
        assert_eq!(decode("QU!D"), Err(Base64Error::NotBase64));
        assert_eq!(decode("nope$"), Err(Base64Error::NotBase64));
    }

    #[test]
    fn a_lone_trailing_character_is_refused() {
        assert_eq!(decode("QUJDQ"), Err(Base64Error::Truncated));
    }

    #[test]
    fn encode_matches_known_vectors_and_pads() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"A"), "QQ==");
        assert_eq!(encode(b"AB"), "QUI=");
        assert_eq!(encode(b"ABC"), "QUJD");
        assert_eq!(encode(b"ABCD"), "QUJDRA==");
    }

    /// The property that matters for the page-image path: whatever comes
    /// out of the blob store survives the trip into a prompt.
    #[test]
    fn encode_then_decode_is_the_identity_for_every_length() {
        for len in 0..=64usize {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
            assert_eq!(
                decode(&encode(&bytes)).expect("round trip"),
                bytes,
                "length {len}"
            );
        }
    }

    #[test]
    fn round_trips_all_byte_values() {
        // Every byte 0..=255, so a table or shift error cannot hide in a
        // range the smaller cases never reach.
        let bytes: Vec<u8> = (0..=255u8).collect();
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut encoded = String::new();
        for chunk in bytes.chunks(3) {
            let b0 = u32::from(chunk[0]);
            let b1 = chunk.get(1).copied().map_or(0, u32::from);
            let b2 = chunk.get(2).copied().map_or(0, u32::from);
            let packed = (b0 << 16) | (b1 << 8) | b2;
            encoded.push(alphabet[((packed >> 18) & 0x3F) as usize] as char);
            encoded.push(alphabet[((packed >> 12) & 0x3F) as usize] as char);
            if chunk.len() >= 2 {
                encoded.push(alphabet[((packed >> 6) & 0x3F) as usize] as char);
            }
            if chunk.len() == 3 {
                encoded.push(alphabet[(packed & 0x3F) as usize] as char);
            }
        }
        assert_eq!(decode(&encoded).expect("ok"), bytes);
    }
}
