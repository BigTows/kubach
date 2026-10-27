//! Random values for Secrets, generated on the user's machine (#230): the
//! user picks the length and the alphabet, the value never needs to be
//! shown to anyone.

use chacha20poly1305::aead::Generate as _;

use super::value::{SecretBytes, wipe};

/// Shortest value the generator makes.
pub const MIN_LENGTH: usize = 8;
/// Longest value the generator makes.
pub const MAX_LENGTH: usize = 256;
/// Length offered by default.
pub const DEFAULT_LENGTH: usize = 32;

const LOWER: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const DIGITS: &[u8] = b"0123456789";
/// Symbols that rarely need quoting in YAML, shells or connection strings.
const SYMBOLS: &[u8] = b"-_.~!@#%^*+=";
const HEX: &[u8] = b"0123456789abcdef";

/// Which characters a generated value uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Charset {
    /// Letters and digits.
    #[default]
    Alphanumeric,
    /// Letters, digits and a few symbols.
    WithSymbols,
    /// Lowercase hex digits (keys, tokens).
    Hex,
}

impl Charset {
    pub const ALL: [Charset; 3] = [Self::Alphanumeric, Self::WithSymbols, Self::Hex];

    pub fn label(self) -> &'static str {
        match self {
            Self::Alphanumeric => "a-z A-Z 0-9",
            Self::WithSymbols => "+ symbols",
            Self::Hex => "hex",
        }
    }

    /// Character classes; a value has at least one of each when it is long
    /// enough.
    fn classes(self) -> &'static [&'static [u8]] {
        match self {
            Self::Alphanumeric => &[LOWER, UPPER, DIGITS],
            Self::WithSymbols => &[LOWER, UPPER, DIGITS, SYMBOLS],
            Self::Hex => &[HEX],
        }
    }

    fn alphabet(self) -> Vec<u8> {
        self.classes().concat()
    }
}

/// A random value of `length` characters (clamped to
/// [`MIN_LENGTH`]..=[`MAX_LENGTH`]) from `charset`, drawn uniformly from
/// the operating system's random source. Values that miss one of the
/// charset's classes are drawn again, so a password has letters of both
/// cases and digits (and symbols).
pub fn generate(length: usize, charset: Charset) -> SecretBytes {
    let length = length.clamp(MIN_LENGTH, MAX_LENGTH);
    let alphabet = charset.alphabet();
    loop {
        let value = draw(length, &alphabet);
        if charset
            .classes()
            .iter()
            .all(|class| value.expose().iter().any(|c| class.contains(c)))
        {
            return value;
        }
    }
}

/// `length` characters of `alphabet`, without modulo bias.
fn draw(length: usize, alphabet: &[u8]) -> SecretBytes {
    // Bytes at or above `limit` would favour the first characters.
    let limit = 256 - 256 % alphabet.len();
    let mut out = Vec::with_capacity(length);
    while out.len() < length {
        let mut random = <[u8; 64]>::generate();
        for &byte in &random {
            if usize::from(byte) < limit && out.len() < length {
                out.push(alphabet[usize::from(byte) % alphabet.len()]);
            }
        }
        wipe(&mut random);
    }
    SecretBytes::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_have_the_length_and_alphabet_asked_for() {
        for charset in Charset::ALL {
            let alphabet = charset.alphabet();
            for length in [MIN_LENGTH, 32, MAX_LENGTH] {
                let value = generate(length, charset);
                assert_eq!(value.len(), length);
                assert!(value.expose().iter().all(|c| alphabet.contains(c)));
                for class in charset.classes() {
                    assert!(value.expose().iter().any(|c| class.contains(c)));
                }
            }
        }
        assert_eq!(generate(1, Charset::Hex).len(), MIN_LENGTH);
        assert_eq!(generate(10_000, Charset::Hex).len(), MAX_LENGTH);
    }

    #[test]
    fn values_differ_and_use_the_whole_alphabet() {
        assert_ne!(
            generate(32, Charset::Alphanumeric),
            generate(32, Charset::Alphanumeric)
        );
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..20 {
            seen.extend(generate(MAX_LENGTH, Charset::Hex).expose().iter().copied());
        }
        assert_eq!(seen.len(), 16);
    }

    #[test]
    fn debug_does_not_show_a_generated_value() {
        let value = generate(16, Charset::Hex);
        let text = String::from_utf8(value.expose().to_vec()).unwrap();
        assert!(!format!("{value:?}").contains(&text));
    }
}
