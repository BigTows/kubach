//! DNS-1123 name validation, shared by everything that checks Kubernetes
//! object names: a label (`[a-z0-9]([-a-z0-9]*[a-z0-9])?`, at most 63
//! characters) and a subdomain (dot-separated labels, at most 253 characters).

/// Longest DNS-1123 label.
pub const MAX_LABEL_LEN: usize = 63;
/// Longest DNS-1123 subdomain.
pub const MAX_SUBDOMAIN_LEN: usize = 253;

/// Why a name is not a valid DNS-1123 label or subdomain.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("the name is empty")]
    Empty,
    #[error("the name is longer than {max} characters")]
    TooLong { max: usize },
    #[error("the name contains an invalid character {0:?}")]
    InvalidChar(char),
    #[error("the name must start and end with a lowercase letter or digit")]
    BadEdge,
    #[error("the name contains an empty label")]
    EmptyLabel,
}

/// Checks `s` as a DNS-1123 label.
pub fn validate_dns_label(s: &str) -> Result<(), NameError> {
    if s.is_empty() {
        return Err(NameError::Empty);
    }
    if s.len() > MAX_LABEL_LEN {
        return Err(NameError::TooLong { max: MAX_LABEL_LEN });
    }
    check_label(s)
}

/// Checks `s` as a DNS-1123 subdomain: every dot-separated part is a label.
pub fn validate_dns_subdomain(s: &str) -> Result<(), NameError> {
    validate_dns_subdomain_max(s, MAX_SUBDOMAIN_LEN)
}

/// Like [`validate_dns_subdomain`] with a tighter length limit (Helm release
/// names are subdomains of at most 53 characters).
pub fn validate_dns_subdomain_max(s: &str, max: usize) -> Result<(), NameError> {
    if s.is_empty() {
        return Err(NameError::Empty);
    }
    if s.len() > max.min(MAX_SUBDOMAIN_LEN) {
        return Err(NameError::TooLong {
            max: max.min(MAX_SUBDOMAIN_LEN),
        });
    }
    // A part longer than a label is reported as such even inside a subdomain.
    s.split('.').try_for_each(|label| {
        if label.is_empty() {
            Err(NameError::EmptyLabel)
        } else if label.len() > MAX_LABEL_LEN {
            Err(NameError::TooLong { max: MAX_LABEL_LEN })
        } else {
            check_label(label)
        }
    })
}

pub fn is_dns_label(s: &str) -> bool {
    validate_dns_label(s).is_ok()
}

pub fn is_dns_subdomain(s: &str) -> bool {
    validate_dns_subdomain(s).is_ok()
}

/// Non-empty label of bounded length: characters first, then the edges.
fn check_label(label: &str) -> Result<(), NameError> {
    if let Some(c) = label
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
    {
        return Err(NameError::InvalidChar(c));
    }
    if label.starts_with('-') || label.ends_with('-') {
        return Err(NameError::BadEdge);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        NameError, is_dns_label, is_dns_subdomain, validate_dns_label, validate_dns_subdomain,
        validate_dns_subdomain_max,
    };

    #[test]
    fn labels() {
        for ok in ["a", "0", "kubach-test-mcp", "a-b", &"a".repeat(63)] {
            assert!(is_dns_label(ok), "{ok:?}");
        }
        assert_eq!(validate_dns_label(""), Err(NameError::Empty));
        assert_eq!(
            validate_dns_label(&"a".repeat(64)),
            Err(NameError::TooLong { max: 63 })
        );
        assert_eq!(validate_dns_label("A"), Err(NameError::InvalidChar('A')));
        assert_eq!(validate_dns_label("a.b"), Err(NameError::InvalidChar('.')));
        assert_eq!(validate_dns_label("a/b"), Err(NameError::InvalidChar('/')));
        assert_eq!(validate_dns_label("-a"), Err(NameError::BadEdge));
        assert_eq!(validate_dns_label("a-"), Err(NameError::BadEdge));
        assert!(!is_dns_label("a\0"));
        assert!(!is_dns_label("a b"));
    }

    #[test]
    fn subdomains() {
        for ok in ["a", "nginx-7d9f.web", "my-secret.v1", "a.b.c"] {
            assert!(is_dns_subdomain(ok), "{ok:?}");
        }
        assert_eq!(validate_dns_subdomain(""), Err(NameError::Empty));
        for empty_label in [".", "..", "a..b", ".a", "a."] {
            assert_eq!(
                validate_dns_subdomain(empty_label),
                Err(NameError::EmptyLabel),
                "{empty_label:?}"
            );
        }
        // Each label is checked on its own, not just the whole string.
        for bad in ["a.-b", "a-.b", "-a.b", "a.b-"] {
            assert_eq!(
                validate_dns_subdomain(bad),
                Err(NameError::BadEdge),
                "{bad:?}"
            );
        }
        for bad in [
            "../x",
            "a/b",
            "x/../../other",
            "%2e%2e",
            "a?b",
            "a#b",
            "A",
            "a\n",
            " a",
        ] {
            assert!(!is_dns_subdomain(bad), "{bad:?}");
        }
    }

    #[test]
    fn subdomain_lengths() {
        let label = "a".repeat(63);
        let long_ok = [label.as_str(); 3].join(".");
        assert!(is_dns_subdomain(&long_ok));
        assert!(is_dns_subdomain(&format!("{long_ok}.{}", "a".repeat(61))));
        assert_eq!(
            validate_dns_subdomain(&format!("{long_ok}.{}", "a".repeat(62))),
            Err(NameError::TooLong { max: 253 })
        );
        assert_eq!(
            validate_dns_subdomain(&"a".repeat(64)),
            Err(NameError::TooLong { max: 63 })
        );
        assert_eq!(
            validate_dns_subdomain_max("a.b", 2),
            Err(NameError::TooLong { max: 2 })
        );
        assert!(validate_dns_subdomain_max("a.b", 3).is_ok());
    }
}
