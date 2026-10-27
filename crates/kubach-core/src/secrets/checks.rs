//! Checks of Secret values done inside Kubach, for the MCP agent (#227):
//! the agent learns conclusions ("empty", "valid JSON", "the certificate
//! expires in 12 days", "same value as `db/password`"), never the values.
//!
//! Nothing here returns a hash, a fingerprint or a prefix of a value: short
//! passwords could be brute-forced from those. Comparisons only ever look at
//! two values that are already in the cluster, so they are no oracle for a
//! guess the agent makes up.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use serde_json::Value;

use super::typed;
use super::x509::{self, CertificateInfo};
use crate::validation::ValidationError;

/// What a value looks like, without the value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueReport {
    pub size: usize,
    pub empty: bool,
    /// Printable UTF-8 text (tabs and line breaks allowed).
    pub text: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub leading_whitespace: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trailing_whitespace: bool,
    /// Ends with `\n` (often an accident of `echo` or a text editor).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trailing_newline: bool,
    /// `object`, `array` or `scalar` when the value is JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<&'static str>,
    /// `mapping` or `sequence` when the value is structured YAML (plain
    /// text is a YAML scalar, which says nothing).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yaml: Option<&'static str>,
    /// The value is itself base64 text; the decoded size.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base64_decoded_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<UrlInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub certificates: Vec<CertificateReport>,
    /// PEM private key blocks (never parsed further than their public part).
    #[serde(skip_serializing_if = "is_zero")]
    pub private_keys: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A URL without userinfo, path, query or fragment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UrlInfo {
    pub scheme: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The URL carries `user:password@` (not shown).
    pub has_credentials: bool,
}

/// A certificate's public facts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CertificateReport {
    pub subject: String,
    pub issuer: String,
    pub san: Vec<String>,
    pub not_before: String,
    pub not_after: String,
    /// `valid`, `expired` or `not-yet-valid`.
    pub validity: &'static str,
    /// Days left, or (negative) days since it expired.
    pub days: i64,
}

impl CertificateReport {
    fn new(info: &CertificateInfo, now: i64) -> Self {
        let (validity, days) = match info.validity(now) {
            x509::Validity::NotYetValid => ("not-yet-valid", 0),
            x509::Validity::Valid { days_left } => ("valid", days_left),
            x509::Validity::Expired { days_ago } => ("expired", -days_ago),
        };
        Self {
            subject: info.subject.clone(),
            issuer: info.issuer.clone(),
            san: info.san.clone(),
            not_before: x509::format_time(info.not_before),
            not_after: x509::format_time(info.not_after),
            validity,
            days,
        }
    }
}

/// Checks one value; `now` is Unix seconds (certificate validity).
pub fn check_value(bytes: &[u8], now: i64) -> ValueReport {
    let mut report = ValueReport {
        size: bytes.len(),
        empty: bytes.is_empty(),
        ..ValueReport::default()
    };
    let Some(text) = std::str::from_utf8(bytes).ok().filter(|text| {
        text.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
    }) else {
        return report;
    };
    report.text = true;
    if text.is_empty() {
        return report;
    }
    report.leading_whitespace = text.starts_with(char::is_whitespace);
    report.trailing_whitespace = text.ends_with(char::is_whitespace);
    report.trailing_newline = text.ends_with('\n');
    report.json = serde_json::from_str::<Value>(text)
        .ok()
        .map(|value| match value {
            Value::Object(_) => "object",
            Value::Array(_) => "array",
            _ => "scalar",
        });
    report.yaml = match serde_saphyr::from_str::<Value>(text) {
        Ok(Value::Object(_)) => Some("mapping"),
        Ok(Value::Array(_)) => Some("sequence"),
        _ => None,
    };
    let compact = text.trim();
    if compact.len() >= 4
        && compact.len().is_multiple_of(4)
        && !compact.contains(char::is_whitespace)
        && let Ok(decoded) = STANDARD.decode(compact)
    {
        report.base64_decoded_size = Some(decoded.len());
    }
    report.url = url_info(compact);
    let blocks = x509::pem_blocks(text);
    report.certificates = blocks
        .iter()
        .filter(|block| block.label == "CERTIFICATE")
        .filter_map(|block| x509::parse_certificate(&block.der).ok())
        .map(|info| CertificateReport::new(&info, now))
        .collect();
    report.private_keys = blocks
        .iter()
        .filter(|block| block.label.ends_with("PRIVATE KEY"))
        .count();
    report
}

/// `scheme://[user[:password]@]host[:port][/…]`; anything else is no URL.
fn url_info(text: &str) -> Option<UrlInfo> {
    let (scheme, rest) = text.split_once("://")?;
    let scheme_ok = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    if !scheme_ok || text.contains(char::is_whitespace) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((_, host_port)) => (true, host_port),
        None => (false, authority),
    };
    let (host, port) = if let Some(v6) = host_port.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        (format!("[{host}]"), after.strip_prefix(':'))
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port)),
            None => (host_port.to_owned(), None),
        }
    };
    let port = match port {
        Some(port) => Some(port.parse::<u16>().ok()?),
        None => None,
    };
    if host.is_empty() && scheme != "file" {
        return None;
    }
    Some(UrlInfo {
        scheme: scheme.to_ascii_lowercase(),
        host,
        port,
        has_credentials: credentials,
    })
}

/// Whether a private key belongs to a certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyMatch {
    Matches,
    Differs,
    /// One of them has no readable key, or the key format does not say.
    Unknown,
}

/// Compares the first certificate of `cert_pem` with the first private key
/// of `key_pem`.
pub fn key_matches_certificate(cert_pem: &[u8], key_pem: &[u8]) -> KeyMatch {
    let (Ok(cert), Ok(key)) = (std::str::from_utf8(cert_pem), std::str::from_utf8(key_pem)) else {
        return KeyMatch::Unknown;
    };
    let cert_key = x509::pem_blocks(cert)
        .iter()
        .find(|block| block.label == "CERTIFICATE")
        .and_then(|block| x509::certificate_public_key(&block.der));
    let private = x509::pem_blocks(key)
        .iter()
        .find(|block| block.label.ends_with("PRIVATE KEY"))
        .and_then(x509::private_key_public_key);
    match (cert_key, private) {
        (Some(a), Some(b)) if a == b => KeyMatch::Matches,
        (Some(_), Some(_)) => KeyMatch::Differs,
        _ => KeyMatch::Unknown,
    }
}

/// One registry of a docker config: the host and whether it has
/// credentials. User names are left out as well.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryAuth {
    pub registry: String,
    pub has_auth: bool,
}

/// Registries of a `.dockerconfigjson` (`legacy`: `.dockercfg`).
pub fn registries(bytes: &[u8], legacy: bool) -> Result<Vec<RegistryAuth>, ValidationError> {
    Ok(typed::docker_logins(bytes, legacy)?
        .into_iter()
        .map(|login| RegistryAuth {
            registry: login.registry,
            has_auth: login.has_password,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Throwaway keys generated for these tests only.
    const EC_KEY: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEIE5u75AqEQftc6o9Rq/1pzOGwaiYffRSPZanz8a4ehL2oAoGCCqGSM49
AwEHoUQDQgAEPaTTYMno3KDwjPjdqoaHRwLCpoUmG2SSA+OdRmlU819Ha+VMOXYQ
qNIGJHQkl+BLjL8MsQlX7/kyIRJhQU3Clg==
-----END EC PRIVATE KEY-----
";
    const EC_KEY_PKCS8: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgTm7vkCoRB+1zqj1G
r/WnM4bBqJh99FI9lqfPxrh6EvahRANCAAQ9pNNgyejcoPCM+N2qhodHAsKmhSYb
ZJID451GaVTzX0dr5Uw5dhCo0gYkdCSX4EuMvwyxCVfv+TIhEmFBTcKW
-----END PRIVATE KEY-----
";
    const EC_CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBgTCCASegAwIBAgIUBpcZ1yRcdXRpQr1G/68HyMY5OpgwCgYIKoZIzj0EAwIw
FjEUMBIGA1UEAwwLa3ViYWNoLnRlc3QwHhcNMjYxMDIzMTUyMjIzWhcNMzYxMDIw
MTUyMjIzWjAWMRQwEgYDVQQDDAtrdWJhY2gudGVzdDBZMBMGByqGSM49AgEGCCqG
SM49AwEHA0IABD2k02DJ6Nyg8Iz43aqGh0cCwqaFJhtkkgPjnUZpVPNfR2vlTDl2
EKjSBiR0JJfgS4y/DLEJV+/5MiESYUFNwpajUzBRMB0GA1UdDgQWBBQW/jH2EluF
1MKknojjVINdFBNPIDAfBgNVHSMEGDAWgBQW/jH2EluF1MKknojjVINdFBNPIDAP
BgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0gAMEUCIFb7QZw/cvmUQ3nEO41P
jOGtQ3En34OQTQ+b/y/AC+poAiEAsheMVW9fDkktRFPFSRfRuCtJqNUqpKD51OR8
nZmSvr4=
-----END CERTIFICATE-----
";
    const OTHER_EC_KEY: &str = "-----BEGIN EC PRIVATE KEY-----
MHcCAQEEINHv+oB/EX1Rc5gsVm/j45MUZ9IwT7lAxaD1bb8aEvZ4oAoGCCqGSM49
AwEHoUQDQgAEcWrXDJ01TCuLtXNFABOgxTkY2AdAwFofPnXFqiqjXedbnFF2ANXi
UD9wZ5VE5oRLTUgxWYdtWzGJX7yzndIDSw==
-----END EC PRIVATE KEY-----
";
    const RSA_KEY: &str = "-----BEGIN RSA PRIVATE KEY-----
MIICXAIBAAKBgQDwbHROGxIE7BrrdPEGJqulU+g1oRb21jdLZA0JOuz0ze3FFP0u
1FkZ4/M2/PS7zUWNQ3fX9cbVmNWugrs42IQDl3eeSkx2DLy2ehAdVQYZLb57MeeM
PFRWjriAAIy5f8onoWeFq80lThN0jG6LwK4PPK4JZy/bolHxwgkVUr3T3wIDAQAB
AoGAKplM0joBq1WaP3Xs7sYTS6veTQJJ61pxWa5UkPob2hRSuMTLR2zgjLHS+j8/
rbblR+5YYa9ul5II6m7HdX8HhLN6xD6znbEX47iyFFP4mCTjZZcKEtgZQl9qbrUu
QNOYHaQ0KSJw2aX+f2HOt7cyREHnBWUX+jd0vxij8v5moIECQQD57YcmEiMnINyD
3AO0DCdj8CYsRvgo79NIbNb5O/4qcxktDVc22Di7I7UfKTnngoCVNlGn/PEaJ0id
GOey+XnvAkEA9kPQNW1JLlOFpPIdjUp0W2iwU9q9HzUD1TmfsIU+3TY5e0d08nzn
IcrbMsIRQUXCl1iEEXHlKkXaf0KjkDX1EQJBAOJ8bCMGALEuyGb99KUu9mtMb2tk
jPKcEv3pFA8cXMIfN8tp8YUUAKUmQLaSZ5Ty4I9XgDwZrmwi+VzT+vRnvCUCQFE2
yfpkoSa6+q8L/bzz2e2vV78/uvEIvcva5hQDzViMomZKiYHSuTLGwIU7xQ7+n47j
7SIHqqjHQqiHZIORl9ECQCKbWaIWI3ijFAV5rgChYZv6DOYKDJcNZKrYUrCPv024
aSbA+204iILDBhbl2eouAed86ChJDjySbuMBAsDZJKE=
-----END RSA PRIVATE KEY-----
";
    const RSA_KEY_PKCS8: &str = "-----BEGIN PRIVATE KEY-----
MIICdgIBADANBgkqhkiG9w0BAQEFAASCAmAwggJcAgEAAoGBAPBsdE4bEgTsGut0
8QYmq6VT6DWhFvbWN0tkDQk67PTN7cUU/S7UWRnj8zb89LvNRY1Dd9f1xtWY1a6C
uzjYhAOXd55KTHYMvLZ6EB1VBhktvnsx54w8VFaOuIAAjLl/yiehZ4WrzSVOE3SM
bovArg88rglnL9uiUfHCCRVSvdPfAgMBAAECgYAqmUzSOgGrVZo/dezuxhNLq95N
AknrWnFZrlSQ+hvaFFK4xMtHbOCMsdL6Pz+ttuVH7lhhr26Xkgjqbsd1fweEs3rE
PrOdsRfjuLIUU/iYJONllwoS2BlCX2putS5A05gdpDQpInDZpf5/Yc63tzJEQecF
ZRf6N3S/GKPy/maggQJBAPnthyYSIycg3IPcA7QMJ2PwJixG+Cjv00hs1vk7/ipz
GS0NVzbYOLsjtR8pOeeCgJU2Uaf88RonSJ0Y57L5ee8CQQD2Q9A1bUkuU4Wk8h2N
SnRbaLBT2r0fNQPVOZ+whT7dNjl7R3TyfOchytsywhFBRcKXWIQRceUqRdp/QqOQ
NfURAkEA4nxsIwYAsS7IZv30pS72a0xva2SM8pwS/ekUDxxcwh83y2nxhRQApSZA
tpJnlPLgj1eAPBmubCL5XNP69Ge8JQJAUTbJ+mShJrr6rwv9vPPZ7a9Xvz+68Qi9
y9rmFAPNWIyiZkqJgdK5MsbAhTvFDv6fjuPtIgeqqMdCqIdkg5GX0QJAIptZohYj
eKMUBXmuAKFhm/oM5goMlw1kqthSsI+/TbhpJsD7bTiIgsMGFuXZ6i4B53zoKEkO
PJJu4wECwNkkoQ==
-----END PRIVATE KEY-----
";
    const RSA_CERT: &str = "-----BEGIN CERTIFICATE-----
MIICEDCCAXmgAwIBAgIUbC26ZXkqEBnGLHfwnAEmNi+Z4PEwDQYJKoZIhvcNAQEL
BQAwGjEYMBYGA1UEAwwPcnNhLmt1YmFjaC50ZXN0MB4XDTI2MTAyMzE1MjIyOVoX
DTM2MTAyMDE1MjIyOVowGjEYMBYGA1UEAwwPcnNhLmt1YmFjaC50ZXN0MIGfMA0G
CSqGSIb3DQEBAQUAA4GNADCBiQKBgQDwbHROGxIE7BrrdPEGJqulU+g1oRb21jdL
ZA0JOuz0ze3FFP0u1FkZ4/M2/PS7zUWNQ3fX9cbVmNWugrs42IQDl3eeSkx2DLy2
ehAdVQYZLb57MeeMPFRWjriAAIy5f8onoWeFq80lThN0jG6LwK4PPK4JZy/bolHx
wgkVUr3T3wIDAQABo1MwUTAdBgNVHQ4EFgQUbhS3FCKd7XF+s/HNkSq4JdViJ+Ew
HwYDVR0jBBgwFoAUbhS3FCKd7XF+s/HNkSq4JdViJ+EwDwYDVR0TAQH/BAUwAwEB
/zANBgkqhkiG9w0BAQsFAAOBgQCHBJnxbHskNLq793h1lS1/HMlSkNXN1WfhvusP
dsTnZwkLTFGhnnExgYcldeoeSklREiVMYnmFzif//yyZO/5ts8V2GKn9kA7MK+o+
AR1fmd3LM47uN2Oewj5AF8ulKu55IEryd9ZP/3+gNFPPMI3b5tYBd9jGyj2cl1fT
JEZCDQ==
-----END CERTIFICATE-----
";

    /// 2030-01-01.
    const NOW: i64 = 1_893_456_000;

    #[test]
    fn whitespace_and_formats() {
        let empty = check_value(b"", NOW);
        assert!(empty.empty && empty.text);

        let padded = check_value(b" hunter2\n", NOW);
        assert!(padded.leading_whitespace && padded.trailing_whitespace);
        assert!(padded.trailing_newline);
        assert_eq!(padded.size, 9);
        assert_eq!(padded.json, None);
        assert_eq!(padded.yaml, None);

        let json = check_value(br#"{"user": "app"}"#, NOW);
        assert_eq!(json.json, Some("object"));
        assert_eq!(json.yaml, Some("mapping"));

        let yaml = check_value(b"- a\n- b\n", NOW);
        assert_eq!(yaml.json, None);
        assert_eq!(yaml.yaml, Some("sequence"));

        let b64 = check_value(b"aHVudGVyMg==", NOW);
        assert_eq!(b64.base64_decoded_size, Some(7));
        assert_eq!(check_value(b"hunter2", NOW).base64_decoded_size, None);

        let binary = check_value(&[0, 159, 146, 150], NOW);
        assert!(!binary.text);
        assert_eq!(binary.size, 4);
    }

    #[test]
    fn urls_lose_credentials_path_and_query() {
        let report = check_value(
            b"postgres://app:topsecret@db.internal:5432/shop?sslmode=disable&password=x",
            NOW,
        );
        let url = report.url.unwrap();
        assert_eq!(url.scheme, "postgres");
        assert_eq!(url.host, "db.internal");
        assert_eq!(url.port, Some(5432));
        assert!(url.has_credentials);
        let shown = serde_json::to_string(&check_value(
            b"postgres://app:topsecret@db.internal:5432/shop?sslmode=disable&password=x",
            NOW,
        ))
        .unwrap();
        for leaked in ["topsecret", "app:", "shop", "sslmode", "password"] {
            assert!(!shown.contains(leaked), "{leaked} in {shown}");
        }
        let v6 = url_info("https://[::1]:8443").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("[::1]", Some(8443)));
        assert!(!v6.has_credentials);
        assert_eq!(url_info("not a url"), None);
        assert_eq!(url_info("http://host:notaport"), None);
    }

    #[test]
    fn certificates_and_keys() {
        let report = check_value(EC_CERT.as_bytes(), NOW);
        assert_eq!(report.certificates.len(), 1);
        let cert = &report.certificates[0];
        assert_eq!(cert.subject, "CN=kubach.test");
        assert_eq!(cert.validity, "valid");
        assert!(cert.days > 0);
        let key = check_value(EC_KEY.as_bytes(), NOW);
        assert_eq!(key.private_keys, 1);
        // A report of a private key carries nothing of it.
        let shown = serde_json::to_string(&key).unwrap();
        assert!(!shown.contains("MHcCAQEE"), "{shown}");

        let expired = check_value(EC_CERT.as_bytes(), NOW + 20 * 365 * 86_400);
        assert_eq!(expired.certificates[0].validity, "expired");
        assert!(expired.certificates[0].days < 0);
    }

    #[test]
    fn key_matches_its_certificate() {
        let ec = EC_CERT.as_bytes();
        assert_eq!(
            key_matches_certificate(ec, EC_KEY.as_bytes()),
            KeyMatch::Matches
        );
        assert_eq!(
            key_matches_certificate(ec, EC_KEY_PKCS8.as_bytes()),
            KeyMatch::Matches
        );
        assert_eq!(
            key_matches_certificate(ec, OTHER_EC_KEY.as_bytes()),
            KeyMatch::Differs
        );
        let rsa = RSA_CERT.as_bytes();
        assert_eq!(
            key_matches_certificate(rsa, RSA_KEY.as_bytes()),
            KeyMatch::Matches
        );
        assert_eq!(
            key_matches_certificate(rsa, RSA_KEY_PKCS8.as_bytes()),
            KeyMatch::Matches
        );
        assert_eq!(
            key_matches_certificate(rsa, EC_KEY.as_bytes()),
            KeyMatch::Differs
        );
        assert_eq!(
            key_matches_certificate(b"nothing", EC_KEY.as_bytes()),
            KeyMatch::Unknown
        );
    }

    #[test]
    fn registries_without_user_names() {
        let config = serde_json::json!({"auths": {
            "ghcr.io": {"username": "bot", "password": "s3cret"},
            "public.example.com": {}
        }});
        let bytes = serde_json::to_vec(&config).unwrap();
        let list = registries(&bytes, false).unwrap();
        assert_eq!(
            list,
            [
                RegistryAuth {
                    registry: "ghcr.io".into(),
                    has_auth: true
                },
                RegistryAuth {
                    registry: "public.example.com".into(),
                    has_auth: false
                },
            ]
        );
        let shown = serde_json::to_string(&list).unwrap();
        assert!(!shown.contains("bot") && !shown.contains("s3cret"));
        assert!(registries(b"nope", false).is_err());
    }
}
