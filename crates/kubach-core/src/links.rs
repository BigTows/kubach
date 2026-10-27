//! Which links from untrusted content (chart READMEs) may be opened.

/// Whether `url` may be handed to the system opener: only `http` and `https`
/// (scheme case-insensitive). `file:`, `smb:`, `javascript:`, custom schemes,
/// UNC paths (`\\host\share`) and relative paths are refused.
pub fn is_safe_external_url(url: &str) -> bool {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        && rest.starts_with("//")
        && rest.len() > 2
        && !url.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::is_safe_external_url;

    #[test]
    fn allows_http_and_https() {
        assert!(is_safe_external_url("http://example.com"));
        assert!(is_safe_external_url("https://example.com/docs?a=1#x"));
    }

    #[test]
    fn scheme_case_is_ignored() {
        assert!(is_safe_external_url("HTTPS://example.com"));
        assert!(is_safe_external_url("HtTp://example.com"));
    }

    #[test]
    fn refuses_other_schemes() {
        assert!(!is_safe_external_url("file://attacker/share/x"));
        assert!(!is_safe_external_url("file:///etc/passwd"));
        assert!(!is_safe_external_url("smb://attacker/share"));
        assert!(!is_safe_external_url("javascript:alert(1)"));
        assert!(!is_safe_external_url("ms-msdt:/id"));
        assert!(!is_safe_external_url("ftp://example.com"));
    }

    #[test]
    fn refuses_unc_and_relative_paths() {
        assert!(!is_safe_external_url(r"\\host\share\setup.exe"));
        assert!(!is_safe_external_url("docs/readme.md"));
        assert!(!is_safe_external_url("/etc/passwd"));
        assert!(!is_safe_external_url("#anchor"));
        assert!(!is_safe_external_url("//host/share"));
    }

    #[test]
    fn refuses_empty_and_malformed() {
        assert!(!is_safe_external_url(""));
        assert!(!is_safe_external_url("   "));
        assert!(!is_safe_external_url("http:"));
        assert!(!is_safe_external_url("http://"));
        assert!(!is_safe_external_url("http:example.com"));
        assert!(!is_safe_external_url("http://exa\nmple.com"));
    }
}
