//! Barcode symbologies, as bar patterns.
//!
//! Only what the corpus actually uses is encoded here. Everything else is
//! reported as unsupported rather than guessed at: a barcode that scans to the
//! wrong string is worse than a visibly missing one.

use thiserror::Error;

#[derive(Debug, Error, PartialEq)]
pub enum BarcodeError {
    #[error("barcode type {0:?} is not supported")]
    UnsupportedSymbology(String),
    #[error("{symbology} encodes digits only, got {value:?}")]
    NotNumeric {
        symbology: &'static str,
        value: String,
    },
    #[error("nothing to encode")]
    Empty,
}

/// One element of a barcode: a bar or a gap, narrow or wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    /// True for a bar, false for the gap between bars.
    pub black: bool,
    /// True for a wide element, false for a narrow one.
    pub wide: bool,
}

/// Interleaved 2 of 5: digits are encoded in pairs, the first of the pair in
/// the bars and the second in the gaps between them. Five elements per digit,
/// two of which are wide -- hence the name.
const I2OF5: [[bool; 5]; 10] = [
    [false, false, true, true, false], // 0
    [true, false, false, false, true], // 1
    [false, true, false, false, true], // 2
    [true, true, false, false, false], // 3
    [false, false, true, false, true], // 4
    [true, false, true, false, false], // 5
    [false, true, true, false, false], // 6
    [false, false, false, true, true], // 7
    [true, false, false, true, false], // 8
    [false, true, false, true, false], // 9
];

/// Encode `digits` as Interleaved 2 of 5.
///
/// The symbology can only encode an even number of digits, so an odd-length
/// value gets a leading zero, which is what every encoder does and what the
/// UBS footer barcodes assume.
pub fn interleaved_2_of_5(digits: &str) -> Result<Vec<Bar>, BarcodeError> {
    let trimmed = digits.trim();
    if trimmed.is_empty() {
        return Err(BarcodeError::Empty);
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Err(BarcodeError::NotNumeric {
            symbology: "code2Of5Interleaved",
            value: trimmed.to_string(),
        });
    }

    let padded = if trimmed.len() % 2 == 1 {
        format!("0{trimmed}")
    } else {
        trimmed.to_string()
    };

    let values: Vec<usize> = padded
        .chars()
        .map(|c| c.to_digit(10).expect("checked above") as usize)
        .collect();

    // Start: narrow bar, narrow gap, narrow bar, narrow gap.
    let mut out = vec![
        Bar {
            black: true,
            wide: false,
        },
        Bar {
            black: false,
            wide: false,
        },
        Bar {
            black: true,
            wide: false,
        },
        Bar {
            black: false,
            wide: false,
        },
    ];

    for pair in values.chunks(2) {
        let (bars, gaps) = (I2OF5[pair[0]], I2OF5[pair[1]]);
        for i in 0..5 {
            out.push(Bar {
                black: true,
                wide: bars[i],
            });
            out.push(Bar {
                black: false,
                wide: gaps[i],
            });
        }
    }

    // Stop: wide bar, narrow gap, narrow bar.
    out.push(Bar {
        black: true,
        wide: true,
    });
    out.push(Bar {
        black: false,
        wide: false,
    });
    out.push(Bar {
        black: true,
        wide: false,
    });

    Ok(out)
}

/// Encode `value` with the symbology an XFA `<barcode type>` names.
pub fn encode(symbology: &str, value: &str) -> Result<Vec<Bar>, BarcodeError> {
    match symbology {
        "code2Of5Interleaved" | "interleaved2of5" => interleaved_2_of_5(value),
        other => Err(BarcodeError::UnsupportedSymbology(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(bars: &[Bar]) -> String {
        bars.iter()
            .map(|b| match (b.black, b.wide) {
                (true, false) => 'n',
                (true, true) => 'W',
                (false, false) => '.',
                (false, true) => '_',
            })
            .collect()
    }

    #[test]
    fn an_even_value_encodes_start_digits_and_stop() {
        let bars = interleaved_2_of_5("12").expect("encode");
        // 4 start + 10 per digit pair + 3 stop
        assert_eq!(bars.len(), 4 + 10 + 3);
        // Start is four narrow elements, alternating bar and gap.
        assert_eq!(pattern(&bars[..4]), "n.n.");
        // Digit 1 (W n n n W) in the bars, interleaved with digit 2
        // (n W n n W) in the gaps between them.
        assert_eq!(pattern(&bars[4..14]), "W.n_n.n.W_");
        // Stop is a wide bar, a narrow gap and a narrow bar.
        assert_eq!(pattern(&bars[14..]), "W.n");
    }

    /// Two wide elements out of every five is the defining property of the
    /// symbology; a transposed table would still produce plausible-looking
    /// bars, so this is checked directly.
    #[test]
    fn every_digit_has_exactly_two_wide_elements() {
        for d in 0..10 {
            assert_eq!(
                I2OF5[d].iter().filter(|w| **w).count(),
                2,
                "digit {d} does not have two wide elements"
            );
        }
    }

    #[test]
    fn an_odd_value_is_padded_to_an_even_length() {
        let odd = interleaved_2_of_5("123").expect("encode");
        let padded = interleaved_2_of_5("0123").expect("encode");
        assert_eq!(odd, padded);
    }

    #[test]
    fn a_non_numeric_value_is_refused_rather_than_mangled() {
        assert!(matches!(
            interleaved_2_of_5("12A4"),
            Err(BarcodeError::NotNumeric { .. })
        ));
        assert_eq!(interleaved_2_of_5("  "), Err(BarcodeError::Empty));
    }

    #[test]
    fn an_unknown_symbology_is_reported_not_guessed() {
        assert!(matches!(
            encode("qrCode", "12"),
            Err(BarcodeError::UnsupportedSymbology(s)) if s == "qrCode"
        ));
    }
}
