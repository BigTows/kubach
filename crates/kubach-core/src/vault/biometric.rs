//! Biometric unlock (#80, ADR 0008): a key that only the OS hands out after
//! Touch ID or Windows Hello. The vault keeps its data key wrapped with this
//! key in a biometric slot; the password slot stays, so a cancelled prompt,
//! changed fingerprints or a missing sensor always fall back to it.
//!
//! - **macOS:** a random 256-bit key in the data protection Keychain with
//!   access control `biometryCurrentSet`. Reading it makes the OS ask for
//!   Touch ID; enrolling or removing a fingerprint makes the item unreadable.
//!   The data protection Keychain needs a signed build with a Keychain
//!   entitlement; an unsigned build gets `errSecMissingEntitlement`
//!   (`-34018`) and the mode is reported as unavailable.
//! - **Windows:** a Windows Hello key pair (`KeyCredentialManager`, private
//!   key in the TPM). The key is SHA-256 of the RSA signature of a fixed
//!   challenge: PKCS#1 v1.5 signatures are deterministic, and only Hello
//!   produces them.
//! - **Linux and others:** unavailable; the password is used.
//!
//! Every call may block on a system prompt: run it off the UI thread.

#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

#[cfg(any(test, feature = "test-support"))]
use super::crypto;
use super::crypto::SecretKey;
#[cfg(any(test, feature = "test-support"))]
use crate::sync::lock;

/// Prefix of the OS key names, followed by the account in the vault slot.
pub const SERVICE: &str = "kubach-vault-biometric";

/// What the OS calls its biometric unlock, for labels.
pub const METHOD_NAME: &str = if cfg!(target_os = "macos") {
    "Touch ID"
} else if cfg!(windows) {
    "Windows Hello"
} else {
    "biometrics"
};

/// Why the biometric key couldn't be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BiometricError {
    /// No sensor, nothing enrolled, an unsigned build, or an OS without
    /// support; the reason is worded for the user.
    #[error("{0}")]
    Unavailable(String),
    /// The user dismissed the system prompt.
    #[error("{METHOD_NAME} was cancelled")]
    Cancelled,
    /// The key is gone: fingerprints (or the Hello PIN/face) changed since
    /// it was made, or it was removed.
    #[error("{METHOD_NAME} changed since the vault was set up for it")]
    Invalidated,
    #[error("{METHOD_NAME}: {0}")]
    Failed(String),
}

/// A key per account that the OS gives out only after biometric
/// verification.
pub trait BiometricStore: Send + Sync {
    /// Whether keys can be made and used here. May be slow; never prompts.
    fn availability(&self) -> Result<(), BiometricError>;
    /// Makes a new key for `account` (replacing any) and returns it. May
    /// prompt (Windows Hello); Touch ID doesn't ask when adding.
    fn create(&self, account: &str) -> Result<SecretKey, BiometricError>;
    /// The key of `account`, after the OS verified the user.
    fn open(&self, account: &str) -> Result<SecretKey, BiometricError>;
    /// Removing a missing key is not an error.
    fn delete(&self, account: &str) -> Result<(), BiometricError>;
}

/// The OS biometric store. Never used by tests.
pub struct SystemBiometricStore;

impl BiometricStore for SystemBiometricStore {
    fn availability(&self) -> Result<(), BiometricError> {
        os::availability()
    }

    fn create(&self, account: &str) -> Result<SecretKey, BiometricError> {
        os::create(&name(account))
    }

    fn open(&self, account: &str) -> Result<SecretKey, BiometricError> {
        os::open(&name(account))
    }

    fn delete(&self, account: &str) -> Result<(), BiometricError> {
        os::delete(&name(account))
    }
}

/// The OS name of the key of `account`.
fn name(account: &str) -> String {
    format!("{SERVICE}-{account}")
}

#[cfg(target_os = "macos")]
mod os {
    //! Keychain item with `biometryCurrentSet` access control.

    use core_foundation::base::{CFType, TCFType as _};
    use core_foundation::string::{CFString, CFStringRef};
    use security_framework::access_control::{ProtectionMode, SecAccessControl};
    use security_framework::base::Error;
    use security_framework::passwords::{
        AccessControlOptions, PasswordOptions, delete_generic_password_options, generic_password,
        set_generic_password_options,
    };
    use zeroize::Zeroizing;

    use super::super::crypto::{self, KEY_LEN, SecretKey};
    use super::{BiometricError, SERVICE};

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        /// Text of the Touch ID prompt ("Kubach is trying to …").
        static kSecUseOperationPrompt: CFStringRef;
    }

    const ERR_USER_CANCELED: i32 = -128;
    const ERR_ITEM_NOT_FOUND: i32 = -25300;
    const ERR_AUTH_FAILED: i32 = -25293;
    const ERR_MISSING_ENTITLEMENT: i32 = -34018;
    const ERR_NOT_AVAILABLE: i32 = -25291;

    /// Where the probe item goes; removed right away.
    const PROBE: &str = "kubach-vault-biometric-probe";

    fn options(service: &str, account: &str) -> PasswordOptions {
        let mut options = PasswordOptions::new_generic_password(service, account);
        options.use_protected_keychain();
        options
    }

    fn unavailable(error: Error) -> BiometricError {
        BiometricError::Unavailable(match error.code() {
            ERR_MISSING_ENTITLEMENT => "Touch ID unlock needs a signed build of Kubach \
                                        (errSecMissingEntitlement, -34018)"
                .to_owned(),
            _ => format!("Touch ID isn't available here: {}", describe(error)),
        })
    }

    fn describe(error: Error) -> String {
        match error.message() {
            Some(message) => format!("{message} ({})", error.code()),
            None => format!("error {}", error.code()),
        }
    }

    fn store(service: &str, account: &str, secret: &[u8]) -> Result<(), Error> {
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleWhenPasscodeSetThisDeviceOnly),
            AccessControlOptions::BIOMETRY_CURRENT_SET.bits(),
        )?;
        let mut options = options(service, account);
        options.set_access_control(access);
        options.set_label("Kubach vault (Touch ID)");
        set_generic_password_options(secret, options)
    }

    fn remove(service: &str, account: &str) -> Result<(), Error> {
        match delete_generic_password_options(options(service, account)) {
            // Without the entitlement nothing can have been stored.
            Err(error) if !matches!(error.code(), ERR_ITEM_NOT_FOUND | ERR_MISSING_ENTITLEMENT) => {
                Err(error)
            }
            _ => Ok(()),
        }
    }

    /// Adds and removes a throwaway item with the same access control:
    /// fails without the entitlement (unsigned build) or without enrolled
    /// fingerprints. Adding never prompts.
    pub fn availability() -> Result<(), BiometricError> {
        let account = uuid::Uuid::new_v4().to_string();
        let result = store(PROBE, &account, b"probe").map_err(unavailable);
        remove(PROBE, &account).ok();
        result
    }

    pub fn create(name: &str) -> Result<SecretKey, BiometricError> {
        let key = crypto::random_key();
        // `set` updates an existing item instead of replacing its access
        // control; start clean.
        remove(SERVICE, name).ok();
        store(SERVICE, name, key.as_slice()).map_err(|error| match error.code() {
            ERR_MISSING_ENTITLEMENT | ERR_NOT_AVAILABLE | ERR_AUTH_FAILED => unavailable(error),
            _ => BiometricError::Failed(describe(error)),
        })?;
        Ok(key)
    }

    pub fn open(name: &str) -> Result<SecretKey, BiometricError> {
        let mut options = options(SERVICE, name);
        #[allow(deprecated)]
        options.query.push((
            // SAFETY: a constant exported by Security.framework.
            unsafe { CFString::wrap_under_get_rule(kSecUseOperationPrompt) },
            CFString::new("unlock imported clusters").as_CFType() as CFType,
        ));
        let secret = Zeroizing::new(generic_password(options).map_err(|error| {
            match error.code() {
                ERR_USER_CANCELED => BiometricError::Cancelled,
                // Items made with biometryCurrentSet vanish when the set of
                // fingerprints changes.
                ERR_ITEM_NOT_FOUND => BiometricError::Invalidated,
                ERR_MISSING_ENTITLEMENT | ERR_NOT_AVAILABLE => unavailable(error),
                _ => BiometricError::Failed(describe(error)),
            }
        })?);
        if secret.len() != KEY_LEN {
            return Err(BiometricError::Invalidated);
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&secret);
        Ok(key)
    }

    pub fn delete(name: &str) -> Result<(), BiometricError> {
        remove(SERVICE, name).map_err(|error| BiometricError::Failed(describe(error)))
    }
}

#[cfg(windows)]
mod os {
    //! Windows Hello key credential; the key is derived from a signature.

    use windows::Security::Credentials::{
        KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
    };
    use windows::Security::Cryptography::Core::{HashAlgorithmNames, HashAlgorithmProvider};
    use windows::Security::Cryptography::CryptographicBuffer;
    use windows::core::{Array, HSTRING};
    use zeroize::Zeroizing;

    use super::super::crypto::{KEY_LEN, SecretKey};
    use super::BiometricError;

    /// Signed to get the key; the same for every vault (the Hello key
    /// pair differs per account).
    const CHALLENGE: &[u8] = b"kubach-vault/1/biometric/windows-hello";

    fn failed(error: windows::core::Error) -> BiometricError {
        BiometricError::Failed(error.message())
    }

    fn status(status: KeyCredentialStatus) -> BiometricError {
        match status {
            KeyCredentialStatus::UserCanceled | KeyCredentialStatus::UserPrefersPassword => {
                BiometricError::Cancelled
            }
            KeyCredentialStatus::NotFound => BiometricError::Invalidated,
            KeyCredentialStatus::SecurityDeviceLocked => {
                BiometricError::Failed("the security device is locked; try again later".into())
            }
            other => BiometricError::Failed(format!("status {}", other.0)),
        }
    }

    pub fn availability() -> Result<(), BiometricError> {
        let supported = KeyCredentialManager::IsSupportedAsync()
            .and_then(|operation| operation.join())
            .map_err(failed)?;
        if supported {
            Ok(())
        } else {
            Err(BiometricError::Unavailable(
                "Windows Hello isn't set up on this computer".into(),
            ))
        }
    }

    fn derive(credential: &KeyCredential) -> Result<SecretKey, BiometricError> {
        let challenge = CryptographicBuffer::CreateFromByteArray(CHALLENGE).map_err(failed)?;
        let signed = credential
            .RequestSignAsync(&challenge)
            .and_then(|operation| operation.join())
            .map_err(failed)?;
        let outcome = signed.Status().map_err(failed)?;
        if outcome != KeyCredentialStatus::Success {
            return Err(status(outcome));
        }
        let signature = signed.Result().map_err(failed)?;
        let digest =
            HashAlgorithmProvider::OpenAlgorithm(&HashAlgorithmNames::Sha256().map_err(failed)?)
                .and_then(|provider| provider.HashData(&signature))
                .map_err(failed)?;
        let mut bytes = Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(&digest, &mut bytes).map_err(failed)?;
        let bytes = Zeroizing::new(bytes.to_vec());
        if bytes.len() != KEY_LEN {
            return Err(BiometricError::Failed("unexpected digest length".into()));
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&bytes);
        Ok(key)
    }

    pub fn create(name: &str) -> Result<SecretKey, BiometricError> {
        let created = KeyCredentialManager::RequestCreateAsync(
            &HSTRING::from(name),
            KeyCredentialCreationOption::ReplaceExisting,
        )
        .and_then(|operation| operation.join())
        .map_err(failed)?;
        let outcome = created.Status().map_err(failed)?;
        if outcome != KeyCredentialStatus::Success {
            return Err(status(outcome));
        }
        derive(&created.Credential().map_err(failed)?)
    }

    pub fn open(name: &str) -> Result<SecretKey, BiometricError> {
        let opened = KeyCredentialManager::OpenAsync(&HSTRING::from(name))
            .and_then(|operation| operation.join())
            .map_err(failed)?;
        let outcome = opened.Status().map_err(failed)?;
        if outcome != KeyCredentialStatus::Success {
            return Err(status(outcome));
        }
        derive(&opened.Credential().map_err(failed)?)
    }

    pub fn delete(name: &str) -> Result<(), BiometricError> {
        // Deleting a missing credential fails; that is fine.
        KeyCredentialManager::DeleteAsync(&HSTRING::from(name))
            .and_then(|operation| operation.join())
            .ok();
        Ok(())
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod os {
    use super::super::crypto::SecretKey;
    use super::BiometricError;

    fn unavailable() -> BiometricError {
        BiometricError::Unavailable("Biometric unlock isn't supported on this system".into())
    }

    pub fn availability() -> Result<(), BiometricError> {
        Err(unavailable())
    }

    pub fn create(_: &str) -> Result<SecretKey, BiometricError> {
        Err(unavailable())
    }

    pub fn open(_: &str) -> Result<SecretKey, BiometricError> {
        Err(unavailable())
    }

    pub fn delete(_: &str) -> Result<(), BiometricError> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
/// In-memory store for tests: keys by account, and an outcome forced on
/// [`open`](BiometricStore::open) (a cancelled prompt, changed fingerprints…).
#[derive(Default)]
pub struct FakeBiometricStore {
    keys: Mutex<HashMap<String, SecretKey>>,
    /// Every call fails with this.
    unavailable: Option<String>,
    /// [`open`](BiometricStore::open) fails with this.
    open_error: Mutex<Option<BiometricError>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeBiometricStore {
    /// A store whose every call reports the mode unavailable.
    pub fn unavailable(reason: &str) -> Self {
        Self {
            unavailable: Some(reason.to_owned()),
            ..Self::default()
        }
    }

    /// Makes the next opens fail with `error`; `None` restores them.
    pub fn fail_open(&self, error: Option<BiometricError>) {
        *lock(&self.open_error) = error;
    }

    /// Forgets every key, as the OS does when fingerprints change.
    pub fn invalidate(&self) {
        lock(&self.keys).clear();
    }

    pub fn len(&self) -> usize {
        lock(&self.keys).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn check(&self) -> Result<(), BiometricError> {
        match &self.unavailable {
            Some(reason) => Err(BiometricError::Unavailable(reason.clone())),
            None => Ok(()),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl BiometricStore for FakeBiometricStore {
    fn availability(&self) -> Result<(), BiometricError> {
        self.check()
    }

    fn create(&self, account: &str) -> Result<SecretKey, BiometricError> {
        self.check()?;
        let key = crypto::random_key();
        lock(&self.keys).insert(account.to_owned(), key.clone());
        Ok(key)
    }

    fn open(&self, account: &str) -> Result<SecretKey, BiometricError> {
        self.check()?;
        if let Some(error) = lock(&self.open_error).clone() {
            return Err(error);
        }
        lock(&self.keys)
            .get(account)
            .cloned()
            .ok_or(BiometricError::Invalidated)
    }

    fn delete(&self, account: &str) -> Result<(), BiometricError> {
        self.check()?;
        lock(&self.keys).remove(account);
        Ok(())
    }
}
