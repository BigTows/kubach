//! Just enough PEM and X.509 (DER) parsing to describe a TLS certificate:
//! subject, issuer, subject alternative names and validity.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// One PEM block: its label (`CERTIFICATE`, `PRIVATE KEY`, …) and DER bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct PemBlock {
    pub label: String,
    pub der: Vec<u8>,
}

impl std::fmt::Debug for PemBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PemBlock({}, {} bytes)", self.label, self.der.len())
    }
}

/// All well-formed PEM blocks in `text`, in order.
pub fn pem_blocks(text: &str) -> Vec<PemBlock> {
    let mut blocks = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(label) = line
            .trim()
            .strip_prefix("-----BEGIN ")
            .and_then(|rest| rest.strip_suffix("-----"))
        else {
            continue;
        };
        let end = format!("-----END {label}-----");
        let mut body = String::new();
        let mut closed = false;
        for line in lines.by_ref() {
            let line = line.trim();
            if line == end {
                closed = true;
                break;
            }
            // Headers of old-style encrypted keys (`Proc-Type: …`).
            if !line.contains(':') {
                body.push_str(line);
            }
        }
        if !closed {
            break;
        }
        if let Ok(der) = STANDARD.decode(body.as_bytes()) {
            blocks.push(PemBlock {
                label: label.to_owned(),
                der,
            });
        }
    }
    blocks
}

/// What a certificate says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CertificateInfo {
    /// `CN=example.com, O=Example` (attributes in certificate order).
    pub subject: String,
    pub issuer: String,
    /// `DNS:example.com`, `IP:10.0.0.1`, `email:…`, `URI:…`.
    pub san: Vec<String>,
    /// Seconds since the Unix epoch.
    pub not_before: i64,
    pub not_after: i64,
}

impl CertificateInfo {
    pub fn self_signed(&self) -> bool {
        self.subject == self.issuer
    }

    /// Validity at `now` (Unix seconds).
    pub fn validity(&self, now: i64) -> Validity {
        if now < self.not_before {
            Validity::NotYetValid
        } else if now >= self.not_after {
            Validity::Expired {
                days_ago: (now - self.not_after) / DAY,
            }
        } else {
            Validity::Valid {
                days_left: (self.not_after - now) / DAY,
            }
        }
    }
}

const DAY: i64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validity {
    NotYetValid,
    Valid { days_left: i64 },
    Expired { days_ago: i64 },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a valid X.509 certificate: {0}")]
pub struct CertificateError(&'static str);

/// Parses a DER-encoded certificate.
pub fn parse_certificate(der: &[u8]) -> Result<CertificateInfo, CertificateError> {
    let (certificate, _) = read(der, SEQUENCE).ok_or(CertificateError("no certificate"))?;
    let (tbs, _) = read(certificate, SEQUENCE).ok_or(CertificateError("no TBS certificate"))?;
    let mut rest = tbs;
    // Optional `[0] EXPLICIT version`.
    if rest.first() == Some(&0xa0) {
        rest = skip(rest).ok_or(CertificateError("bad version"))?;
    }
    rest = skip(rest).ok_or(CertificateError("no serial number"))?;
    rest = skip(rest).ok_or(CertificateError("no signature algorithm"))?;
    let (issuer, after) = read(rest, SEQUENCE).ok_or(CertificateError("no issuer"))?;
    let (validity, after) = read(after, SEQUENCE).ok_or(CertificateError("no validity"))?;
    let (subject, after) = read(after, SEQUENCE).ok_or(CertificateError("no subject"))?;
    let mut rest = skip(after).ok_or(CertificateError("no public key"))?;

    let (not_before, validity_rest) = parse_time(validity).ok_or(CertificateError("bad time"))?;
    let (not_after, _) = parse_time(validity_rest).ok_or(CertificateError("bad time"))?;

    let mut san = Vec::new();
    while let Some((tag, content, after)) = tlv(rest) {
        if tag == 0xa3 {
            san = subject_alt_names(content).unwrap_or_default();
        }
        rest = after;
    }
    Ok(CertificateInfo {
        subject: name(subject),
        issuer: name(issuer),
        san,
        not_before,
        not_after,
    })
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;
const OCTET_STRING: u8 = 0x04;

/// Splits one TLV off `input`: `(tag, content, rest)`. Only low tag
/// numbers, which is all X.509 uses.
fn tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let len = rest[..count]
            .iter()
            .fold(0usize, |len, &b| (len << 8) | b as usize);
        (len, &rest[count..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

fn read(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (found, content, rest) = tlv(input)?;
    (found == tag).then_some((content, rest))
}

fn skip(input: &[u8]) -> Option<&[u8]> {
    tlv(input).map(|(_, _, rest)| rest)
}

/// `RDNSequence` → `C=DE, O=Example, CN=example.com`.
fn name(mut input: &[u8]) -> String {
    let mut parts = Vec::new();
    while let Some((set, rest)) = read(input, SET) {
        let mut attributes = set;
        while let Some((attribute, after)) = read(attributes, SEQUENCE) {
            if let Some((oid, value)) = read(attribute, OID) {
                let value = tlv(value)
                    .map(|(tag, content, _)| string(tag, content))
                    .unwrap_or_default();
                parts.push(format!("{}={value}", attribute_name(oid)));
            }
            attributes = after;
        }
        input = rest;
    }
    parts.join(", ")
}

fn attribute_name(oid: &[u8]) -> String {
    match oid {
        [0x55, 0x04, 0x03] => "CN".into(),
        [0x55, 0x04, 0x05] => "serialNumber".into(),
        [0x55, 0x04, 0x06] => "C".into(),
        [0x55, 0x04, 0x07] => "L".into(),
        [0x55, 0x04, 0x08] => "ST".into(),
        [0x55, 0x04, 0x09] => "street".into(),
        [0x55, 0x04, 0x0a] => "O".into(),
        [0x55, 0x04, 0x0b] => "OU".into(),
        [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x09, 0x01] => "emailAddress".into(),
        [0x09, 0x92, 0x26, 0x89, 0x93, 0xf2, 0x2c, 0x64, 0x01, 0x19] => "DC".into(),
        [0x09, 0x92, 0x26, 0x89, 0x93, 0xf2, 0x2c, 0x64, 0x01, 0x01] => "UID".into(),
        other => dotted(other),
    }
}

fn dotted(oid: &[u8]) -> String {
    let Some((&first, rest)) = oid.split_first() else {
        return String::new();
    };
    let mut parts = vec![(first / 40).to_string(), (first % 40).to_string()];
    let mut value: u64 = 0;
    for &byte in rest {
        value = (value << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            parts.push(value.to_string());
            value = 0;
        }
    }
    parts.join(".")
}

fn string(tag: u8, content: &[u8]) -> String {
    match tag {
        // BMPString: UTF-16BE.
        0x1e => {
            let units: Vec<u16> = content
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| u16::from_be_bytes(pair))
                .collect();
            String::from_utf16_lossy(&units)
        }
        // T61String and others: treat as Latin-1 unless it is UTF-8.
        _ => match std::str::from_utf8(content) {
            Ok(text) => text.to_owned(),
            Err(_) => content.iter().map(|&b| b as char).collect(),
        },
    }
}

/// The SAN extension inside `[3] EXPLICIT Extensions`.
fn subject_alt_names(explicit: &[u8]) -> Option<Vec<String>> {
    let (mut extensions, _) = read(explicit, SEQUENCE)?;
    while let Some((extension, after)) = read(extensions, SEQUENCE) {
        extensions = after;
        let (oid, mut rest) = read(extension, OID)?;
        if oid != [0x55, 0x1d, 0x11] {
            continue;
        }
        // Optional `critical BOOLEAN`.
        if rest.first() == Some(&0x01) {
            rest = skip(rest)?;
        }
        let (value, _) = read(rest, OCTET_STRING)?;
        let (mut names, _) = read(value, SEQUENCE)?;
        let mut out = Vec::new();
        while let Some((tag, content, after)) = tlv(names) {
            names = after;
            match tag {
                0x81 => out.push(format!("email:{}", string(tag, content))),
                0x82 => out.push(format!("DNS:{}", string(tag, content))),
                0x86 => out.push(format!("URI:{}", string(tag, content))),
                0x87 => out.push(format!("IP:{}", ip(content))),
                _ => {}
            }
        }
        return Some(out);
    }
    Some(Vec::new())
}

fn ip(bytes: &[u8]) -> String {
    match bytes.len() {
        4 => std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string(),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(bytes);
            std::net::Ipv6Addr::from(octets).to_string()
        }
        _ => "?".into(),
    }
}

/// UTCTime or GeneralizedTime (`…Z` only, as RFC 5280 requires).
fn parse_time(input: &[u8]) -> Option<(i64, &[u8])> {
    let (tag, content, rest) = tlv(input)?;
    let text = std::str::from_utf8(content).ok()?.strip_suffix('Z')?;
    let (year, rest_digits) = match tag {
        0x17 => {
            let yy: i64 = text.get(..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, text.get(2..)?)
        }
        0x18 => (text.get(..4)?.parse().ok()?, text.get(4..)?),
        _ => return None,
    };
    let field = |at: usize| -> Option<i64> { rest_digits.get(at..at + 2)?.parse().ok() };
    let (month, day, hour, minute) = (field(0)?, field(2)?, field(4)?, field(6)?);
    let second = field(8).unwrap_or(0);
    let days = days_from_civil(year, month, day);
    Some((days * DAY + hour * 3600 + minute * 60 + second, rest))
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-22 17:19 UTC`.
pub fn format_time(unix: i64) -> String {
    let days = unix.div_euclid(DAY);
    let secs = unix.rem_euclid(DAY);
    // Inverse of `days_from_civil`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// A public key in a form two sources can be compared in: the key of a
/// certificate and the one a private key implies (#227: does `tls.key`
/// belong to `tls.crt`). Never shown, so `Debug` prints no bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum PublicKey {
    /// Modulus and public exponent, without leading zero bytes.
    Rsa { modulus: Vec<u8>, exponent: Vec<u8> },
    /// An elliptic-curve point or an Ed25519 key, as encoded.
    Point(Vec<u8>),
}

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rsa { modulus, .. } => write!(f, "PublicKey(RSA, {} bytes)", modulus.len()),
            Self::Point(point) => write!(f, "PublicKey(point, {} bytes)", point.len()),
        }
    }
}

const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_ED25519: &[u8] = &[0x2b, 0x65, 0x70];
const INTEGER: u8 = 0x02;
const BIT_STRING: u8 = 0x03;

fn unsigned(integer: &[u8]) -> Vec<u8> {
    let start = integer
        .iter()
        .position(|b| *b != 0)
        .unwrap_or(integer.len());
    integer[start..].to_vec()
}

/// `RSAPublicKey` or the first two integers of `RSAPrivateKey` (after
/// `version` when `skip_version`).
fn rsa_key(der: &[u8], skip_version: bool) -> Option<PublicKey> {
    let (mut fields, _) = read(der, SEQUENCE)?;
    if skip_version {
        fields = skip(fields)?;
    }
    let (modulus, rest) = read(fields, INTEGER)?;
    let (exponent, _) = read(rest, INTEGER)?;
    Some(PublicKey::Rsa {
        modulus: unsigned(modulus),
        exponent: unsigned(exponent),
    })
}

/// The content of a BIT STRING without its unused-bits byte.
fn bits(content: &[u8]) -> Option<&[u8]> {
    content.split_first().map(|(_, bits)| bits)
}

/// The public key of a DER certificate.
pub fn certificate_public_key(der: &[u8]) -> Option<PublicKey> {
    let (certificate, _) = read(der, SEQUENCE)?;
    let (tbs, _) = read(certificate, SEQUENCE)?;
    let mut rest = tbs;
    if rest.first() == Some(&0xa0) {
        rest = skip(rest)?;
    }
    // Serial, signature algorithm, issuer, validity, subject.
    for _ in 0..5 {
        rest = skip(rest)?;
    }
    let (spki, _) = read(rest, SEQUENCE)?;
    let (algorithm, rest) = read(spki, SEQUENCE)?;
    let (oid, _) = read(algorithm, OID)?;
    let (key, _) = read(rest, BIT_STRING)?;
    let key = bits(key)?;
    if oid == OID_RSA {
        rsa_key(key, false)
    } else {
        Some(PublicKey::Point(key.to_vec()))
    }
}

/// SEC 1 `ECPrivateKey`: the optional `[1] publicKey`.
fn ec_private_key(der: &[u8]) -> Option<PublicKey> {
    let (fields, _) = read(der, SEQUENCE)?;
    let mut rest = skip(skip(fields)?)?;
    while let Some((tag, content, after)) = tlv(rest) {
        if tag == 0xa1 {
            let (key, _) = read(content, BIT_STRING)?;
            return Some(PublicKey::Point(bits(key)?.to_vec()));
        }
        rest = after;
    }
    None
}

/// The public key a PEM private key implies: PKCS #1 (`RSA PRIVATE KEY`),
/// SEC 1 (`EC PRIVATE KEY`) or PKCS #8 (`PRIVATE KEY`). `None` when it can't
/// be told without cryptography (an EC key without its public part, an
/// Ed25519 key without the optional public key, encrypted keys).
pub fn private_key_public_key(block: &PemBlock) -> Option<PublicKey> {
    match block.label.as_str() {
        "RSA PRIVATE KEY" => rsa_key(&block.der, true),
        "EC PRIVATE KEY" => ec_private_key(&block.der),
        "PRIVATE KEY" => {
            let (fields, _) = read(&block.der, SEQUENCE)?;
            let rest = skip(fields)?;
            let (algorithm, rest) = read(rest, SEQUENCE)?;
            let (oid, _) = read(algorithm, OID)?;
            let (private, mut rest) = read(rest, OCTET_STRING)?;
            if oid == OID_RSA {
                return rsa_key(private, true);
            }
            if oid == OID_EC {
                return ec_private_key(private);
            }
            if oid == OID_ED25519 {
                // OneAsymmetricKey v2: `[1] IMPLICIT publicKey BIT STRING`.
                while let Some((tag, content, after)) = tlv(rest) {
                    if tag == 0x81 {
                        return Some(PublicKey::Point(bits(content)?.to_vec()));
                    }
                    rest = after;
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
pub(crate) const TEST_CERT: &str = "-----BEGIN CERTIFICATE-----
MIICDDCCAbKgAwIBAgIUSTr3bXduGiNjqQJyhm1vb4ZW/tYwCgYIKoZIzj0EAwIw
OjELMAkGA1UEBhMCREUxFDASBgNVBAoMC0t1YmFjaCBUZXN0MRUwEwYDVQQDDAxl
eGFtcGxlLnRlc3QwHhcNMjYxMDIyMTcxOTE1WhcNMzYxMDE5MTcxOTE1WjA6MQsw
CQYDVQQGEwJERTEUMBIGA1UECgwLS3ViYWNoIFRlc3QxFTATBgNVBAMMDGV4YW1w
bGUudGVzdDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABFwNRo99vnYmA7HukZxH
2EsAzLumZE3MkUR9T1TfpARDJylOvw832lb5NamHjjC0UmivZvT4MV2Qyh/SASrb
kHWjgZUwgZIwHQYDVR0OBBYEFC7vkyNHEediAT6nHnagy6An7pD3MB8GA1UdIwQY
MBaAFC7vkyNHEediAT6nHnagy6An7pD3MA8GA1UdEwEB/wQFMAMBAf8wPwYDVR0R
BDgwNoIMZXhhbXBsZS50ZXN0gg4qLmV4YW1wbGUudGVzdIcECgAAAYcQAAAAAAAA
AAAAAAAAAAAAATAKBggqhkjOPQQDAgNIADBFAiAg2KdVCj06snV1t0/7ZGKbfplo
DxORXmsBBu+7ZZQiCgIhALEk5isBpBGwZOdD4+yToi3Xz+QPokJdE+NgLdqQjCfY
-----END CERTIFICATE-----
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_certificate() {
        let blocks = pem_blocks(TEST_CERT);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].label, "CERTIFICATE");
        let info = parse_certificate(&blocks[0].der).unwrap();
        assert_eq!(info.subject, "C=DE, O=Kubach Test, CN=example.test");
        assert!(info.self_signed());
        assert_eq!(
            info.san,
            [
                "DNS:example.test",
                "DNS:*.example.test",
                "IP:10.0.0.1",
                "IP:::1"
            ]
        );
        assert_eq!(format_time(info.not_before), "2026-10-22 17:19 UTC");
        assert_eq!(format_time(info.not_after), "2036-10-19 17:19 UTC");
    }

    #[test]
    fn validity_relative_to_now() {
        let info = CertificateInfo {
            subject: String::new(),
            issuer: String::new(),
            san: Vec::new(),
            not_before: 100 * DAY,
            not_after: 200 * DAY,
        };
        assert_eq!(info.validity(50 * DAY), Validity::NotYetValid);
        assert_eq!(info.validity(150 * DAY), Validity::Valid { days_left: 50 });
        assert_eq!(info.validity(210 * DAY), Validity::Expired { days_ago: 10 });
    }

    #[test]
    fn pem_skips_garbage_and_reads_keys() {
        let text = format!(
            "junk\n{TEST_CERT}\n-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n\
             -----BEGIN CERTIFICATE-----\nnot closed"
        );
        let blocks = pem_blocks(&text);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[1].label, "PRIVATE KEY");
        assert_eq!(blocks[1].der, [0, 1, 2]);
        assert!(!format!("{:?}", blocks[1]).contains("AAEC"));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_certificate(&[0x30, 0x03, 0x02, 0x01, 0x00]).is_err());
        assert!(parse_certificate(b"").is_err());
    }

    #[test]
    fn time_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(951_782_400), "2000-02-29 00:00 UTC");
        let (t, _) = parse_time(b"\x17\x0d491231235959Z").unwrap();
        assert_eq!(format_time(t), "2049-12-31 23:59 UTC");
        let (t, _) = parse_time(b"\x18\x0f19991231235959Z").unwrap();
        assert_eq!(format_time(t), "1999-12-31 23:59 UTC");
    }
}
