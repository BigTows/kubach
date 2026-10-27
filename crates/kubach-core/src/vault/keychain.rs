//! Where the data key of a keychain slot lives: the OS credential store
//! (Keychain, Credential Manager, Secret Service) through `keyring`, or an
//! in-memory store in tests.

#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

use zeroize::Zeroizing;

#[cfg(any(test, feature = "test-support"))]
use crate::sync::lock;

/// Service name of vault keys in the OS credential store.
pub const SERVICE: &str = "kubach-vault";

/// A credential store failure, already worded for the user.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct KeyStoreError(pub String);

/// Secrets by account name, under [`SERVICE`].
pub trait KeyStore: Send + Sync {
    /// `Ok(None)` when there is no secret for `account`.
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError>;
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), KeyStoreError>;
    /// Removing a missing secret is not an error.
    fn delete(&self, account: &str) -> Result<(), KeyStoreError>;
}

/// The OS credential store. Never used by tests.
pub struct SystemKeyStore;

impl SystemKeyStore {
    /// Whether the store can be used; on Linux the Secret Service (GNOME
    /// Keyring, KWallet) may be missing. Checked once, never panics.
    pub fn availability() -> Result<(), KeyStoreError> {
        keyring::Entry::store_status()
            .as_ref()
            .map(|_| ())
            .map_err(|error| {
                KeyStoreError(format!("The system keychain is not available: {error}"))
            })
    }

    fn entry(account: &str) -> Result<keyring::Entry, KeyStoreError> {
        keyring::Entry::new(SERVICE, account).map_err(store_error)
    }
}

fn store_error(error: keyring::Error) -> KeyStoreError {
    KeyStoreError(format!("System keychain: {error}"))
}

impl KeyStore for SystemKeyStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        match Self::entry(account)?.get_secret() {
            Ok(secret) => Ok(Some(Zeroizing::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), KeyStoreError> {
        Self::entry(account)?
            .set_secret(secret)
            .map_err(store_error)
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(store_error(error)),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryKeyStore {
    secrets: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
    /// When set, every call fails with this message.
    failing: Option<String>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryKeyStore {
    /// A store whose every call fails with `message`.
    pub fn failing(message: &str) -> Self {
        Self {
            failing: Some(message.to_owned()),
            ..Self::default()
        }
    }

    fn check(&self) -> Result<(), KeyStoreError> {
        match &self.failing {
            Some(message) => Err(KeyStoreError(message.clone())),
            None => Ok(()),
        }
    }

    pub fn len(&self) -> usize {
        lock(&self.secrets).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(any(test, feature = "test-support"))]
impl KeyStore for MemoryKeyStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        self.check()?;
        Ok(lock(&self.secrets).get(account).cloned())
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), KeyStoreError> {
        self.check()?;
        lock(&self.secrets).insert(account.to_owned(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), KeyStoreError> {
        self.check()?;
        lock(&self.secrets).remove(account);
        Ok(())
    }
}
