//! The system proxy from the environment: `HTTPS_PROXY` / `HTTP_PROXY` /
//! `ALL_PROXY` and `NO_PROXY`, the way curl and Go read them.

use http::Uri;

/// Proxy for `target` from the process environment, if any.
pub fn from_env(target: &Uri) -> Option<Uri> {
    proxy_for(target, |name| {
        crate::env::var(name).filter(|value| !value.trim().is_empty())
    })
}

/// Proxy for `target` with variables from `var`: `HTTPS_PROXY` for https
/// (`HTTP_PROXY` for http), else `ALL_PROXY`; lowercase names too. `None`
/// when no proxy is set or `NO_PROXY` matches the host. A proxy without a
/// scheme is `http://`; `socks5h://` is treated as `socks5://`.
pub fn proxy_for(target: &Uri, var: impl Fn(&str) -> Option<String>) -> Option<Uri> {
    let lookup = |name: &str| var(name).or_else(|| var(&name.to_lowercase()));
    let host = target.host()?;
    if let Some(no_proxy) = lookup("NO_PROXY")
        && bypasses(&no_proxy, host)
    {
        return None;
    }
    let scheme_var = if target.scheme_str() == Some("http") {
        "HTTP_PROXY"
    } else {
        "HTTPS_PROXY"
    };
    let raw = lookup(scheme_var).or_else(|| lookup("ALL_PROXY"))?;
    parse_proxy(&raw)
}

fn parse_proxy(raw: &str) -> Option<Uri> {
    let raw = raw.trim();
    let with_scheme = match raw.split_once("://") {
        Some(("socks5h", rest)) => format!("socks5://{rest}"),
        Some(_) => raw.to_owned(),
        None => format!("http://{raw}"),
    };
    with_scheme.parse().ok()
}

/// Whether the `NO_PROXY` list skips the proxy for `host`: `*` matches
/// everything, `example.com` and `.example.com` match the domain and its
/// subdomains; ports in entries are ignored.
fn bypasses(no_proxy: &str, host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    no_proxy
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            let entry = strip_port(entry)
                .trim_start_matches('.')
                .to_ascii_lowercase();
            host == entry || host.ends_with(&format!(".{entry}"))
        })
}

fn strip_port(entry: &str) -> &str {
    if let Some(rest) = entry.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match entry.rsplit_once(':') {
        // One colon: host:port. More: a bare IPv6 address.
        Some((host, port)) if !host.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            host
        }
        _ => entry,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn proxy(target: &str, vars: &[(&str, &str)]) -> Option<String> {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        proxy_for(&target.parse().unwrap(), |name| vars.get(name).cloned())
            .map(|uri| uri.to_string())
    }

    const TARGET: &str = "https://get.helm.sh/helm.tar.gz";

    #[test]
    fn no_variables_no_proxy() {
        assert_eq!(proxy(TARGET, &[]), None);
    }

    #[test]
    fn https_proxy_in_either_case_and_default_scheme() {
        assert_eq!(
            proxy(TARGET, &[("HTTPS_PROXY", "http://proxy:3128")]).as_deref(),
            Some("http://proxy:3128/")
        );
        assert_eq!(
            proxy(TARGET, &[("https_proxy", "proxy:8080")]).as_deref(),
            Some("http://proxy:8080/")
        );
        // HTTP_PROXY is for http targets only.
        assert_eq!(proxy(TARGET, &[("HTTP_PROXY", "http://p:1")]), None);
        assert_eq!(
            proxy("http://example.com/", &[("HTTP_PROXY", "http://p:1")]).as_deref(),
            Some("http://p:1/")
        );
    }

    #[test]
    fn all_proxy_and_socks() {
        assert_eq!(
            proxy(TARGET, &[("ALL_PROXY", "socks5h://127.0.0.1:1080")]).as_deref(),
            Some("socks5://127.0.0.1:1080/")
        );
        assert_eq!(
            proxy(
                TARGET,
                &[("ALL_PROXY", "socks5://a:1"), ("HTTPS_PROXY", "http://b:2")]
            )
            .as_deref(),
            Some("http://b:2/")
        );
    }

    #[test]
    fn no_proxy_matches_domains() {
        let with = |no_proxy: &str| {
            proxy(
                TARGET,
                &[("HTTPS_PROXY", "http://p:1"), ("NO_PROXY", no_proxy)],
            )
        };
        assert_eq!(with("*"), None);
        assert_eq!(with("helm.sh"), None);
        assert_eq!(with("localhost, .helm.sh"), None);
        assert_eq!(with("get.helm.sh:443"), None);
        assert_eq!(with("GET.HELM.SH"), None);
        assert!(with("elm.sh").is_some());
        assert!(with("example.com,10.0.0.1").is_some());
        assert!(with("").is_some());
    }

    #[test]
    fn no_proxy_ip_addresses() {
        assert!(bypasses("10.0.0.1", "10.0.0.1"));
        assert!(bypasses("[::1]:8080", "[::1]"));
        assert!(bypasses("::1", "::1"));
        assert!(!bypasses("10.0.0.1", "10.0.0.2"));
    }
}
