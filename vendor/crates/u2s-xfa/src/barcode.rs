//! Barcode symbologies, as bar patterns.
//!
//! Interleaved 2 of 5 is encoded here, with the `<barcode>` options XFA
//! 3.3 defines for it. Every other symbology, and every checksum the
//! symbology's standard does not define, is reported as unsupported rather
//! than guessed at: a barcode that scans to the wrong string is worse than a
//! visibly missing one.

use thiserror::Error;

use crate::xfa::{Barcode, BarcodeChecksum};

#[derive(Debug, Error, PartialEq)]
pub enum BarcodeError {
    #[error("barcode type {0:?} is not supported")]
    UnsupportedSymbology(String),
    #[error("{symbology} has no {checksum:?} checksum")]
    UnsupportedChecksum {
        symbology: &'static str,
        checksum: BarcodeChecksum,
    },
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
    let trimmed = digits_of("code2Of5Interleaved", digits)?;

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

/// A value encoded for a `<barcode>`: its bars and its human-readable text.
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub bars: Vec<Bar>,
    /// The text to print beside the bars: the value, with the check digit
    /// when `printCheckDigit` asks for it.
    pub legend: String,
    /// How many characters the bars encode, check digit and padding
    /// included.
    pub encoded_len: usize,
}

/// Encode `value` as the `<barcode>` `spec` asks (XFA 3.3 "The barcode
/// element"): its symbology, with its checksum inserted when the
/// symbology's standard defines one of that kind. `startChar` and `endChar`
/// are ignored for a symbology with fixed start and stop patterns, as the
/// spec says.
pub fn encode(spec: &Barcode, value: &str) -> Result<Encoded, BarcodeError> {
    match spec.symbology.as_str() {
        "code2Of5Interleaved" | "interleaved2of5" => {
            let digits = digits_of("code2Of5Interleaved", value)?;
            if spec.start_char.is_some() || spec.end_char.is_some() {
                log::debug!("code2Of5Interleaved has fixed start and stop patterns; startChar and endChar are ignored");
            }
            let check = match spec.checksum {
                BarcodeChecksum::None => None,
                BarcodeChecksum::Auto | BarcodeChecksum::OneMod10 => Some(mod10_check_digit(digits)),
                checksum => {
                    return Err(BarcodeError::UnsupportedChecksum {
                        symbology: "code2Of5Interleaved",
                        checksum,
                    });
                }
            };
            let data = match check {
                Some(check) => format!("{digits}{check}"),
                None => digits.to_string(),
            };
            let bars = interleaved_2_of_5(&data)?;
            let legend = if check.is_some() && !spec.print_check_digit {
                digits.to_string()
            } else {
                data.clone()
            };
            Ok(Encoded {
                bars,
                legend,
                encoded_len: data.len() + data.len() % 2,
            })
        }
        other => Err(BarcodeError::UnsupportedSymbology(other.to_string())),
    }
}

/// The width of the bars in narrow modules, for the longer of the encoded
/// value and the `dataLength` the template expects (XFA 3.3: the processor
/// derives the narrow bar width from `dataLength` and the field width).
pub fn width_in_modules(spec: &Barcode, encoded: &Encoded) -> f32 {
    let chars = encoded.encoded_len.max(spec.data_length.unwrap_or(0));
    let ratio = spec.wide_narrow_ratio;
    // Interleaved 2 of 5: a start of four narrow elements, five elements
    // per digit of which two are wide, a stop of a wide bar and two narrow
    // elements, digits padded to an even count.
    let digits = chars + chars % 2;
    4.0 + digits as f32 * (3.0 + 2.0 * ratio) + ratio + 2.0
}

/// `value` trimmed, when it is digits only.
fn digits_of<'a>(symbology: &'static str, value: &'a str) -> Result<&'a str, BarcodeError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BarcodeError::Empty);
    }
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Err(BarcodeError::NotNumeric {
            symbology,
            value: trimmed.to_string(),
        });
    }
    Ok(trimmed)
}

/// The "1 modulo 10" check digit of `digits`: weights of 3 and 1 from the
/// rightmost digit, the digit that brings the sum to a multiple of ten.
fn mod10_check_digit(digits: &str) -> char {
    let sum: u32 = digits
        .chars()
        .rev()
        .enumerate()
        .map(|(i, c)| c.to_digit(10).unwrap_or(0) * if i % 2 == 0 { 3 } else { 1 })
        .sum();
    char::from_digit((10 - sum % 10) % 10, 10).unwrap_or('0')
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

    fn spec(symbology: &str) -> Barcode {
        Barcode {
            symbology: symbology.to_string(),
            wide_narrow_ratio: 3.0,
            checksum: BarcodeChecksum::None,
            data_length: None,
            text_location: Default::default(),
            start_char: None,
            end_char: None,
            print_check_digit: false,
        }
    }

    /// The I2of5 check digit weights the rightmost digit by 3 (the digit for
    /// 1234 is 6 since 4*3 + 3 + 2*3 + 1 = 22, and 8 brings it to 30: 8).
    #[test]
    fn the_auto_checksum_appends_a_mod_10_check_digit() {
        assert_eq!(mod10_check_digit("1234"), '8');
        let mut with = spec("code2Of5Interleaved");
        with.checksum = BarcodeChecksum::Auto;
        let encoded = encode(&with, "1234").expect("encode");
        assert_eq!(encoded.encoded_len, 6);
        assert_eq!(encoded.legend, "1234", "the check digit is not printed unless asked");
        with.print_check_digit = true;
        assert_eq!(encode(&with, "1234").expect("encode").legend, "12348");
        assert_eq!(
            encoded.bars,
            interleaved_2_of_5("12348").expect("encode"),
            "the bars carry the check digit"
        );
        with.checksum = BarcodeChecksum::TwoMod10;
        assert!(matches!(
            encode(&with, "1234"),
            Err(BarcodeError::UnsupportedChecksum { .. })
        ));
    }

    #[test]
    fn the_width_in_modules_follows_the_expected_data_length() {
        let plain = spec("code2Of5Interleaved");
        let encoded = encode(&plain, "12").expect("encode");
        // start 4 + two digits of 3 + 2*3 + stop 3 + 2 = 4 + 18 + 5
        assert_eq!(width_in_modules(&plain, &encoded), 27.0);
        let mut long = plain.clone();
        long.data_length = Some(4);
        assert_eq!(width_in_modules(&long, &encoded), 4.0 + 4.0 * 9.0 + 5.0);
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
            encode(&spec("qrCode"), "12"),
            Err(BarcodeError::UnsupportedSymbology(s)) if s == "qrCode"
        ));
    }
}
