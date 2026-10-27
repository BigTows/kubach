//! Strict validation of names that end up in API URLs. A name with `/` or
//! `..` could otherwise address a path outside the session's namespace.

// RFC 1123 label (namespaces, container names) and subdomain (object names).
pub use kubach_core::names::{is_dns_label, is_dns_subdomain};

/// A kind or plural resource name as an agent may pass it (`Deployment`,
/// `deployments`).
pub fn is_kind_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 63 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `v1`, `apps/v1`, `example.com/v1beta1`.
pub fn is_api_version(s: &str) -> bool {
    let (group, version) = match s.split_once('/') {
        Some((group, version)) => (Some(group), version),
        None => (None, s),
    };
    group.is_none_or(is_dns_subdomain) && is_dns_label(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_versions() {
        assert!(is_kind_name("Deployment"));
        assert!(!is_kind_name("pods/log"));
        assert!(!is_kind_name("deployments.apps"));
        assert!(is_api_version("v1"));
        assert!(is_api_version("apps/v1"));
        assert!(!is_api_version("apps/v1/namespaces/x"));
        assert!(!is_api_version("../v1"));
    }
}
