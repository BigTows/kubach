//! Encrypted store of imported kubeconfigs (`vault.kubach`), see ADR 0008.
//!
//! The file holds a JSON envelope: the names of the imported entries and
//! their contexts in the clear (so they are listed while locked), key slots,
//! and the configs themselves encrypted with a random data key. A password
//! slot wraps the data key with an Argon2id-derived key; a keychain slot
//! keeps it in the OS credential store; a biometric slot wraps it with a
//! key the OS releases after Touch ID or Windows Hello ([`biometric`]).
//!
//! [`LockedVault`] is the file as read; unlocking gives a [`Vault`] whose
//! data key and decrypted configs live in memory only and are wiped when it
//! is dropped.

pub mod biometric;
pub mod crypto;
pub mod import;
pub mod keychain;
pub mod manual;
pub mod screen_lock;

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kubach_i18n::{Message, msg};
use kube::config::Kubeconfig;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use self::biometric::{BiometricError, BiometricStore};
use self::crypto::{KEY_LEN, KdfParams, SecretKey};
pub use self::import::{ImportPlan, NameProblem, PlannedContext};
use self::keychain::{KeyStore, KeyStoreError};
use crate::kubeconfig::{FileProblem, Source};

/// File name of the vault in the config directory.
pub const VAULT_FILE: &str = "vault.kubach";

const FORMAT: &str = "kubach-vault";
const VERSION: u32 = 1;
const PASSWORD_AAD: &[u8] = b"kubach-vault/1/password";
const KEYCHAIN_AAD: &[u8] = b"kubach-vault/1/keychain";
const BIOMETRIC_AAD: &[u8] = b"kubach-vault/1/biometric";

/// `vault.kubach` in [`config_dir`](crate::settings::config_dir), which
/// honours `KUBACH_CONFIG_DIR`.
pub fn default_path() -> Option<PathBuf> {
    crate::settings::config_dir().map(|dir| dir.join(VAULT_FILE))
}

/// Context names of the vault at `path`, from its public header only:
/// nothing is decrypted, no key is asked for and nothing is written (a
/// damaged file gives no names instead of a backup). For processes that
/// must never hold the vault's secrets, such as `kubach mcp`, to tell an
/// imported context from an unknown one. The header is authenticated only
/// on unlock, so treat the names as a hint.
pub fn context_names(path: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct Header {
        format: String,
        entries: Vec<EntryInfo>,
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Header>(&bytes).ok())
        .filter(|header| header.format == FORMAT)
        .map(|header| {
            header
                .entries
                .into_iter()
                .flat_map(|entry| entry.contexts)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Wrong password")]
    WrongPassword,
    #[error("The vault file is damaged ({reason}).{}", backup_note(.backup))]
    Corrupted {
        reason: String,
        backup: Option<PathBuf>,
    },
    #[error("This vault was written by a newer Kubach (format {0}); update Kubach to open it")]
    UnsupportedVersion(u32),
    #[error("This vault has no password; unlock it with the system keychain")]
    NoPassword,
    #[error("This vault is not stored in the system keychain")]
    NoKeychain,
    #[error("The vault key is not in the system keychain; unlock with the password")]
    KeychainMissing,
    #[error("The key in the system keychain doesn't open this vault; unlock with the password")]
    KeychainMismatch,
    #[error("A vault needs a password, the system keychain or both")]
    NoKey,
    #[error("{} unlock isn't turned on for this vault", biometric::METHOD_NAME)]
    NoBiometric,
    #[error(
        "{} unlock needs a vault password to fall back on; set one first",
        biometric::METHOD_NAME
    )]
    BiometricNeedsPassword,
    #[error(
        "The {} key doesn't open this vault; unlock with the password",
        biometric::METHOD_NAME
    )]
    BiometricMismatch,
    #[error("{}", biometric_message(.0))]
    Biometric(#[from] BiometricError),
    #[error("The password is empty")]
    EmptyPassword,
    #[error("A vault already exists at {}", .0.display())]
    Exists(PathBuf),
    #[error("No imported config with id {0}")]
    UnknownEntry(String),
    #[error("Can't store the config: {0}")]
    Serialize(String),
    #[error("Context names must be non-empty, unique and not used by other contexts")]
    ContextNames,
    #[error("Confirm reading the local files the config refers to first")]
    UnreadFiles,
    #[error(transparent)]
    KeyStore(#[from] KeyStoreError),
    #[error("Can't access the vault file: {0}")]
    Io(#[from] io::Error),
}

impl VaultError {
    /// The error for the screen; [`Display`](fmt::Display) is the English
    /// text for logs. Reasons worded by the OS or a library stay as they
    /// are.
    pub fn message(&self) -> Message {
        let method = biometric::METHOD_NAME;
        match self {
            VaultError::WrongPassword => msg!("vault-error-wrong-password"),
            VaultError::Corrupted { reason, backup } => match backup {
                Some(path) => msg!(
                    "vault-error-corrupted-backup",
                    reason = reason.as_str(),
                    path = path.display().to_string()
                ),
                None => msg!("vault-error-corrupted", reason = reason.as_str()),
            },
            VaultError::UnsupportedVersion(version) => {
                msg!("vault-error-newer-format", version = *version)
            }
            VaultError::NoPassword => msg!("vault-error-no-password"),
            VaultError::NoKeychain => msg!("vault-error-no-keychain"),
            VaultError::KeychainMissing => msg!("vault-error-keychain-missing"),
            VaultError::KeychainMismatch => msg!("vault-error-keychain-mismatch"),
            VaultError::NoKey => msg!("vault-error-no-key"),
            VaultError::NoBiometric => msg!("vault-error-no-biometric", method = method),
            VaultError::BiometricNeedsPassword => {
                msg!("vault-error-biometric-needs-password", method = method)
            }
            VaultError::BiometricMismatch => {
                msg!("vault-error-biometric-mismatch", method = method)
            }
            VaultError::Biometric(error) => match error {
                BiometricError::Cancelled => {
                    msg!("vault-error-biometric-cancelled", method = method)
                }
                BiometricError::Invalidated => {
                    msg!("vault-error-biometric-invalidated", method = method)
                }
                BiometricError::Unavailable(reason) => {
                    msg!("vault-error-text", text = reason.as_str())
                }
                BiometricError::Failed(reason) => {
                    msg!(
                        "vault-error-biometric-failed",
                        method = method,
                        reason = reason.as_str()
                    )
                }
            },
            VaultError::EmptyPassword => msg!("vault-error-empty-password"),
            VaultError::Exists(path) => {
                msg!("vault-error-exists", path = path.display().to_string())
            }
            VaultError::UnknownEntry(id) => msg!("vault-error-unknown-entry", id = id.as_str()),
            VaultError::Serialize(error) => msg!("vault-error-serialize", error = error.as_str()),
            VaultError::ContextNames => msg!("vault-error-context-names"),
            VaultError::UnreadFiles => msg!("vault-error-unread-files"),
            VaultError::KeyStore(error) => msg!("vault-error-text", text = error.0.as_str()),
            VaultError::Io(error) => msg!("vault-error-io", error = error.to_string()),
        }
    }
}

fn biometric_message(error: &BiometricError) -> String {
    let name = biometric::METHOD_NAME;
    match error {
        BiometricError::Cancelled => format!("{name} was cancelled"),
        BiometricError::Invalidated => format!(
            "{name} changed since the vault was set up for it (fingerprints added or \
             removed), so its key no longer works. Enter the password, then turn {name} \
             unlock on again in Settings."
        ),
        BiometricError::Unavailable(reason) => reason.clone(),
        BiometricError::Failed(reason) => format!("{name} failed: {reason}"),
    }
}

impl VaultError {
    /// Whether the password form should take over: an automatic unlock
    /// (biometrics, keychain) failed or was cancelled, but the vault is fine.
    pub fn falls_back_to_password(&self) -> bool {
        matches!(
            self,
            Self::Biometric(_)
                | Self::BiometricMismatch
                | Self::NoBiometric
                | Self::KeyStore(_)
                | Self::KeychainMissing
                | Self::KeychainMismatch
                | Self::NoKeychain
        )
    }

    /// Whether the user dismissed the system prompt on purpose.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Biometric(BiometricError::Cancelled))
    }
}

fn backup_note(backup: &Option<PathBuf>) -> String {
    match backup {
        Some(path) => format!(" A copy was saved to {}.", path.display()),
        None => String::new(),
    }
}

/// An imported kubeconfig as listed without unlocking.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryInfo {
    pub id: String,
    /// Display name, e.g. the file name it was imported from.
    pub name: String,
    /// Context names, in file order.
    pub contexts: Vec<String>,
}

/// How the vault can be unlocked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyMode {
    pub password: bool,
    pub keychain: bool,
    /// Touch ID / Windows Hello.
    pub biometric: bool,
}

/// A way to unlock the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnlockMethod {
    Biometric,
    Keychain,
    Password,
}

/// Which OS stores can be used right now (checked in the background).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Availability {
    pub biometric: bool,
    pub keychain: bool,
}

impl KeyMode {
    /// What to try without typing anything, in order: biometrics first (the
    /// user expects the prompt), then the keychain. Empty when only the
    /// password opens the vault here.
    pub fn automatic(&self, available: Availability) -> Vec<UnlockMethod> {
        let mut methods = Vec::new();
        if self.biometric && available.biometric {
            methods.push(UnlockMethod::Biometric);
        }
        if self.keychain && available.keychain {
            methods.push(UnlockMethod::Keychain);
        }
        methods
    }

    /// The first way to try: [`automatic`](Self::automatic) or the password.
    pub fn first(&self, available: Availability) -> UnlockMethod {
        self.automatic(available)
            .first()
            .copied()
            .unwrap_or(UnlockMethod::Password)
    }
}

/// A password stretched with Argon2id, ready to protect a vault. Deriving
/// is slow on purpose: do it off the UI thread.
pub struct DerivedPassword {
    kdf: Kdf,
    kek: SecretKey,
}

impl DerivedPassword {
    pub fn derive(password: &str, params: KdfParams) -> Result<Self, VaultError> {
        if password.is_empty() {
            return Err(VaultError::EmptyPassword);
        }
        let salt = crypto::random_salt();
        let kek = crypto::derive_key(password.as_bytes(), &salt, params)
            .map_err(|_| VaultError::Serialize("invalid key derivation parameters".into()))?;
        Ok(Self {
            kdf: Kdf {
                alg: "argon2id".into(),
                params,
                salt: salt.to_vec(),
            },
            kek,
        })
    }

    fn slot(&self, data_key: &[u8; KEY_LEN]) -> Slot {
        let (wrapped_key, nonce) = crypto::seal(&self.kek, data_key, PASSWORD_AAD);
        Slot::Password {
            kdf: self.kdf.clone(),
            nonce: nonce.to_vec(),
            wrapped_key,
        }
    }
}

/// Imported config as a source of contexts; see
/// [`Contexts::load_all`](crate::Contexts::load_all).
#[derive(Clone, Debug)]
pub struct Imported {
    pub source: Source,
    /// The config, or [`FileProblem::Locked`] while the vault is locked.
    pub config: Result<Kubeconfig, FileProblem>,
    /// Context names, known even while locked.
    pub contexts: Vec<String>,
}

mod b64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Kdf {
    alg: String,
    #[serde(flatten)]
    params: KdfParams,
    #[serde(with = "b64")]
    salt: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum Slot {
    Password {
        kdf: Kdf,
        #[serde(with = "b64")]
        nonce: Vec<u8>,
        #[serde(with = "b64")]
        wrapped_key: Vec<u8>,
    },
    Keychain {
        account: String,
        /// AEAD tag over nothing, proving the key in the keychain is this
        /// vault's.
        #[serde(with = "b64")]
        nonce: Vec<u8>,
        #[serde(with = "b64")]
        check: Vec<u8>,
    },
    /// The data key wrapped with a key the OS releases after Touch ID or
    /// Windows Hello ([`biometric`]).
    Biometric {
        account: String,
        #[serde(with = "b64")]
        nonce: Vec<u8>,
        #[serde(with = "b64")]
        wrapped_key: Vec<u8>,
    },
}

/// The file as stored.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    entries: Vec<EntryInfo>,
    slots: Vec<Slot>,
    #[serde(with = "b64")]
    nonce: Vec<u8>,
    #[serde(with = "b64")]
    ciphertext: Vec<u8>,
}

/// The authenticated public part of the envelope.
#[derive(Serialize)]
struct PublicHeader<'a> {
    format: &'a str,
    version: u32,
    entries: &'a [EntryInfo],
}

fn header_aad(entries: &[EntryInfo]) -> Vec<u8> {
    serde_json::to_vec(&PublicHeader {
        format: FORMAT,
        version: VERSION,
        entries,
    })
    .expect("header serializes")
}

/// The encrypted part.
#[derive(Clone, Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct Secrets {
    entries: Vec<SecretEntry>,
}

#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SecretEntry {
    id: String,
    /// The kubeconfig as YAML.
    kubeconfig: String,
    /// File it was imported from, for "Update from file".
    origin: Option<String>,
    /// Unix seconds.
    imported_at: u64,
}

impl Envelope {
    fn key_mode(&self) -> KeyMode {
        KeyMode {
            password: self
                .slots
                .iter()
                .any(|slot| matches!(slot, Slot::Password { .. })),
            keychain: self
                .slots
                .iter()
                .any(|slot| matches!(slot, Slot::Keychain { .. })),
            biometric: self
                .slots
                .iter()
                .any(|slot| matches!(slot, Slot::Biometric { .. })),
        }
    }

    fn keychain_account(&self) -> Option<&str> {
        self.slots.iter().find_map(|slot| match slot {
            Slot::Keychain { account, .. } => Some(account.as_str()),
            _ => None,
        })
    }

    fn biometric_account(&self) -> Option<&str> {
        self.slots.iter().find_map(|slot| match slot {
            Slot::Biometric { account, .. } => Some(account.as_str()),
            _ => None,
        })
    }
}

/// The vault file, read but not decrypted.
#[derive(Clone, Debug)]
pub struct LockedVault {
    path: PathBuf,
    envelope: Envelope,
}

impl LockedVault {
    /// Reads the vault at `path`; `Ok(None)` when there is none. A file that
    /// can't be parsed is copied aside and reported as corrupted.
    pub fn read(path: &Path) -> Result<Option<Self>, VaultError> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let envelope: Envelope = match serde_json::from_slice(&bytes) {
            Ok(envelope) => envelope,
            Err(error) => return Err(corrupted(path, format!("unreadable: {error}"))),
        };
        if envelope.format != FORMAT {
            return Err(corrupted(path, "not a Kubach vault".into()));
        }
        if envelope.version > VERSION {
            return Err(VaultError::UnsupportedVersion(envelope.version));
        }
        // Bounds before any derivation: a huge m_cost would otherwise
        // allocate that much memory on unlock.
        for slot in &envelope.slots {
            if let Slot::Password { kdf, .. } = slot
                && !kdf.params.is_supported()
            {
                let KdfParams {
                    m_cost,
                    t_cost,
                    p_cost,
                } = kdf.params;
                return Err(corrupted(
                    path,
                    format!(
                        "key derivation parameters out of the supported range: \
                         m_cost {m_cost} KiB, t_cost {t_cost}, p_cost {p_cost}"
                    ),
                ));
            }
        }
        Ok(Some(Self {
            path: path.to_owned(),
            envelope,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[EntryInfo] {
        &self.envelope.entries
    }

    pub fn key_mode(&self) -> KeyMode {
        self.envelope.key_mode()
    }

    /// Contexts of every entry, with [`FileProblem::Locked`].
    pub fn imported(&self) -> Vec<Imported> {
        self.envelope
            .entries
            .iter()
            .map(|entry| Imported {
                source: source_of(&self.path, entry),
                config: Err(FileProblem::Locked),
                contexts: entry.contexts.clone(),
            })
            .collect()
    }

    /// Unlocks with `password`. Slow on purpose (Argon2id): run it off the
    /// UI thread.
    pub fn unlock_with_password(&self, password: &str) -> Result<Vault, VaultError> {
        let mut found = false;
        for slot in &self.envelope.slots {
            let Slot::Password {
                kdf,
                nonce,
                wrapped_key,
            } = slot
            else {
                continue;
            };
            found = true;
            if kdf.alg != "argon2id" {
                return Err(VaultError::UnsupportedVersion(self.envelope.version));
            }
            let kek = crypto::derive_key(password.as_bytes(), &kdf.salt, kdf.params)
                .map_err(|_| corrupted(&self.path, "invalid key derivation parameters".into()))?;
            let Ok(key) = crypto::open(&kek, nonce, wrapped_key, PASSWORD_AAD) else {
                continue;
            };
            return self.open_with(key_from(&key)?);
        }
        Err(if found {
            VaultError::WrongPassword
        } else {
            VaultError::NoPassword
        })
    }

    /// Unlocks with the key kept in `store`.
    pub fn unlock_with_keychain(&self, store: &dyn KeyStore) -> Result<Vault, VaultError> {
        let Some((account, nonce, check)) =
            self.envelope.slots.iter().find_map(|slot| match slot {
                Slot::Keychain {
                    account,
                    nonce,
                    check,
                } => Some((account, nonce, check)),
                _ => None,
            })
        else {
            return Err(VaultError::NoKeychain);
        };
        let secret = store.get(account)?.ok_or(VaultError::KeychainMissing)?;
        let key = key_from(&secret).map_err(|_| VaultError::KeychainMismatch)?;
        crypto::open(&key, nonce, check, KEYCHAIN_AAD).map_err(|_| VaultError::KeychainMismatch)?;
        self.open_with(key)
    }

    /// Unlocks with the key `store` releases after Touch ID / Windows Hello.
    /// Blocks on the system prompt: run it off the UI thread.
    pub fn unlock_with_biometric(&self, store: &dyn BiometricStore) -> Result<Vault, VaultError> {
        let Some((account, nonce, wrapped_key)) =
            self.envelope.slots.iter().find_map(|slot| match slot {
                Slot::Biometric {
                    account,
                    nonce,
                    wrapped_key,
                } => Some((account, nonce, wrapped_key)),
                _ => None,
            })
        else {
            return Err(VaultError::NoBiometric);
        };
        let kek = store.open(account)?;
        let key = crypto::open(&kek, nonce, wrapped_key, BIOMETRIC_AAD)
            .map_err(|_| VaultError::BiometricMismatch)?;
        self.open_with(key_from(&key).map_err(|_| VaultError::BiometricMismatch)?)
    }

    /// Unlocks without a password: biometrics, then the keychain, whichever
    /// the vault has and is given. A cancelled biometric prompt stops here
    /// (the user chose the password); other failures go on to the next way.
    /// The error is that of the first way tried, for the password form.
    pub fn unlock_without_password(
        &self,
        biometric: Option<&dyn BiometricStore>,
        keychain: Option<&dyn KeyStore>,
    ) -> Result<Vault, VaultError> {
        let available = Availability {
            biometric: biometric.is_some(),
            keychain: keychain.is_some(),
        };
        let mut first_error = None;
        for method in self.key_mode().automatic(available) {
            let result = match (method, biometric, keychain) {
                (UnlockMethod::Biometric, Some(store), _) => self.unlock_with_biometric(store),
                (UnlockMethod::Keychain, _, Some(store)) => self.unlock_with_keychain(store),
                _ => continue,
            };
            match result {
                Ok(vault) => return Ok(vault),
                Err(error) if error.is_cancelled() || !error.falls_back_to_password() => {
                    return Err(error);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or(if self.key_mode().password {
            VaultError::NoKeychain
        } else {
            VaultError::NoKey
        }))
    }

    /// Decrypts the payload with the (verified) data key.
    fn open_with(&self, key: SecretKey) -> Result<Vault, VaultError> {
        let aad = header_aad(&self.envelope.entries);
        let plaintext =
            crypto::open(&key, &self.envelope.nonce, &self.envelope.ciphertext, &aad)
                .map_err(|_| corrupted(&self.path, "contents don't match the key".into()))?;
        let secrets: Secrets = serde_json::from_slice(&plaintext)
            .map_err(|error| corrupted(&self.path, format!("contents unreadable: {error}")))?;
        let listed: Vec<&str> = self
            .envelope
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        let stored: Vec<&str> = secrets.entries.iter().map(|e| e.id.as_str()).collect();
        if listed != stored {
            return Err(corrupted(&self.path, "entry list doesn't match".into()));
        }
        Ok(Vault {
            path: self.path.clone(),
            envelope: self.envelope.clone(),
            key,
            secrets,
        })
    }
}

fn key_from(bytes: &[u8]) -> Result<SecretKey, VaultError> {
    if bytes.len() != KEY_LEN {
        return Err(VaultError::KeychainMismatch);
    }
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    key.copy_from_slice(bytes);
    Ok(key)
}

fn source_of(path: &Path, entry: &EntryInfo) -> Source {
    Source::Imported {
        vault: path.to_owned(),
        id: entry.id.clone(),
        name: entry.name.clone(),
    }
}

/// Copies a damaged vault aside (once per distinct content) and builds the
/// error. The original is left alone; only a reset replaces it.
fn corrupted(path: &Path, reason: String) -> VaultError {
    VaultError::Corrupted {
        reason,
        backup: backup(path).ok().flatten(),
    }
}

fn backup(path: &Path) -> io::Result<Option<PathBuf>> {
    let bytes = std::fs::read(path)?;
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(None);
    };
    let prefix = format!("{}.corrupt-", name.to_string_lossy());
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix)
            && std::fs::read(entry.path()).is_ok_and(|existing| existing == bytes)
        {
            return Ok(Some(entry.path()));
        }
    }
    let target = (0..)
        .map(|n| match n {
            0 => dir.join(format!("{prefix}{}", now())),
            n => dir.join(format!("{prefix}{}-{n}", now())),
        })
        .find(|candidate| !candidate.exists())
        .expect("a free name exists");
    write_private(&target, &bytes)?;
    Ok(Some(target))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Writes `bytes` to `path` atomically, readable by the owner only.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        crate::settings::create_private_dir(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&tmp)?;
        crate::kubeconfig::restrict_permissions(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp).ok();
    }
    result
}

/// An unlocked vault. The data key and configs are wiped when dropped
/// (clones too: a clone lets slow keychain calls run off the UI thread).
#[derive(Clone)]
pub struct Vault {
    path: PathBuf,
    envelope: Envelope,
    key: SecretKey,
    secrets: Secrets,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("path", &self.path)
            .field("entries", &self.envelope.entries)
            .finish_non_exhaustive()
    }
}

impl Vault {
    /// Creates an empty vault at `path` protected by `password`, the
    /// `keychain` or both. Fails if a file is already there.
    pub fn create(
        path: &Path,
        password: Option<&DerivedPassword>,
        keychain: Option<&dyn KeyStore>,
    ) -> Result<Self, VaultError> {
        if path.exists() {
            return Err(VaultError::Exists(path.to_owned()));
        }
        let mut vault = Self {
            path: path.to_owned(),
            envelope: Envelope {
                format: FORMAT.into(),
                version: VERSION,
                entries: Vec::new(),
                slots: Vec::new(),
                nonce: Vec::new(),
                ciphertext: Vec::new(),
            },
            key: crypto::random_key(),
            secrets: Secrets::default(),
        };
        vault.rekey(password, keychain, None, None)?;
        Ok(vault)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[EntryInfo] {
        &self.envelope.entries
    }

    pub fn entry(&self, id: &str) -> Option<&EntryInfo> {
        self.envelope.entries.iter().find(|entry| entry.id == id)
    }

    pub fn key_mode(&self) -> KeyMode {
        self.envelope.key_mode()
    }

    /// The vault as it is on disk, for locking.
    pub fn locked(&self) -> LockedVault {
        LockedVault {
            path: self.path.clone(),
            envelope: self.envelope.clone(),
        }
    }

    /// All context names of the vault.
    pub fn context_names(&self) -> HashSet<String> {
        self.envelope
            .entries
            .iter()
            .flat_map(|entry| entry.contexts.iter().cloned())
            .collect()
    }

    fn secret(&self, id: &str) -> Option<&SecretEntry> {
        self.secrets.entries.iter().find(|entry| entry.id == id)
    }

    /// The stored kubeconfig YAML of `id`, e.g. for export.
    pub fn kubeconfig_yaml(&self, id: &str) -> Option<&str> {
        self.secret(id).map(|entry| entry.kubeconfig.as_str())
    }

    /// File `id` was imported from, if any.
    pub fn origin(&self, id: &str) -> Option<&Path> {
        self.secret(id)?.origin.as_deref().map(Path::new)
    }

    /// Every entry with its parsed config.
    pub fn imported(&self) -> Vec<Imported> {
        self.envelope
            .entries
            .iter()
            .map(|entry| Imported {
                source: source_of(&self.path, entry),
                config: self
                    .kubeconfig_yaml(&entry.id)
                    .ok_or(FileProblem::Missing)
                    .and_then(|yaml| {
                        Kubeconfig::from_yaml(yaml)
                            .map_err(|error| FileProblem::Invalid(error.to_string()))
                    }),
                contexts: entry.contexts.clone(),
            })
            .collect()
    }

    /// Adds `plan` as a new entry named `name` and saves; returns its id.
    pub fn add(
        &mut self,
        name: &str,
        plan: &ImportPlan,
        origin: Option<&Path>,
    ) -> Result<String, VaultError> {
        if !plan.names_valid() {
            return Err(VaultError::ContextNames);
        }
        if plan.unread_files() {
            return Err(VaultError::UnreadFiles);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let yaml = plan.yaml().map_err(VaultError::Serialize)?;
        self.change(|envelope, secrets| {
            envelope.entries.push(EntryInfo {
                id: id.clone(),
                name: name.trim().to_owned(),
                contexts: plan.context_names(),
            });
            secrets.entries.push(SecretEntry {
                id: id.clone(),
                kubeconfig: yaml.to_string(),
                origin: origin.map(|path| path.to_string_lossy().into_owned()),
                imported_at: now(),
            });
        })?;
        Ok(id)
    }

    /// Replaces the config of `id` with `plan` ("Update from file").
    pub fn replace(
        &mut self,
        id: &str,
        plan: &ImportPlan,
        origin: Option<&Path>,
    ) -> Result<(), VaultError> {
        self.check(id)?;
        if !plan.names_valid() {
            return Err(VaultError::ContextNames);
        }
        if plan.unread_files() {
            return Err(VaultError::UnreadFiles);
        }
        let yaml = plan.yaml().map_err(VaultError::Serialize)?;
        self.change(|envelope, secrets| {
            if let Some(entry) = envelope.entries.iter_mut().find(|e| e.id == id) {
                entry.contexts = plan.context_names();
            }
            if let Some(entry) = secrets.entries.iter_mut().find(|e| e.id == id) {
                entry.kubeconfig = yaml.to_string();
                entry.origin = origin.map(|path| path.to_string_lossy().into_owned());
                entry.imported_at = now();
            }
        })
    }

    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), VaultError> {
        self.check(id)?;
        self.change(|envelope, _| {
            if let Some(entry) = envelope.entries.iter_mut().find(|e| e.id == id) {
                entry.name = name.trim().to_owned();
            }
        })
    }

    pub fn remove(&mut self, id: &str) -> Result<(), VaultError> {
        self.check(id)?;
        self.change(|envelope, secrets| {
            envelope.entries.retain(|entry| entry.id != id);
            secrets.entries.retain(|entry| entry.id != id);
        })
    }

    fn check(&self, id: &str) -> Result<(), VaultError> {
        match self.entry(id) {
            Some(_) => Ok(()),
            None => Err(VaultError::UnknownEntry(id.to_owned())),
        }
    }

    /// Applies `edit` and saves; on failure nothing changes in memory.
    fn change(&mut self, edit: impl FnOnce(&mut Envelope, &mut Secrets)) -> Result<(), VaultError> {
        let (mut envelope, mut secrets) = (self.envelope.clone(), self.secrets.clone());
        edit(&mut envelope, &mut secrets);
        let previous = std::mem::replace(&mut self.secrets, secrets);
        if let Err(error) = self.write(envelope) {
            self.secrets = previous;
            return Err(error);
        }
        Ok(())
    }

    /// `envelope` with `secrets` encrypted under the current key.
    fn sealed(&self, mut envelope: Envelope, secrets: &Secrets) -> Result<Envelope, VaultError> {
        let plaintext = Zeroizing::new(
            serde_json::to_vec(secrets)
                .map_err(|error| VaultError::Serialize(error.to_string()))?,
        );
        let (ciphertext, nonce) =
            crypto::seal(&self.key, &plaintext, &header_aad(&envelope.entries));
        envelope.ciphertext = ciphertext;
        envelope.nonce = nonce.to_vec();
        Ok(envelope)
    }

    /// Sets a new password (or adds one to a keychain-only vault): a new
    /// data key, everything encrypted again, the keychain copy (if any)
    /// replaced. `keychain` is required when the vault uses it. A biometric
    /// slot gets a fresh OS key from `biometric` (Windows Hello asks once
    /// more); without it, or if that fails, biometric unlock is turned off.
    pub fn change_password(
        &mut self,
        password: &DerivedPassword,
        keychain: Option<&dyn KeyStore>,
        biometric: Option<&dyn BiometricStore>,
    ) -> Result<(), VaultError> {
        let uses_keychain = self.key_mode().keychain;
        if uses_keychain && keychain.is_none() {
            return Err(VaultError::NoKeychain);
        }
        let old_biometric = self.envelope.biometric_account().map(str::to_owned);
        let new_biometric = biometric.filter(|_| old_biometric.is_some());
        self.rekey(
            Some(password),
            keychain.filter(|_| uses_keychain),
            new_biometric,
            keychain,
        )?;
        if let (Some(account), Some(store)) = (old_biometric, biometric) {
            store.delete(&account).ok();
        }
        Ok(())
    }

    /// Also wraps the data key with a key the OS releases after Touch ID /
    /// Windows Hello. The password stays: it is the way back when the
    /// prompt is cancelled or fingerprints change, so it is required.
    /// Replaces an existing biometric slot (e.g. after fingerprints
    /// changed).
    pub fn enable_biometric(&mut self, store: &dyn BiometricStore) -> Result<(), VaultError> {
        if !self.key_mode().password {
            return Err(VaultError::BiometricNeedsPassword);
        }
        let old = self.envelope.biometric_account().map(str::to_owned);
        let account = uuid::Uuid::new_v4().to_string();
        let slot = biometric_slot(store, &self.key, &account)?;
        let mut envelope = self.envelope.clone();
        envelope
            .slots
            .retain(|slot| !matches!(slot, Slot::Biometric { .. }));
        envelope.slots.push(slot);
        if let Err(error) = self.write(envelope) {
            store.delete(&account).ok();
            return Err(error);
        }
        if let Some(old) = old {
            store.delete(&old).ok();
        }
        Ok(())
    }

    /// Stops unlocking with Touch ID / Windows Hello and deletes the OS key.
    pub fn disable_biometric(&mut self, store: &dyn BiometricStore) -> Result<(), VaultError> {
        let Some(account) = self.envelope.biometric_account().map(str::to_owned) else {
            return Ok(());
        };
        let mut envelope = self.envelope.clone();
        envelope
            .slots
            .retain(|slot| !matches!(slot, Slot::Biometric { .. }));
        self.write(envelope)?;
        // A stale OS key is harmless; ignore failures.
        store.delete(&account).ok();
        Ok(())
    }

    /// Also keeps the data key in `store`, so the vault unlocks without the
    /// password ("Remember in keychain").
    pub fn enable_keychain(&mut self, store: &dyn KeyStore) -> Result<(), VaultError> {
        if self.key_mode().keychain {
            return Ok(());
        }
        let account = uuid::Uuid::new_v4().to_string();
        store.set(&account, self.key.as_slice())?;
        let mut envelope = self.envelope.clone();
        envelope
            .slots
            .push(keychain_slot(&self.key, account.clone()));
        if let Err(error) = self.write(envelope) {
            store.delete(&account).ok();
            return Err(error);
        }
        Ok(())
    }

    /// Stops keeping the key in `store`; the password stays the only way in.
    pub fn disable_keychain(&mut self, store: &dyn KeyStore) -> Result<(), VaultError> {
        let Some(account) = self.envelope.keychain_account().map(str::to_owned) else {
            return Ok(());
        };
        if !self.key_mode().password {
            return Err(VaultError::NoKey);
        }
        let mut envelope = self.envelope.clone();
        envelope
            .slots
            .retain(|slot| !matches!(slot, Slot::Keychain { .. }));
        self.write(envelope)?;
        // A stale entry is harmless; ignore failures.
        store.delete(&account).ok();
        Ok(())
    }

    /// Writes `envelope` with the current secrets.
    fn write(&mut self, envelope: Envelope) -> Result<(), VaultError> {
        let envelope = self.sealed(envelope, &self.secrets)?;
        write_private(
            &self.path,
            &serde_json::to_vec_pretty(&envelope).expect("serializes"),
        )?;
        self.envelope = envelope;
        Ok(())
    }

    /// Re-encrypts everything under a fresh data key protected by
    /// `password` and/or `keychain` (and `biometric`, next to a password),
    /// then drops the old keychain entry from `cleanup`. On failure the
    /// vault on disk and in memory stays as it was.
    fn rekey(
        &mut self,
        password: Option<&DerivedPassword>,
        keychain: Option<&dyn KeyStore>,
        biometric: Option<&dyn BiometricStore>,
        cleanup: Option<&dyn KeyStore>,
    ) -> Result<(), VaultError> {
        if password.is_none() && keychain.is_none() {
            return Err(VaultError::NoKey);
        }
        let data_key = crypto::random_key();
        let mut slots: Vec<Slot> = password.map(|p| p.slot(&data_key)).into_iter().collect();
        // Best effort: a new password must not hinge on the OS prompt; if
        // the biometric key can't be made, that way in is simply off.
        let new_biometric = biometric.filter(|_| password.is_some()).and_then(|store| {
            let account = uuid::Uuid::new_v4().to_string();
            let slot = biometric_slot(store, &data_key, &account).ok()?;
            slots.push(slot);
            Some((store, account))
        });
        let new_account = match keychain {
            Some(store) => {
                let account = uuid::Uuid::new_v4().to_string();
                if let Err(error) = store.set(&account, data_key.as_slice()) {
                    if let Some((biometric, account)) = &new_biometric {
                        biometric.delete(account).ok();
                    }
                    return Err(error.into());
                }
                slots.push(keychain_slot(&data_key, account.clone()));
                Some((store, account))
            }
            None => None,
        };
        let old_account = self.envelope.keychain_account().map(str::to_owned);
        let old_key = std::mem::replace(&mut self.key, data_key);
        let mut envelope = self.envelope.clone();
        envelope.slots = slots;
        match self.write(envelope) {
            Ok(()) => {
                if let (Some(old), Some(store)) = (old_account, cleanup) {
                    store.delete(&old).ok();
                }
                Ok(())
            }
            Err(error) => {
                self.key = old_key;
                if let Some((store, account)) = new_account {
                    store.delete(&account).ok();
                }
                if let Some((store, account)) = new_biometric {
                    store.delete(&account).ok();
                }
                Err(error)
            }
        }
    }
}

fn keychain_slot(key: &[u8; KEY_LEN], account: String) -> Slot {
    let (check, nonce) = crypto::seal(key, b"", KEYCHAIN_AAD);
    Slot::Keychain {
        account,
        nonce: nonce.to_vec(),
        check,
    }
}

fn biometric_slot(
    store: &dyn BiometricStore,
    data_key: &[u8; KEY_LEN],
    account: &str,
) -> Result<Slot, VaultError> {
    let kek = store.create(account)?;
    let (wrapped_key, nonce) = crypto::seal(&kek, data_key, BIOMETRIC_AAD);
    Ok(Slot::Biometric {
        account: account.to_owned(),
        nonce: nonce.to_vec(),
        wrapped_key,
    })
}

/// What [`reset`] could not clean up.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct ResetOutcome {
    /// A keychain entry or biometric key may be left in the OS store: the
    /// vault and its backups gave no readable key slots, or the store
    /// refused to delete one.
    pub keys_left: bool,
}

impl ResetOutcome {
    /// What to tell the user, if anything: the store entries are named
    /// after random accounts, so only a search by service name finds them.
    pub fn warning(&self) -> Option<Message> {
        self.keys_left.then(|| {
            msg!(
                "vault-reset-keys-left",
                keychain = keychain::SERVICE,
                biometric = biometric::SERVICE
            )
        })
    }
}

/// Deletes the vault at `path`, its keychain entry, biometric key, the
/// `.corrupt-` backups of damaged files and a leftover `.tmp`. Imported
/// configs are lost. Works on damaged files too: the store accounts are
/// then taken from the backups, without making a new one.
pub fn reset(
    path: &Path,
    keychain: Option<&dyn KeyStore>,
    biometric: Option<&dyn BiometricStore>,
) -> Result<ResetOutcome, VaultError> {
    let leftovers = leftover_files(path)?;
    // A readable vault names its own accounts; older ones in backups are
    // orphans by now and go too.
    let vault_slots = match std::fs::read(path) {
        Ok(bytes) => slot_accounts(&bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(SlotAccounts::default()),
        Err(_) => None,
    };
    let backup_slots: Vec<SlotAccounts> = leftovers
        .iter()
        .filter_map(|path| std::fs::read(path).ok())
        .filter_map(|bytes| slot_accounts(&bytes))
        .collect();
    let found = vault_slots.is_some() || !backup_slots.is_empty();
    let mut keychain_accounts = BTreeSet::new();
    let mut biometric_accounts = BTreeSet::new();
    for slots in vault_slots.into_iter().chain(backup_slots) {
        keychain_accounts.extend(slots.keychain);
        biometric_accounts.extend(slots.biometric);
    }
    let mut keys_left = !found;
    for account in &keychain_accounts {
        match keychain {
            Some(store) => keys_left |= store.delete(account).is_err(),
            None => keys_left = true,
        }
    }
    for account in &biometric_accounts {
        match biometric {
            Some(store) => keys_left |= store.delete(account).is_err(),
            None => keys_left = true,
        }
    }

    let mut first_error = None;
    for file in leftovers.iter().map(PathBuf::as_path).chain([path]) {
        match std::fs::remove_file(file) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(ResetOutcome { keys_left }),
    }
}

/// Keychain and biometric accounts named in the slots of a vault file,
/// read leniently: a file with a broken header or ciphertext still names
/// them. `None` when the slots can't be found at all.
fn slot_accounts(bytes: &[u8]) -> Option<SlotAccounts> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let slots = value.get("slots")?.as_array()?;
    let mut accounts = SlotAccounts::default();
    for slot in slots {
        let Some(account) = slot.get("account").and_then(|a| a.as_str()) else {
            continue;
        };
        match slot.get("kind").and_then(|kind| kind.as_str()) {
            Some("keychain") => accounts.keychain.push(account.to_owned()),
            Some("biometric") => accounts.biometric.push(account.to_owned()),
            _ => {}
        }
    }
    Some(accounts)
}

#[derive(Default)]
struct SlotAccounts {
    keychain: Vec<String>,
    biometric: Vec<String>,
}

/// Files next to the vault that Kubach itself writes: `<name>.tmp`,
/// `<name>.corrupt-<time>[-<n>]` and their own `.tmp`. Nothing else in the
/// folder matches.
fn leftover_files(path: &Path) -> io::Result<Vec<PathBuf>> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(Vec::new());
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let name = name.to_string_lossy();
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        if let Some(file) = entry.file_name().to_str()
            && is_leftover(&name, file)
        {
            files.push(dir.join(file));
        }
    }
    files.sort();
    Ok(files)
}

fn is_leftover(vault: &str, file: &str) -> bool {
    let Some(rest) = file.strip_prefix(vault) else {
        return false;
    };
    if rest == ".tmp" {
        return true;
    }
    let Some(stamp) = rest.strip_prefix(".corrupt-") else {
        return false;
    };
    let stamp = stamp.strip_suffix(".tmp").unwrap_or(stamp);
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    match stamp.split_once('-') {
        Some((time, n)) => digits(time) && digits(n),
        None => digits(stamp),
    }
}

/// Failed unlock attempts and the wait they impose.
#[derive(Clone, Debug, Default)]
pub struct Attempts {
    failures: u32,
}

impl Attempts {
    /// Failures allowed without waiting.
    pub const FREE: u32 = 3;
    /// Longest wait between attempts.
    pub const MAX_DELAY: Duration = Duration::from_secs(30);

    pub fn failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    pub fn succeeded(&mut self) {
        self.failures = 0;
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Wait before the next attempt: none for the first [`FREE`](Self::FREE)
    /// failures, then 1 s, 2 s, 4 s… up to [`MAX_DELAY`](Self::MAX_DELAY).
    pub fn delay(&self) -> Duration {
        match self.failures.checked_sub(Self::FREE) {
            None => Duration::ZERO,
            Some(extra) => Duration::from_secs(1u64 << extra.min(5)).min(Self::MAX_DELAY),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keychain::MemoryKeyStore;
    use super::*;

    #[test]
    fn errors_have_a_translation_and_the_same_english_text() {
        use kubach_i18n::Locale;
        let errors = [
            VaultError::WrongPassword,
            VaultError::Corrupted {
                reason: "bad tag".into(),
                backup: Some(PathBuf::from("/tmp/vault.kubach.corrupt-1")),
            },
            VaultError::Corrupted {
                reason: "bad tag".into(),
                backup: None,
            },
            VaultError::UnsupportedVersion(9),
            VaultError::NoPassword,
            VaultError::NoKeychain,
            VaultError::KeychainMissing,
            VaultError::KeychainMismatch,
            VaultError::NoKey,
            VaultError::NoBiometric,
            VaultError::BiometricNeedsPassword,
            VaultError::BiometricMismatch,
            VaultError::Biometric(BiometricError::Cancelled),
            VaultError::Biometric(BiometricError::Invalidated),
            VaultError::Biometric(BiometricError::Unavailable("no sensor".into())),
            VaultError::Biometric(BiometricError::Failed("-1".into())),
            VaultError::EmptyPassword,
            VaultError::Exists(PathBuf::from("/tmp/vault.kubach")),
            VaultError::UnknownEntry("a1".into()),
            VaultError::Serialize("eof".into()),
            VaultError::ContextNames,
            VaultError::UnreadFiles,
            VaultError::KeyStore(KeyStoreError("denied".into())),
            VaultError::Io(io::Error::other("disk full")),
        ];
        for error in errors {
            // English stays the text of logs and MCP.
            assert_eq!(error.message().english(), error.to_string());
            let russian = error.message().translate(Locale::Ru);
            assert!(!russian.contains("vault-error"), "{russian}");
        }
        assert_eq!(
            VaultError::WrongPassword.message().translate(Locale::Ru),
            "Неверный пароль"
        );
        let outcome = ResetOutcome { keys_left: true };
        assert!(
            outcome
                .warning()
                .unwrap()
                .translate(Locale::Ru)
                .contains(keychain::SERVICE)
        );
    }

    const CONFIG: &str = r#"
apiVersion: v1
kind: Config
clusters:
  - name: c
    cluster: { server: "https://c.example.com" }
users:
  - name: u
    user: { token: very-secret-token }
contexts:
  - name: team-prod
    context: { cluster: c, user: u }
"#;

    fn plan() -> ImportPlan {
        ImportPlan::from_yaml(CONFIG, &HashSet::new()).unwrap()
    }

    fn derived(password: &str) -> DerivedPassword {
        DerivedPassword::derive(password, KdfParams::INSECURE_FAST).unwrap()
    }

    fn create(path: &Path, password: &str) -> Result<Vault, VaultError> {
        let password = DerivedPassword::derive(password, KdfParams::INSECURE_FAST)?;
        Vault::create(path, Some(&password), None)
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(VAULT_FILE);
        (dir, path)
    }

    #[test]
    fn context_names_from_the_header_without_side_effects() {
        let (dir, path) = setup();
        assert!(context_names(&path).is_empty());
        let mut vault = create(&path, "pw").unwrap();
        vault.add("team.yaml", &plan(), None).unwrap();
        assert_eq!(context_names(&path), ["team-prod"]);

        std::fs::write(&path, "{ damaged").unwrap();
        assert!(context_names(&path).is_empty());
        // No `.corrupt-` backup, unlike `LockedVault::read`.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn encrypts_and_decrypts_with_a_password() {
        let (_dir, path) = setup();
        let mut vault = create(&path, "pw").unwrap();
        let id = vault
            .add("team.yaml", &plan(), Some(Path::new("/x/team.yaml")))
            .unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("very-secret-token"));
        assert!(!raw.contains("c.example.com"));
        // Names stay readable so tiles show while locked.
        assert!(raw.contains("team-prod"));

        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert_eq!(locked.entries()[0].contexts, ["team-prod"]);
        assert_eq!(
            locked.key_mode(),
            KeyMode {
                password: true,
                keychain: false,
                biometric: false,
            }
        );
        assert!(matches!(
            locked.imported()[0].config,
            Err(FileProblem::Locked)
        ));

        let vault = locked.unlock_with_password("pw").unwrap();
        assert!(
            vault
                .kubeconfig_yaml(&id)
                .unwrap()
                .contains("very-secret-token")
        );
        assert_eq!(vault.origin(&id), Some(Path::new("/x/team.yaml")));
        let imported = vault.imported();
        assert_eq!(
            imported[0].config.as_ref().unwrap().contexts[0].name,
            "team-prod"
        );
        assert_eq!(
            imported[0].source,
            Source::Imported {
                vault: path.clone(),
                id: id.clone(),
                name: "team.yaml".into()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_is_private() {
        let (_dir, path) = setup();
        create(&path, "pw").unwrap();
        assert_eq!(crate::kubeconfig::loose_permissions(&path), None);
        assert!(std::fs::metadata(&path).is_ok());
    }

    #[test]
    fn missing_vault_reads_as_none_and_create_never_overwrites() {
        let (_dir, path) = setup();
        assert!(LockedVault::read(&path).unwrap().is_none());
        create(&path, "pw").unwrap();
        assert!(matches!(create(&path, "other"), Err(VaultError::Exists(_))));
        assert!(matches!(
            create(&path.with_extension("x"), ""),
            Err(VaultError::EmptyPassword)
        ));
    }

    #[test]
    fn wrong_password_is_reported_and_attempts_slow_down() {
        let (_dir, path) = setup();
        create(&path, "right").unwrap();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_password("wrong"),
            Err(VaultError::WrongPassword)
        ));
        assert_eq!(VaultError::WrongPassword.to_string(), "Wrong password");
        // A wrong password never creates a backup.
        let files = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
        assert_eq!(files, 1);

        let mut attempts = Attempts::default();
        let delays: Vec<u64> = (0..10)
            .map(|_| {
                attempts.failed();
                attempts.delay().as_secs()
            })
            .collect();
        assert_eq!(delays, [0, 0, 1, 2, 4, 8, 16, 30, 30, 30]);
        attempts.succeeded();
        assert_eq!(attempts.delay(), Duration::ZERO);
    }

    #[test]
    fn damaged_file_is_kept_and_backed_up() {
        let (dir, path) = setup();
        let mut vault = create(&path, "pw").unwrap();
        vault.add("team", &plan(), None).unwrap();
        let good = std::fs::read_to_string(&path).unwrap();

        // Truncated JSON.
        std::fs::write(&path, &good[..good.len() / 2]).unwrap();
        let error = LockedVault::read(&path).unwrap_err();
        let VaultError::Corrupted {
            backup: Some(backup),
            ..
        } = &error
        else {
            panic!("{error:?}");
        };
        assert_eq!(
            std::fs::read_to_string(backup).unwrap(),
            &good[..good.len() / 2]
        );
        assert!(error.to_string().contains("A copy was saved"));
        // Reading again doesn't pile up copies; the damaged file stays.
        LockedVault::read(&path).unwrap_err();
        let copies = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("corrupt")
            })
            .count();
        assert_eq!(copies, 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            &good[..good.len() / 2]
        );

        // Ciphertext flipped: the password is right, the contents are not.
        let mut envelope: serde_json::Value = serde_json::from_str(&good).unwrap();
        let ciphertext = envelope["ciphertext"].as_str().unwrap().to_owned();
        let flipped = if ciphertext.starts_with('A') {
            "B"
        } else {
            "A"
        };
        envelope["ciphertext"] = format!("{flipped}{}", &ciphertext[1..]).into();
        std::fs::write(&path, envelope.to_string()).unwrap();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_password("pw"),
            Err(VaultError::Corrupted {
                backup: Some(_),
                ..
            })
        ));

        // Tampered public entry list fails authentication too.
        let mut envelope: serde_json::Value = serde_json::from_str(&good).unwrap();
        envelope["entries"][0]["contexts"][0] = "evil".into();
        std::fs::write(&path, envelope.to_string()).unwrap();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_password("pw"),
            Err(VaultError::Corrupted { .. })
        ));

        // Reset deletes it; a new vault can then be created.
        assert_eq!(reset(&path, None, None).unwrap(), ResetOutcome::default());
        assert!(LockedVault::read(&path).unwrap().is_none());
        // The `.corrupt-` copy goes with it.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        create(&path, "new").unwrap();
    }

    #[test]
    fn oversized_kdf_parameters_are_refused_before_deriving() {
        let (_dir, path) = setup();
        create(&path, "pw").unwrap();
        let good: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (field, value) in [
            ("m_cost", u64::from(u32::MAX)),
            ("m_cost", u64::from(KdfParams::MAX_M_COST) + 1),
            ("t_cost", u64::from(KdfParams::MAX_T_COST) + 1),
            ("p_cost", u64::from(KdfParams::MAX_P_COST) + 1),
            ("p_cost", 0),
        ] {
            let mut envelope = good.clone();
            envelope["slots"][0]["kdf"][field] = value.into();
            std::fs::write(&path, envelope.to_string()).unwrap();
            let error = LockedVault::read(&path).unwrap_err();
            assert!(
                matches!(&error, VaultError::Corrupted { reason, .. }
                    if reason.contains("out of the supported range")),
                "{field}={value}: {error:?}"
            );
        }
        // Within bounds the file still opens.
        std::fs::write(&path, good.to_string()).unwrap();
        LockedVault::read(&path)
            .unwrap()
            .unwrap()
            .unlock_with_password("pw")
            .unwrap();
    }

    #[test]
    fn newer_format_is_refused_without_backup() {
        let (_dir, path) = setup();
        create(&path, "pw").unwrap();
        let mut envelope: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        envelope["version"] = 2.into();
        std::fs::write(&path, envelope.to_string()).unwrap();
        assert!(matches!(
            LockedVault::read(&path),
            Err(VaultError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn password_change_reencrypts_everything() {
        let (_dir, path) = setup();
        let mut vault = create(&path, "old").unwrap();
        let id = vault.add("team", &plan(), None).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        vault.change_password(&derived("new"), None, None).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        let ciphertext = |raw: &str| {
            serde_json::from_str::<serde_json::Value>(raw).unwrap()["ciphertext"].clone()
        };
        assert_ne!(ciphertext(&before), ciphertext(&after));

        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_password("old"),
            Err(VaultError::WrongPassword)
        ));
        let vault = locked.unlock_with_password("new").unwrap();
        assert!(vault.kubeconfig_yaml(&id).is_some());
    }

    #[test]
    fn entries_can_be_renamed_replaced_and_removed() {
        let (_dir, path) = setup();
        let mut vault = create(&path, "pw").unwrap();
        let a = vault.add("a", &plan(), None).unwrap();
        let taken = vault.context_names();
        let b = vault
            .add("b", &ImportPlan::from_yaml(CONFIG, &taken).unwrap(), None)
            .unwrap();
        assert_eq!(vault.entry(&b).unwrap().contexts, ["team-prod-imported"]);

        // A plan renamed back onto a taken name is refused.
        let mut clash = ImportPlan::from_yaml(CONFIG, &vault.context_names()).unwrap();
        clash.rename(0, "team-prod");
        assert!(matches!(
            vault.add("c", &clash, None),
            Err(VaultError::ContextNames)
        ));
        assert!(matches!(
            vault.replace(&b, &clash, None),
            Err(VaultError::ContextNames)
        ));
        assert_eq!(vault.imported().len(), 2);

        // Pasted YAML with file references needs the files read first.
        let with_file = CONFIG.replace("team-prod", "team-files").replace(
            "token: very-secret-token",
            "tokenFile: /nonexistent/kubach-test-token",
        );
        let mut unread = ImportPlan::from_yaml(&with_file, &HashSet::new()).unwrap();
        assert!(unread.unread_files());
        assert!(matches!(
            vault.add("files", &unread, None),
            Err(VaultError::UnreadFiles)
        ));
        assert!(matches!(
            vault.replace(&b, &unread, None),
            Err(VaultError::UnreadFiles)
        ));
        unread.read_files();
        assert_eq!(unread.warnings.len(), 1);
        assert_eq!(vault.imported().len(), 2);

        vault.rename(&a, " renamed ").unwrap();
        let replacement = CONFIG.replace("team-prod", "team-dev");
        vault
            .replace(
                &a,
                &ImportPlan::from_yaml(&replacement, &HashSet::new()).unwrap(),
                Some(Path::new("/new")),
            )
            .unwrap();
        vault.remove(&b).unwrap();
        assert!(matches!(vault.remove(&b), Err(VaultError::UnknownEntry(_))));

        let vault = LockedVault::read(&path)
            .unwrap()
            .unwrap()
            .unlock_with_password("pw")
            .unwrap();
        assert_eq!(vault.entries().len(), 1);
        assert_eq!(vault.entries()[0].name, "renamed");
        assert_eq!(vault.entries()[0].contexts, ["team-dev"]);
        assert_eq!(vault.origin(&a), Some(Path::new("/new")));
    }

    #[test]
    fn keychain_and_mixed_key_modes() {
        let (_dir, path) = setup();
        let store = MemoryKeyStore::default();

        // Keychain only.
        let mut vault = Vault::create(&path, None, Some(&store)).unwrap();
        let id = vault.add("team", &plan(), None).unwrap();
        assert_eq!(store.len(), 1);
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_password("x"),
            Err(VaultError::NoPassword)
        ));
        let mut vault = locked.unlock_with_keychain(&store).unwrap();
        assert!(vault.kubeconfig_yaml(&id).is_some());
        // Can't drop the only key.
        assert!(matches!(
            vault.disable_keychain(&store),
            Err(VaultError::NoKey)
        ));

        // Add a password, keep the keychain: both unlock, one keychain entry.
        vault
            .change_password(&derived("pw"), Some(&store), None)
            .unwrap();
        assert_eq!(store.len(), 1);
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert_eq!(
            locked.key_mode(),
            KeyMode {
                password: true,
                keychain: true,
                biometric: false,
            }
        );
        locked.unlock_with_password("pw").unwrap();
        locked.unlock_with_keychain(&store).unwrap();
        assert!(matches!(
            vault.change_password(&derived("pw2"), None, None),
            Err(VaultError::NoKeychain)
        ));

        // Changing the password replaces the keychain copy too.
        vault
            .change_password(&derived("pw2"), Some(&store), None)
            .unwrap();
        assert_eq!(store.len(), 1);
        let locked = LockedVault::read(&path).unwrap().unwrap();
        locked.unlock_with_keychain(&store).unwrap();
        locked.unlock_with_password("pw2").unwrap();
        assert!(matches!(
            locked.unlock_with_keychain(&MemoryKeyStore::default()),
            Err(VaultError::KeychainMissing)
        ));

        // Password only: the keychain entry is removed.
        vault.disable_keychain(&store).unwrap();
        assert!(store.is_empty());
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_keychain(&store),
            Err(VaultError::NoKeychain)
        ));
        locked.unlock_with_password("pw2").unwrap();

        // Remember in keychain again, without changing the password.
        vault.enable_keychain(&store).unwrap();
        assert_eq!(store.len(), 1);
        let locked = LockedVault::read(&path).unwrap().unwrap();
        locked.unlock_with_keychain(&store).unwrap();
        locked.unlock_with_password("pw2").unwrap();

        assert!(matches!(
            Vault::create(&path.with_extension("none"), None, None),
            Err(VaultError::NoKey)
        ));
        assert!(!reset(&path, Some(&store), None).unwrap().keys_left);
        assert!(!path.exists());
        assert!(store.is_empty());
    }

    #[test]
    fn keychain_failures_leave_the_vault_unchanged() {
        let (_dir, path) = setup();
        let mut vault = create(&path, "pw").unwrap();
        vault.add("team", &plan(), None).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let broken = MemoryKeyStore::failing("no secret service");
        let error = vault.enable_keychain(&broken).unwrap_err();
        assert_eq!(error.to_string(), "no secret service");
        let error = vault.change_password(&derived("x"), Some(&broken), None);
        assert!(error.is_ok(), "keychain unused, so not touched");
        let before_rekey = std::fs::read_to_string(&path).unwrap();
        assert_ne!(before, before_rekey);
        LockedVault::read(&path)
            .unwrap()
            .unwrap()
            .unlock_with_password("x")
            .unwrap();

        // A key from another vault in the keychain is told apart from damage.
        let store = MemoryKeyStore::default();
        vault.enable_keychain(&store).unwrap();
        let account = LockedVault::read(&path)
            .unwrap()
            .unwrap()
            .envelope
            .keychain_account()
            .unwrap()
            .to_owned();
        store.set(&account, &[7; KEY_LEN]).unwrap();
        assert!(matches!(
            LockedVault::read(&path)
                .unwrap()
                .unwrap()
                .unlock_with_keychain(&store),
            Err(VaultError::KeychainMismatch)
        ));
        store.delete(&account).unwrap();
        assert!(matches!(
            LockedVault::read(&path)
                .unwrap()
                .unwrap()
                .unlock_with_keychain(&store),
            Err(VaultError::KeychainMissing)
        ));
        // A failing keychain during a password change keeps the old state.
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(
            vault
                .change_password(&derived("y"), Some(&broken), None)
                .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn automatic_unlock_order() {
        let mode = |password, keychain, biometric| KeyMode {
            password,
            keychain,
            biometric,
        };
        let all = Availability {
            biometric: true,
            keychain: true,
        };
        let none = Availability::default();
        use UnlockMethod::*;
        assert_eq!(mode(true, true, true).automatic(all), [Biometric, Keychain]);
        assert_eq!(mode(true, false, true).first(all), Biometric);
        // Unsigned macOS build, Linux, no sensor: straight to the password.
        assert_eq!(mode(true, false, true).first(none), Password);
        assert_eq!(
            mode(true, true, true).automatic(Availability {
                biometric: false,
                keychain: true
            }),
            [Keychain]
        );
        assert_eq!(mode(true, false, false).first(all), Password);
    }

    #[test]
    fn biometric_unlock_and_fallbacks() {
        use super::biometric::{BiometricError, BiometricStore as _, FakeBiometricStore};
        let (_dir, path) = setup();
        let bio = FakeBiometricStore::default();

        // Needs a password to fall back on.
        let store = MemoryKeyStore::default();
        let mut keychain_only =
            Vault::create(&path.with_extension("k"), None, Some(&store)).unwrap();
        assert!(matches!(
            keychain_only.enable_biometric(&bio),
            Err(VaultError::BiometricNeedsPassword)
        ));

        let mut vault = create(&path, "pw").unwrap();
        let id = vault.add("team", &plan(), None).unwrap();
        // Unavailable (unsigned build, Linux): nothing changes on disk.
        let before = std::fs::read_to_string(&path).unwrap();
        let error = vault
            .enable_biometric(&FakeBiometricStore::unavailable("needs a signed build"))
            .unwrap_err();
        assert!(error.to_string().contains("needs a signed build"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        vault.enable_biometric(&bio).unwrap();
        assert_eq!(bio.len(), 1);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains(r#""kind": "biometric""#));
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(locked.key_mode().biometric && locked.key_mode().password);
        let unlocked = locked.unlock_without_password(Some(&bio), None).unwrap();
        assert!(unlocked.kubeconfig_yaml(&id).is_some());
        locked.unlock_with_password("pw").unwrap();

        // Cancelled prompt: the password form, without trying the keychain.
        vault.enable_keychain(&store).unwrap();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        bio.fail_open(Some(BiometricError::Cancelled));
        let error = locked
            .unlock_without_password(Some(&bio), Some(&store))
            .unwrap_err();
        assert!(error.is_cancelled() && error.falls_back_to_password());
        // Another failure goes on to the keychain.
        bio.fail_open(Some(BiometricError::Failed("sensor busy".into())));
        locked
            .unlock_without_password(Some(&bio), Some(&store))
            .unwrap();
        bio.fail_open(None);
        vault.disable_keychain(&store).unwrap();

        // Fingerprints changed: the key is gone, a clear message, password.
        bio.invalidate();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        let error = locked
            .unlock_without_password(Some(&bio), None)
            .unwrap_err();
        assert!(matches!(
            error,
            VaultError::Biometric(BiometricError::Invalidated)
        ));
        assert!(error.falls_back_to_password());
        assert!(error.to_string().contains("Enter the password"));
        let mut vault = locked.unlock_with_password("pw").unwrap();
        // Turning it on again replaces the stale slot.
        vault.enable_biometric(&bio).unwrap();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        locked.unlock_with_biometric(&bio).unwrap();

        // A key that doesn't wrap this vault's data key.
        let other = FakeBiometricStore::default();
        let account = locked.envelope.biometric_account().unwrap().to_owned();
        other.create(&account).unwrap();
        assert!(matches!(
            locked.unlock_with_biometric(&other),
            Err(VaultError::BiometricMismatch)
        ));

        // Without a biometric store (Linux) only the password is left.
        assert!(matches!(
            locked.unlock_without_password(None, None),
            Err(VaultError::NoKeychain)
        ));
        // Damage is not a reason to ask for the password again.
        assert!(
            !VaultError::Corrupted {
                reason: "x".into(),
                backup: None
            }
            .falls_back_to_password()
        );

        // A new password re-creates the biometric key; the old one goes.
        vault
            .change_password(&derived("pw2"), None, Some(&bio))
            .unwrap();
        assert_eq!(bio.len(), 1);
        let locked = LockedVault::read(&path).unwrap().unwrap();
        locked.unlock_with_biometric(&bio).unwrap();
        locked.unlock_with_password("pw2").unwrap();
        // If the OS refuses a new key, the password still changes and
        // biometric unlock turns off.
        vault
            .change_password(
                &derived("pw3"),
                None,
                Some(&FakeBiometricStore::unavailable("gone")),
            )
            .unwrap();
        assert!(
            !LockedVault::read(&path)
                .unwrap()
                .unwrap()
                .key_mode()
                .biometric
        );
        // The previous OS key stayed behind (another store got the call).
        bio.invalidate();
        vault.enable_biometric(&bio).unwrap();

        // Off: the slot and the OS key are removed.
        vault.disable_biometric(&bio).unwrap();
        assert!(bio.is_empty());
        let locked = LockedVault::read(&path).unwrap().unwrap();
        assert!(matches!(
            locked.unlock_with_biometric(&bio),
            Err(VaultError::NoBiometric)
        ));
        locked.unlock_with_password("pw3").unwrap();

        // Reset deletes the OS key too.
        vault.enable_biometric(&bio).unwrap();
        assert!(!reset(&path, None, Some(&bio)).unwrap().keys_left);
        assert!(bio.is_empty());
        assert!(!path.exists());
    }

    /// Every file name in `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn reset_removes_backups_and_tmp_but_not_other_files() {
        let (dir, path) = setup();
        let store = MemoryKeyStore::default();
        let mut vault = Vault::create(&path, Some(&derived("pw")), Some(&store)).unwrap();
        vault.add("team", &plan(), None).unwrap();
        let good = std::fs::read_to_string(&path).unwrap();
        // A damaged file gets a backup.
        std::fs::write(&path, &good[..good.len() / 2]).unwrap();
        LockedVault::read(&path).unwrap_err();
        assert_eq!(names(dir.path()).len(), 2, "the vault and its backup");
        let ours = [
            "vault.kubach.tmp",
            "vault.kubach.corrupt-1700000000-2",
            "vault.kubach.corrupt-1700000001.tmp",
        ];
        let others = [
            "settings.json",
            "vault.kubach.bak",
            "vault.kubach.corrupt-",
            "vault.kubach.corrupt-x1",
            "vault.kubach.corrupt-1-",
            "vault.kubach.corrupt-1.old",
            "vault.kubach.tmp.keep",
            "other.kubach.corrupt-1",
            "my-vault.kubach.corrupt-1",
        ];
        for name in ours.iter().chain(&others) {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        std::fs::create_dir(dir.path().join("vault.kubach.corrupt-2")).unwrap();
        let outcome = reset(&path, Some(&store), None).unwrap();
        let mut left: Vec<String> = others.iter().map(|name| name.to_string()).collect();
        left.push("vault.kubach.corrupt-2".into());
        left.sort();
        assert_eq!(names(dir.path()), left);
        // Neither the file nor the backup names the keychain account.
        assert!(outcome.keys_left);
        assert_eq!(store.len(), 1);
        let warning = outcome.warning().unwrap().english();
        assert!(warning.contains(keychain::SERVICE), "{warning}");

        // Reset of a missing vault is fine and touches nothing else.
        assert_eq!(reset(&path, None, None).unwrap(), ResetOutcome::default());
        assert_eq!(names(dir.path()), left);
        assert_eq!(ResetOutcome::default().warning(), None);
    }

    #[test]
    fn reset_finds_store_accounts_in_backups() {
        use super::biometric::FakeBiometricStore;

        let (dir, path) = setup();
        let store = MemoryKeyStore::default();
        let bio = FakeBiometricStore::default();
        let mut vault = Vault::create(&path, Some(&derived("pw")), Some(&store)).unwrap();
        vault.enable_biometric(&bio).unwrap();
        let good = std::fs::read_to_string(&path).unwrap();
        // Readable header, damaged contents: unlocking backs it up.
        let mut envelope: serde_json::Value = serde_json::from_str(&good).unwrap();
        envelope["ciphertext"] = "AAAA".into();
        std::fs::write(&path, envelope.to_string()).unwrap();
        let locked = LockedVault::read(&path).unwrap().unwrap();
        locked.unlock_with_password("pw").unwrap_err();
        // Then the header breaks too.
        std::fs::write(&path, "{ damaged").unwrap();
        assert_eq!(names(dir.path()).len(), 2);

        let outcome = reset(&path, Some(&store), Some(&bio)).unwrap();
        assert_eq!(outcome, ResetOutcome::default());
        assert!(store.is_empty());
        assert!(bio.is_empty());
        assert!(names(dir.path()).is_empty(), "{:?}", names(dir.path()));
    }

    #[test]
    fn reset_reports_keys_the_store_refused_to_delete() {
        let (_dir, path) = setup();
        let store = MemoryKeyStore::default();
        Vault::create(&path, None, Some(&store)).unwrap();
        let outcome = reset(&path, Some(&MemoryKeyStore::failing("locked")), None).unwrap();
        assert!(outcome.keys_left);
        assert!(!path.exists());
        // Without a store to ask, the entry stays too.
        Vault::create(&path, None, Some(&store)).unwrap();
        assert!(reset(&path, None, None).unwrap().keys_left);
    }

    #[test]
    fn leftover_names_match_exactly() {
        for name in [
            "vault.kubach.tmp",
            "vault.kubach.corrupt-1",
            "vault.kubach.corrupt-1700000000",
            "vault.kubach.corrupt-1700000000-12",
            "vault.kubach.corrupt-1700000000.tmp",
            "vault.kubach.corrupt-1700000000-3.tmp",
        ] {
            assert!(is_leftover(VAULT_FILE, name), "{name}");
        }
        for name in [
            "vault.kubach",
            "vault.kubach.tmp.tmp",
            "vault.kubach.corrupt-",
            "vault.kubach.corrupt-.tmp",
            "vault.kubach.corrupt-1-",
            "vault.kubach.corrupt--1",
            "vault.kubach.corrupt-1-2-3",
            "vault.kubach.corrupt-1e5",
            "vault.kubach.corrupt-1/../x",
            "xvault.kubach.tmp",
            "vault.kubachX.tmp",
        ] {
            assert!(!is_leftover(VAULT_FILE, name), "{name}");
        }
    }

    #[test]
    fn default_path_is_in_the_config_dir() {
        let path = default_path().unwrap();
        assert_eq!(path.file_name().unwrap(), VAULT_FILE);
        assert_eq!(path.parent(), crate::settings::config_dir().as_deref());
    }
}
