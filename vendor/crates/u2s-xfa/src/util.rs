//! Mojibake repair for text that was exported double-encoded.
//!
//! Ported verbatim from upstream `core/src/util.rs` — only these three
//! functions of that file are reachable from the render closure.

use unicode_normalization::UnicodeNormalization;

/// Normalise text to NFC (Normalization Form C).
///
/// XFA templates sometimes encode accented characters as a base letter
/// followed by a combining mark (e.g. `a` + U+0300 COMBINING GRAVE ACCENT for
/// "à") rather than the precomposed codepoint. Embedded fonts commonly carry
/// the precomposed glyph but not the bare combining mark, which renders as a
/// missing-glyph box. Composing once here, at the parser edge, means layout,
/// rendering and text extraction all see the same NFC text regardless of how
/// the source XML happened to encode it.
pub fn normalize_nfc(s: &str) -> String {
    s.nfc().collect()
}

fn looks_double_encoded(s: &str) -> bool {
    // Reason in reconstructed-byte space so Windows-1252 continuation bytes
    // (e.g. 0x82 → U+201A, well above U+00FF) are still recognised.
    let mut prev: Option<u8> = None;
    for c in s.chars() {
        let Some(b) = cp1252_byte(c) else {
            prev = None;
            continue;
        };
        if let Some(p) = prev {
            let lead = matches!(p, 0xC2..=0xF4);
            let cont = matches!(b, 0x80..=0xBF);
            if lead && cont {
                return true;
            }
        }
        prev = Some(b);
    }
    false
}

fn cp1252_byte(c: char) -> Option<u8> {
    let u = c as u32;
    if u <= 0xFF {
        // Latin-1 range maps 1:1 (the 0x80..=0x9F C1 controls included — they
        // occur when the original mis-decode used ISO-8859-1 rather than 1252).
        return Some(u as u8);
    }
    Some(match u {
        0x20AC => 0x80,
        0x201A => 0x82,
        0x0192 => 0x83,
        0x201E => 0x84,
        0x2026 => 0x85,
        0x2020 => 0x86,
        0x2021 => 0x87,
        0x02C6 => 0x88,
        0x2030 => 0x89,
        0x0160 => 0x8A,
        0x2039 => 0x8B,
        0x0152 => 0x8C,
        0x017D => 0x8E,
        0x2018 => 0x91,
        0x2019 => 0x92,
        0x201C => 0x93,
        0x201D => 0x94,
        0x2022 => 0x95,
        0x2013 => 0x96,
        0x2014 => 0x97,
        0x02DC => 0x98,
        0x2122 => 0x99,
        0x0161 => 0x9A,
        0x203A => 0x9B,
        0x0153 => 0x9C,
        0x017E => 0x9E,
        0x0178 => 0x9F,
        _ => return None,
    })
}

pub fn fix_double_encoded_utf8(s: &str) -> String {
    if !looks_double_encoded(s) {
        return s.to_string();
    }
    // Reconstruct the original bytes: each char came from one source byte under
    // a Latin-1/Windows-1252 mis-decode. Latin-1 maps bytes 1:1 to U+0000..=U+00FF;
    // Windows-1252 remaps 0x80..=0x9F to assorted higher code points, so reverse
    // those too. Any char that fits neither means this isn't a clean mis-read —
    // bail and leave the text untouched.
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        match cp1252_byte(c) {
            Some(b) => bytes.push(b),
            None => return s.to_string(),
        }
    }
    match std::str::from_utf8(&bytes) {
        // Reinterpreting as UTF-8 succeeded → this is the repaired text.
        Ok(fixed) => fixed.to_string(),
        // Not actually valid double-encoded UTF-8; leave it alone.
        Err(_) => s.to_string(),
    }
}
