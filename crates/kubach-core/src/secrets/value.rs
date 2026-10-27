//! A Secret value in memory: never printed, wiped when dropped.

use std::fmt;

/// Bytes of one Secret value.
///
/// `Debug` shows only the size, there is no `Display` or `Serialize`, and
/// the buffer is overwritten when dropped, so a value can't end up in a log
/// line or an error report by accident.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The raw value. Callers must not log or persist it.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    /// The value as text, if it is printable UTF-8 (control characters other
    /// than tabs and line breaks make it binary).
    pub fn text(&self) -> Option<&str> {
        let text = std::str::from_utf8(&self.0).ok()?;
        text.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
            .then_some(text)
    }

    /// How the value is shown: its text, or a note about binary data.
    pub fn decode(&self) -> Decoded<'_> {
        match self.text() {
            Some(text) => Decoded::Text(text),
            None => Decoded::Binary { len: self.len() },
        }
    }
}

impl From<Vec<u8>> for SecretBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&str> for SecretBytes {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes(<hidden, {} bytes>)", self.0.len())
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Overwrites `bytes` with zeros in a way the optimizer keeps.
pub fn wipe(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, exclusive reference.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// A decoded value for display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoded<'a> {
    Text(&'a str),
    Binary { len: usize },
}

/// `binary data, 3 bytes` / `1 byte`.
pub fn describe_binary(len: usize) -> String {
    format!("binary data, {}", describe_size(len))
}

/// `0 bytes`, `1 byte`, `12 bytes`, `1.5 KiB`.
pub fn describe_size(len: usize) -> String {
    match len {
        1 => "1 byte".to_owned(),
        len if len < 1024 => format!("{len} bytes"),
        len if len < 1024 * 1024 => format!("{:.1} KiB", len as f64 / 1024.0),
        len => format!("{:.1} MiB", len as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_value() {
        let value = SecretBytes::from("hunter2");
        let printed = format!("{value:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("7 bytes"));
    }

    #[test]
    fn text_or_binary() {
        assert_eq!(
            SecretBytes::from("line 1\nline 2\t!").decode(),
            Decoded::Text("line 1\nline 2\t!")
        );
        assert_eq!(
            SecretBytes::new(vec![0xff, 0xfe, 0x00]).decode(),
            Decoded::Binary { len: 3 }
        );
        // Valid UTF-8 with a NUL is still binary.
        assert_eq!(
            SecretBytes::new(b"a\0b".to_vec()).decode(),
            Decoded::Binary { len: 3 }
        );
        assert_eq!(SecretBytes::default().decode(), Decoded::Text(""));
    }

    #[test]
    fn sizes() {
        assert_eq!(describe_binary(1), "binary data, 1 byte");
        assert_eq!(describe_size(0), "0 bytes");
        assert_eq!(describe_size(1536), "1.5 KiB");
        assert_eq!(describe_size(3 * 1024 * 1024), "3.0 MiB");
    }

    #[test]
    fn wipe_zeroes() {
        let mut bytes = b"secret".to_vec();
        wipe(&mut bytes);
        assert_eq!(bytes, vec![0; 6]);
    }
}
