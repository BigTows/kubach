//! Primitives of the vault (ADR 0008): XChaCha20-Poly1305 and Argon2id.

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, Generate, Key, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Length of the data key and of password-derived keys.
pub const KEY_LEN: usize = 32;
/// Length of an XChaCha20 nonce.
pub const NONCE_LEN: usize = 24;
/// Length of the Argon2id salt.
pub const SALT_LEN: usize = 16;

/// A 256-bit key, wiped from memory when dropped.
pub type SecretKey = Zeroizing<[u8; KEY_LEN]>;

/// Argon2id cost parameters, stored in the vault header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost: u32,
    /// Passes.
    pub t_cost: u32,
    /// Lanes.
    pub p_cost: u32,
}

impl KdfParams {
    /// RFC 9106 second recommended option: 64 MiB, 3 passes, 1 lane.
    pub const DEFAULT: Self = Self {
        m_cost: 64 * 1024,
        t_cost: 3,
        p_cost: 1,
    };

    /// Fast parameters for tests only; never written by the app.
    pub const INSECURE_FAST: Self = Self {
        m_cost: 8,
        t_cost: 1,
        p_cost: 1,
    };

    /// Most memory accepted from a vault header: 1 GiB (in KiB), 16 times
    /// the default. A damaged or hostile file must not make Kubach
    /// allocate gigabytes or hash for minutes.
    pub const MAX_M_COST: u32 = 1024 * 1024;
    /// Most passes accepted from a vault header.
    pub const MAX_T_COST: u32 = 16;
    /// Most lanes accepted from a vault header.
    pub const MAX_P_COST: u32 = 16;

    /// Whether Kubach accepts these parameters: Argon2's own minimums and
    /// the maximums above. Checked before anything is allocated.
    pub fn is_supported(&self) -> bool {
        (1..=Self::MAX_P_COST).contains(&self.p_cost)
            && (1..=Self::MAX_T_COST).contains(&self.t_cost)
            && self.m_cost <= Self::MAX_M_COST
            && self.m_cost >= 8 * self.p_cost
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Encryption or key derivation failed: wrong key or tampered data. Says
/// nothing more on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CryptoError;

/// A fresh random key.
pub fn random_key() -> SecretKey {
    let key = Key::<XChaCha20Poly1305>::generate();
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    out.copy_from_slice(&key);
    out
}

/// A fresh random salt.
pub fn random_salt() -> [u8; SALT_LEN] {
    <[u8; SALT_LEN]>::generate()
}

/// Derives a key from `password` with Argon2id.
pub fn derive_key(
    password: &[u8],
    salt: &[u8],
    params: KdfParams,
) -> Result<SecretKey, CryptoError> {
    if !params.is_supported() {
        return Err(CryptoError);
    }
    let params = Params::new(params.m_cost, params.t_cost, params.p_cost, Some(KEY_LEN))
        .map_err(|_| CryptoError)?;
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, out.as_mut())
        .map_err(|_| CryptoError)?;
    Ok(out)
}

/// Encrypts `plaintext` under `key` with a fresh random nonce.
pub fn seal(key: &[u8; KEY_LEN], plaintext: &[u8], aad: &[u8]) -> (Vec<u8>, [u8; NONCE_LEN]) {
    let cipher = XChaCha20Poly1305::new(&Key::<XChaCha20Poly1305>::from(*key));
    let nonce = XNonce::generate();
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        // Only fails for messages beyond the cipher's limit (256 GiB).
        .expect("vault payload is far below the XChaCha20 limit");
    let mut nonce_bytes = [0u8; NONCE_LEN];
    nonce_bytes.copy_from_slice(&nonce);
    (ciphertext, nonce_bytes)
}

/// Decrypts and authenticates `ciphertext`.
pub fn open(
    key: &[u8; KEY_LEN],
    nonce: &[u8],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let nonce = XNonce::try_from(nonce).map_err(|_| CryptoError)?;
    let cipher = XChaCha20Poly1305::new(&Key::<XChaCha20Poly1305>::from(*key));
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| CryptoError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_open_round_trip() {
        let key = random_key();
        let (ciphertext, nonce) = seal(&key, b"secret", b"aad");
        assert_ne!(ciphertext.as_slice(), b"secret");
        assert_eq!(
            open(&key, &nonce, &ciphertext, b"aad").unwrap().as_slice(),
            b"secret"
        );
        // Wrong key, wrong AAD, flipped bit, bad nonce: all rejected.
        assert_eq!(
            open(&random_key(), &nonce, &ciphertext, b"aad"),
            Err(CryptoError)
        );
        assert_eq!(open(&key, &nonce, &ciphertext, b"other"), Err(CryptoError));
        let mut flipped = ciphertext.clone();
        flipped[0] ^= 1;
        assert_eq!(open(&key, &nonce, &flipped, b"aad"), Err(CryptoError));
        assert_eq!(
            open(&key, &nonce[1..], &ciphertext, b"aad"),
            Err(CryptoError)
        );
    }

    #[test]
    fn nonces_and_keys_are_random() {
        let key = random_key();
        let (_, a) = seal(&key, b"x", b"");
        let (_, b) = seal(&key, b"x", b"");
        assert_ne!(a, b);
        assert_ne!(*random_key(), *random_key());
        assert_ne!(random_salt(), random_salt());
    }

    #[test]
    fn derivation_depends_on_password_salt_and_params() {
        let salt = random_salt();
        let params = KdfParams::INSECURE_FAST;
        let a = derive_key(b"hunter2", &salt, params).unwrap();
        assert_eq!(*a, *derive_key(b"hunter2", &salt, params).unwrap());
        assert_ne!(*a, *derive_key(b"hunter3", &salt, params).unwrap());
        assert_ne!(*a, *derive_key(b"hunter2", &random_salt(), params).unwrap());
        let slower = KdfParams {
            t_cost: 2,
            ..params
        };
        assert_ne!(*a, *derive_key(b"hunter2", &salt, slower).unwrap());
        let invalid = KdfParams {
            m_cost: 0,
            ..params
        };
        assert_eq!(derive_key(b"x", &salt, invalid), Err(CryptoError));
    }

    #[test]
    fn parameters_are_bounded() {
        assert!(KdfParams::DEFAULT.is_supported());
        assert!(KdfParams::INSECURE_FAST.is_supported());
        let max = KdfParams {
            m_cost: KdfParams::MAX_M_COST,
            t_cost: KdfParams::MAX_T_COST,
            p_cost: KdfParams::MAX_P_COST,
        };
        assert!(max.is_supported());
        let salt = random_salt();
        for params in [
            KdfParams {
                m_cost: KdfParams::MAX_M_COST + 1,
                ..max
            },
            KdfParams {
                m_cost: u32::MAX,
                ..KdfParams::DEFAULT
            },
            KdfParams {
                t_cost: KdfParams::MAX_T_COST + 1,
                ..KdfParams::INSECURE_FAST
            },
            KdfParams {
                t_cost: u32::MAX,
                ..KdfParams::INSECURE_FAST
            },
            KdfParams {
                m_cost: 8 * (KdfParams::MAX_P_COST + 1),
                t_cost: 1,
                p_cost: KdfParams::MAX_P_COST + 1,
            },
            KdfParams {
                p_cost: 0,
                ..KdfParams::INSECURE_FAST
            },
            KdfParams {
                t_cost: 0,
                ..KdfParams::INSECURE_FAST
            },
            KdfParams {
                m_cost: 7,
                ..KdfParams::INSECURE_FAST
            },
        ] {
            assert!(!params.is_supported(), "{params:?}");
            // Rejected up front: returns at once instead of allocating.
            assert_eq!(derive_key(b"x", &salt, params), Err(CryptoError));
        }
    }
}
