//! Detached OpenPGP signatures of tool releases (#403).
//!
//! A [`Keyring`] holds the public keys Kubach trusts for a tool, checked
//! into the source; [`Keyring::verify`] checks an armored detached
//! signature (`.asc`) over some data and names the key that made it. Keys
//! are only those of the keyring: nothing is fetched from key servers.

use std::fmt;
use std::io::Read;

use pgp::composed::{Deserializable as _, DetachedSignature, SignedPublicKey};
use pgp::packet::{Signature, SignatureType};
use pgp::types::{KeyDetails as _, SignedUser, Tag, Timestamp};
use serde::{Deserialize, Serialize};

const KEY_BEGIN: &str = "-----BEGIN PGP PUBLIC KEY BLOCK-----";
const KEY_END: &str = "-----END PGP PUBLIC KEY BLOCK-----";
/// Armored signatures larger than this are refused (a real one is ~900 B).
pub const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignatureError {
    #[error("the signature can't be read: {0}")]
    Malformed(String),
    #[error("signed by key {issuer}, which Kubach does not trust")]
    UnknownKey { issuer: String },
    #[error("the signature by {signer} does not match the file: {reason}")]
    Invalid { signer: String, reason: String },
    #[error("the signature by {signer} was made after the key expired")]
    Expired { signer: String },
    #[error("the key {signer} is revoked")]
    Revoked { signer: String },
}

/// Who made a valid signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signer {
    /// Primary user ID of the key, e.g. `Scott Rigby <scott@r6by.com>`.
    pub user_id: String,
    /// Fingerprint of the primary key, uppercase hex without spaces.
    pub fingerprint: String,
}

impl Signer {
    /// The fingerprint in groups of four, as gpg and release notes print it.
    pub fn grouped_fingerprint(&self) -> String {
        group_fingerprint(&self.fingerprint)
    }
}

impl fmt::Display for Signer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.user_id, self.grouped_fingerprint())
    }
}

/// `208DD36E…` → `208D D36E …`.
pub fn group_fingerprint(hex: &str) -> String {
    hex.as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Trusted public keys.
#[derive(Debug, Default)]
pub struct Keyring {
    keys: Vec<SignedPublicKey>,
}

impl Keyring {
    /// Every armored public key block in `text`; text between the blocks
    /// (the `pub`/`uid` listings of a `KEYS` file) is ignored, and so is a
    /// block that does not parse or has no valid self-signature on a user
    /// ID. Certifications by other keys are not needed and not checked.
    pub fn from_armored(text: &str) -> Self {
        let mut keys = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find(KEY_BEGIN) {
            let Some(len) = rest[start..].find(KEY_END) else {
                break;
            };
            let end = start + len + KEY_END.len();
            if let Ok((key, _)) = SignedPublicKey::from_string(&rest[start..end])
                && self_signatures(&key).next().is_some()
            {
                keys.push(key);
            }
            rest = &rest[end..];
        }
        Self { keys }
    }

    /// Primary key fingerprints, uppercase hex, in keyring order.
    pub fn fingerprints(&self) -> Vec<String> {
        self.keys.iter().map(fingerprint).collect()
    }

    /// Checks the detached `signature` (armored `.asc`, or binary `.sig`
    /// as the AWS CLI publishes) over `data` and returns the
    /// signer. The issuer must be a primary key of the keyring or one of
    /// its signing subkeys with a valid binding; the key must not be
    /// revoked or expired at the signature's creation time.
    pub fn verify(&self, signature: &[u8], data: impl Read) -> Result<Signer, SignatureError> {
        let signature = parse_detached(signature)?;
        if !matches!(
            signature.typ(),
            Some(SignatureType::Binary | SignatureType::Text)
        ) {
            return Err(SignatureError::Malformed(
                "not a signature over a file".into(),
            ));
        }
        let created = signature
            .created()
            .ok_or_else(|| SignatureError::Malformed("no creation time".into()))?;
        let Some((key, component)) = self.find_issuer(&signature) else {
            return Err(SignatureError::UnknownKey {
                issuer: issuer(&signature),
            });
        };
        let signer = Signer {
            user_id: user_id(key),
            fingerprint: fingerprint(key),
        };
        let invalid = |reason: String| SignatureError::Invalid {
            signer: signer.to_string(),
            reason,
        };
        match component {
            Component::Primary => signature.verify(&key.primary_key, data),
            Component::Subkey(ix) => signature.verify(&key.public_subkeys[ix], data),
        }
        .map_err(|e| invalid(e.to_string()))?;
        if revoked(key) {
            return Err(SignatureError::Revoked {
                signer: signer.to_string(),
            });
        }
        let expires = match component {
            Component::Primary => primary_expiry(key),
            Component::Subkey(ix) => primary_expiry(key)
                .into_iter()
                .chain(subkey_expiry(key, ix))
                .min(),
        };
        if expires.is_some_and(|expires| created.as_secs() >= expires) {
            return Err(SignatureError::Expired {
                signer: signer.to_string(),
            });
        }
        Ok(signer)
    }

    /// The key and its component that made `signature`, by the issuer
    /// fingerprint, else the issuer key ID.
    fn find_issuer(&self, signature: &Signature) -> Option<(&SignedPublicKey, Component)> {
        let fingerprints = signature.issuer_fingerprint();
        let key_ids = signature.issuer_key_id();
        let matches = |fp: pgp::types::Fingerprint, id: pgp::types::KeyId| {
            if fingerprints.is_empty() {
                key_ids.contains(&&id)
            } else {
                fingerprints.contains(&&fp)
            }
        };
        for key in &self.keys {
            if matches(
                key.primary_key.fingerprint(),
                key.primary_key.legacy_key_id(),
            ) {
                return Some((key, Component::Primary));
            }
            for (ix, subkey) in key.public_subkeys.iter().enumerate() {
                let signs = subkey.signatures.iter().any(|sig| sig.key_flags().sign());
                if signs
                    && matches(subkey.key.fingerprint(), subkey.key.legacy_key_id())
                    && subkey.verify_bindings(&key.primary_key).is_ok()
                {
                    return Some((key, Component::Subkey(ix)));
                }
            }
        }
        None
    }
}

/// An armored or binary detached signature.
fn parse_detached(bytes: &[u8]) -> Result<Signature, SignatureError> {
    let malformed = |e: pgp::errors::Error| SignatureError::Malformed(e.to_string());
    let armored = std::str::from_utf8(bytes)
        .ok()
        .filter(|text| text.trim_start().starts_with("-----BEGIN PGP"));
    let detached = match armored {
        Some(text) => DetachedSignature::from_string(text).map_err(malformed)?.0,
        None => DetachedSignature::from_bytes(bytes).map_err(malformed)?,
    };
    Ok(detached.signature)
}

#[derive(Clone, Copy, Debug)]
enum Component {
    Primary,
    Subkey(usize),
}

fn fingerprint(key: &SignedPublicKey) -> String {
    format!("{:X}", key.primary_key.fingerprint())
}

/// The primary user ID (self-signed and flagged primary, else the first
/// self-signed one).
fn user_id(key: &SignedPublicKey) -> String {
    let signed: Vec<_> = self_signatures(key).collect();
    signed
        .iter()
        .find(|(_, sig)| sig.is_primary())
        .or_else(|| signed.first())
        .map(|(user, _)| String::from_utf8_lossy(user.id.id()).into_owned())
        .unwrap_or_else(|| group_fingerprint(&fingerprint(key)))
}

/// The user IDs of `key` with each valid self-signature on them (by the
/// primary key itself; third-party certifications are skipped).
fn self_signatures(key: &SignedPublicKey) -> impl Iterator<Item = (&SignedUser, &Signature)> {
    let primary = &key.primary_key;
    key.details.users.iter().flat_map(move |user| {
        user.signatures
            .iter()
            .filter(move |sig| {
                sig.verify_certification(primary, Tag::UserId, &user.id)
                    .is_ok()
            })
            .map(move |sig| (user, sig))
    })
}

fn issuer(signature: &Signature) -> String {
    if let Some(fp) = signature.issuer_fingerprint().first() {
        return group_fingerprint(&format!("{fp:X}"));
    }
    signature.issuer_key_id().first().map_or_else(
        || "unknown".to_owned(),
        |id| id.as_ref().iter().map(|b| format!("{b:02X}")).collect(),
    )
}

/// A verified key revocation signature on the primary key.
fn revoked(key: &SignedPublicKey) -> bool {
    key.details
        .revocation_signatures
        .iter()
        .any(|sig| sig.verify_key(&key.primary_key).is_ok())
}

/// When the primary key expires (Unix seconds), from the newest verified
/// self-signature of a user ID or of the key itself; `None` never.
fn primary_expiry(key: &SignedPublicKey) -> Option<u32> {
    let primary = &key.primary_key;
    let user_sigs = self_signatures(key).map(|(_, sig)| sig);
    let direct = key
        .details
        .direct_signatures
        .iter()
        .filter(|sig| sig.verify_key(primary).is_ok());
    let newest = user_sigs
        .chain(direct)
        .max_by_key(|sig| sig.created().map(Timestamp::as_secs))?;
    expiry(primary.created_at(), newest)
}

/// When subkey `ix` expires, from its newest binding signature.
fn subkey_expiry(key: &SignedPublicKey, ix: usize) -> Option<u32> {
    let subkey = &key.public_subkeys[ix];
    let newest = subkey
        .signatures
        .iter()
        .max_by_key(|sig| sig.created().map(Timestamp::as_secs))?;
    expiry(subkey.key.created_at(), newest)
}

fn expiry(created: Timestamp, signature: &Signature) -> Option<u32> {
    let after = signature.key_expiration_time()?.as_secs();
    (after > 0).then(|| created.as_secs().saturating_add(after))
}

#[cfg(test)]
pub(crate) mod test_keys {
    //! A key generated for tests and signatures made with it.

    use pgp::composed::{
        DetachedSignature, EncryptionCaps, KeyType, SecretKeyParamsBuilder, SignedPublicKey,
        SignedSecretKey,
    };
    use pgp::crypto::hash::HashAlgorithm;
    use pgp::types::Password;
    use rand_chacha::ChaCha8Rng;
    use rand_chacha::rand_core::SeedableRng as _;

    /// An Ed25519 signing key for `user`, from a fixed seed.
    pub fn secret_key(user: &str, seed: u64) -> SignedSecretKey {
        let mut params = SecretKeyParamsBuilder::default();
        params
            .key_type(KeyType::Ed25519)
            .can_certify(true)
            .can_sign(true)
            .can_encrypt(EncryptionCaps::None)
            .primary_user_id(user.into());
        params
            .build()
            .unwrap()
            .generate(ChaCha8Rng::seed_from_u64(seed))
            .unwrap()
    }

    /// The public key of `key`, armored.
    pub fn armored_public(key: &SignedSecretKey) -> String {
        SignedPublicKey::from(key.clone())
            .to_armored_string(Default::default())
            .unwrap()
    }

    /// An armored detached signature of `data` by `key`.
    pub fn sign(key: &SignedSecretKey, data: &[u8]) -> Vec<u8> {
        DetachedSignature::sign_binary_data(
            ChaCha8Rng::seed_from_u64(7),
            &key.primary_key,
            &Password::empty(),
            HashAlgorithm::Sha256,
            data,
        )
        .unwrap()
        .to_armored_bytes(Default::default())
        .unwrap()
    }

    /// A binary detached signature (`.sig`, as the AWS CLI publishes).
    pub fn sign_binary(key: &SignedSecretKey, data: &[u8]) -> Vec<u8> {
        use pgp::ser::Serialize as _;
        DetachedSignature::sign_binary_data(
            ChaCha8Rng::seed_from_u64(7),
            &key.primary_key,
            &Password::empty(),
            HashAlgorithm::Sha256,
            data,
        )
        .unwrap()
        .to_bytes()
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_keys::{armored_public, secret_key, sign, sign_binary};
    use super::*;

    #[test]
    fn verifies_binary_signatures() {
        let key = secret_key("AWS-like <cli@example.com>", 9);
        let keyring = Keyring::from_armored(&armored_public(&key));
        let data = b"awscli-exe-linux-x86_64.zip contents";
        let signature = sign_binary(&key, data);
        assert!(!signature.starts_with(b"-----BEGIN"));
        let signer = keyring.verify(&signature, &data[..]).unwrap();
        assert_eq!(signer.user_id, "AWS-like <cli@example.com>");
        assert!(matches!(
            keyring.verify(&signature, &b"tampered"[..]),
            Err(SignatureError::Invalid { .. })
        ));
    }

    #[test]
    fn verifies_and_names_the_signer() {
        let key = secret_key("Test Maintainer <test@example.com>", 1);
        let keyring = Keyring::from_armored(&format!(
            "pub   ed25519 [SC]\nuid Test\n\n{}\ntrailing text\n",
            armored_public(&key)
        ));
        assert_eq!(keyring.fingerprints().len(), 1);
        let data = b"6b4e61c8  helm-v4.3.1-darwin-arm64.tar.gz\n";
        let signer = keyring.verify(&sign(&key, data), &data[..]).unwrap();
        assert_eq!(signer.user_id, "Test Maintainer <test@example.com>");
        assert_eq!(signer.fingerprint, keyring.fingerprints()[0]);
        assert_eq!(signer.fingerprint.len(), 40);
        assert!(
            signer
                .to_string()
                .starts_with("Test Maintainer <test@example.com> (")
        );
    }

    #[test]
    fn tampered_data_is_invalid() {
        let key = secret_key("A <a@example.com>", 2);
        let keyring = Keyring::from_armored(&armored_public(&key));
        let signature = sign(&key, b"archive bytes");
        assert!(matches!(
            keyring.verify(&signature, &b"archive bytez"[..]),
            Err(SignatureError::Invalid { .. })
        ));
    }

    #[test]
    fn unknown_key_is_refused() {
        let trusted = secret_key("Trusted <t@example.com>", 3);
        let stranger = secret_key("Stranger <s@example.com>", 4);
        let keyring = Keyring::from_armored(&armored_public(&trusted));
        let error = keyring
            .verify(&sign(&stranger, b"x"), &b"x"[..])
            .unwrap_err();
        assert!(
            matches!(error, SignatureError::UnknownKey { .. }),
            "{error}"
        );
        // An empty keyring trusts nothing.
        assert!(matches!(
            Keyring::default().verify(&sign(&trusted, b"x"), &b"x"[..]),
            Err(SignatureError::UnknownKey { .. })
        ));
    }

    #[test]
    fn garbage_is_malformed() {
        let keyring = Keyring::default();
        for bad in [&b"not a signature"[..], b"<html>404</html>", &[0xff, 0xfe]] {
            assert!(matches!(
                keyring.verify(bad, &b"x"[..]),
                Err(SignatureError::Malformed(_))
            ));
        }
    }

    /// Expiry comes from the newest self-signature: Taylor Thomas's key in
    /// Helm's `KEYS` expired on 2022-08-06; Scott Rigby's never expires.
    #[test]
    fn key_expiry_from_self_signatures() {
        let keyring = Keyring::from_armored(include_str!("helm-KEYS"));
        let key = |fp: &str| {
            keyring
                .keys
                .iter()
                .find(|key| fingerprint(key) == fp)
                .unwrap()
        };
        let taylor = primary_expiry(key("76939899B137D575D3274E756DCCB9D752D35BA8")).unwrap();
        // 2022-08-06 in UTC, give or take the time of day.
        assert!(
            (1_659_744_000..1_659_744_000 + 86_400).contains(&taylor),
            "{taylor}"
        );
        assert_eq!(
            primary_expiry(key("208DD36ED5BB3745A16743A4C7C6FBB5B91C1155")),
            None
        );
        assert!(!revoked(key("208DD36ED5BB3745A16743A4C7C6FBB5B91C1155")));
    }

    #[test]
    fn groups_fingerprints() {
        assert_eq!(
            group_fingerprint("208DD36ED5BB3745A16743A4C7C6FBB5B91C1155"),
            "208D D36E D5BB 3745 A167 43A4 C7C6 FBB5 B91C 1155"
        );
    }
}
