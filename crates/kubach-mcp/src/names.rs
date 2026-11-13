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

/// Most characters of an MCP client's name that are kept.
pub const MAX_CLIENT_NAME_CHARS: usize = 64;

/// The name shown when a client gave none, or none of it could be shown.
pub const UNKNOWN_CLIENT: &str = "unknown";

/// The name an MCP client reports for itself, made fit to show (#179): the
/// client writes it, and it goes into window titles, notifications, the
/// sessions list and the audit log. Control characters, invisible and bidi
/// formatting characters are dropped, a run of white space (line breaks
/// and tabs too) becomes one space, and the rest is cut to
/// [`MAX_CLIENT_NAME_CHARS`] characters (with `…`). An empty result is
/// [`UNKNOWN_CLIENT`].
pub fn clean_client_name(raw: &str) -> String {
    let mut name = String::new();
    let mut kept = 0;
    let mut cut = false;
    let mut space = false;
    // Whatever its length, a name is read once and not past its first
    // kept characters.
    for c in raw.chars() {
        // Line breaks and tabs are white space too.
        if c.is_whitespace() {
            space = kept > 0;
            continue;
        }
        if c.is_control() || hidden_in_name(c) {
            continue;
        }
        if kept + usize::from(space) >= MAX_CLIENT_NAME_CHARS {
            cut = true;
            break;
        }
        if space {
            name.push(' ');
            kept += 1;
            space = false;
        }
        name.push(c);
        kept += 1;
    }
    if name.is_empty() {
        return UNKNOWN_CLIENT.to_owned();
    }
    if cut {
        name.push('…');
    }
    name
}

/// Characters that take no room on screen or change the order of the
/// text around them.
fn hidden_in_name(c: char) -> bool {
    crate::policy::hidden_in_command(c)
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_client_name_is_kept() {
        for name in ["claude-code", "Cursor 1.2", "агент", "my agent (dev)"] {
            assert_eq!(clean_client_name(name), name);
        }
    }

    #[test]
    fn a_client_name_loses_what_would_not_show_as_written() {
        // Line breaks, tabs and other controls, runs of spaces.
        assert_eq!(
            clean_client_name("  agent\r\n\tKubach:  approve\u{1b}[31m all\u{0}  "),
            "agent Kubach: approve[31m all"
        );
        // Bidi overrides and isolates, zero-width and other invisible
        // characters; a line separator is white space.
        assert_eq!(
            clean_client_name(
                "a\u{202E}b\u{2066}c\u{200B}d\u{FEFF}e\u{2028}f\u{00AD}g\u{E0041}h\u{200F}"
            ),
            "abcde fgh"
        );
        for empty in ["", "   ", "\n\u{200B}\u{202E}"] {
            assert_eq!(clean_client_name(empty), UNKNOWN_CLIENT);
        }
    }

    #[test]
    fn a_long_client_name_is_cut() {
        let cut = clean_client_name(&"ж".repeat(10_000));
        assert_eq!(cut.chars().count(), MAX_CLIENT_NAME_CHARS + 1);
        assert!(cut.ends_with('…'));
        let exact = "a".repeat(MAX_CLIENT_NAME_CHARS);
        assert_eq!(clean_client_name(&exact), exact);
        // A cleaned name stays as it is.
        assert_eq!(
            clean_client_name(&cut).chars().count(),
            MAX_CLIENT_NAME_CHARS + 1
        );
    }

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
