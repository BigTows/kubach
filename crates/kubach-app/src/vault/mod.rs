//! The vault of imported kubeconfigs in the UI (ADR 0008): its state
//! (missing, locked, unlocked, damaged), unlocking off the UI thread with
//! a growing wait after wrong passwords, locking by hand, after idle time
//! and with the OS screen, and the dialogs to import, unlock and manage it.
//!
//! Everything slow (Argon2id, the OS keychain, which may show a system
//! prompt or wait on D-Bus, Touch ID / Windows Hello) runs in the background; meanwhile the state is
//! [`busy`](VaultState::is_busy) and refuses other changes.
//!
//! The decrypted configs live in [`VaultState`] only; nothing here writes
//! them anywhere but the encrypted file (export aside, which the user asks
//! for explicitly).

mod dialogs;
mod manual;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui_kit::*;
use kubach_core::secrets::ClearTrigger;
use kubach_core::vault::biometric::{BiometricStore, SystemBiometricStore};
use kubach_core::vault::keychain::{KeyStore, SystemKeyStore};
use kubach_core::vault::screen_lock::ScreenLock;
use kubach_core::vault::{
    self, Attempts, Availability, DerivedPassword, EntryInfo, Imported, KeyMode, LockedVault,
    UnlockMethod, Vault, VaultError,
};
use kubach_i18n::t;
use zeroize::Zeroizing;

pub use dialogs::{
    AfterUnlock, add_cluster, change_password, confirm_remove, confirm_reset, drop_zone, export,
    import_clipboard, import_file, prompt_unlock, rename, update_from_file,
};

use crate::settings::AppSettings;

/// How often the idle timer looks at the clock.
const IDLE_CHECK: Duration = Duration::from_secs(10);

/// How often the OS is asked whether the screen is locked, while the vault
/// is unlocked.
const SCREEN_CHECK: Duration = Duration::from_secs(2);

fn busy_error() -> SharedString {
    t!("vault-busy").into()
}

fn closing() -> SharedString {
    t!("vault-closing").into()
}

fn locked_error() -> SharedString {
    t!("vault-locked").into()
}

/// What the vault is doing in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Turning "Remember in the system keychain" on or off.
    Keychain,
    /// Turning Touch ID / Windows Hello unlock on or off.
    Biometric,
    /// Encrypting everything again with a new password.
    Password,
    /// Deleting the vault and its keychain entry.
    Reset,
}

/// What the vault file is like right now.
pub enum VaultStatus {
    /// No vault yet: created on the first import.
    Missing,
    Locked(LockedVault),
    Unlocked(Box<Vault>),
    /// Unreadable (damaged or from a newer Kubach); only a reset helps.
    Broken(SharedString),
    /// The OS has no config directory.
    Unavailable,
}

/// Whether an OS facility (credential store, biometrics) can be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    Checking,
    Available,
    Unavailable(SharedString),
}

/// The vault everyone reads; observe it to react to lock and unlock.
pub struct VaultState {
    path: Option<PathBuf>,
    status: VaultStatus,
    attempts: Attempts,
    /// No password attempt before this.
    retry_at: Option<Instant>,
    last_activity: Instant,
    keychain: Support,
    /// Touch ID / Windows Hello.
    biometric: Support,
    busy: Option<Busy>,
    /// Bumped whenever the imported configs may have changed (load, lock,
    /// unlock, edits); the keychain and Touch ID checks leave it alone, so
    /// observers can skip reloading contexts for them (#299).
    revision: u64,
    _idle: Task<()>,
    _screen: Task<()>,
    _keychain_check: Task<()>,
    _biometric_check: Task<()>,
}

struct GlobalVault(Entity<VaultState>);

impl Global for GlobalVault {}

/// Reads the vault file and makes the state global. Call after
/// [`settings::init`](crate::settings::init).
pub fn init(cx: &mut App) {
    let entity = cx.new(|cx| VaultState::new(vault::default_path(), cx));
    cx.set_global(GlobalVault(entity));
}

/// The shared vault, once [`init`] ran.
pub fn global(cx: &App) -> Option<Entity<VaultState>> {
    cx.try_global::<GlobalVault>()
        .map(|global| global.0.clone())
}

/// Imported configs as context sources (locked ones without config).
pub fn imported(cx: &App) -> Vec<Imported> {
    global(cx)
        .map(|vault| vault.read(cx).imported())
        .unwrap_or_default()
}

/// Records user input, postponing the idle lock. Cheap; call on any click
/// or key press.
pub fn touch(cx: &mut App) {
    if let Some(vault) = global(cx) {
        vault.update(cx, |vault, _| vault.last_activity = Instant::now());
    }
}

/// Locks the vault now.
pub fn lock(cx: &mut App) {
    if let Some(vault) = global(cx) {
        vault.update(cx, |vault, cx| vault.lock(cx));
    }
}

fn store() -> &'static dyn KeyStore {
    &SystemKeyStore
}

fn biometric_store() -> &'static dyn BiometricStore {
    &SystemBiometricStore
}

impl VaultState {
    fn new(path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let idle = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(IDLE_CHECK).await;
                if this.update(cx, |this, cx| this.lock_if_idle(cx)).is_err() {
                    break;
                }
            }
        });
        let screen = cx.spawn(async move |this, cx| {
            let mut screen = ScreenLock::new();
            loop {
                cx.background_executor().timer(SCREEN_CHECK).await;
                let Ok(watch) = this.update(cx, |this, cx| this.watches_screen(cx)) else {
                    break;
                };
                if !watch {
                    continue;
                }
                // Asked off the UI thread; the asker comes back for reuse.
                let locked;
                (screen, locked) = cx
                    .background_spawn(async move {
                        let locked = screen.is_locked().await;
                        (screen, locked)
                    })
                    .await;
                if locked == Some(true)
                    && this
                        .update(cx, |this, cx| {
                            if this.watches_screen(cx) {
                                this.lock(cx);
                            }
                        })
                        .is_err()
                {
                    break;
                }
            }
        });
        // Secret Service on Linux may be missing or slow to answer.
        let check = cx.background_spawn(async { SystemKeyStore::availability() });
        let keychain_check = cx.spawn(async move |this, cx| {
            let result = check.await;
            this.update(cx, |this, cx| {
                this.keychain = match result {
                    Ok(()) => Support::Available,
                    Err(error) => Support::Unavailable(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        });
        // Touch ID needs a signed build (an unsigned one gets -34018);
        // Windows asks the Hello service. Neither prompts.
        let check = cx.background_spawn(async { biometric_store().availability() });
        let biometric_check = cx.spawn(async move |this, cx| {
            let result = check.await;
            this.update(cx, |this, cx| {
                this.biometric = match result {
                    Ok(()) => Support::Available,
                    Err(error) => Support::Unavailable(error.to_string().into()),
                };
                cx.notify();
            })
            .ok();
        });
        let mut this = Self {
            path,
            status: VaultStatus::Missing,
            attempts: Attempts::default(),
            retry_at: None,
            last_activity: Instant::now(),
            keychain: Support::Checking,
            biometric: Support::Checking,
            busy: None,
            revision: 0,
            _idle: idle,
            _screen: screen,
            _keychain_check: keychain_check,
            _biometric_check: biometric_check,
        };
        this.reload(cx);
        this
    }

    /// Reads the file again unless it is unlocked (then memory is newer).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if matches!(self.status, VaultStatus::Unlocked(_)) {
            return;
        }
        self.status = match &self.path {
            None => VaultStatus::Unavailable,
            Some(path) => match LockedVault::read(path) {
                Ok(Some(locked)) => VaultStatus::Locked(locked),
                Ok(None) => VaultStatus::Missing,
                Err(error) => VaultStatus::Broken(error.message().to_string().into()),
            },
        };
        self.revision += 1;
        cx.notify();
    }

    /// Changes whenever [`imported`](Self::imported) may have changed.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn status(&self) -> &VaultStatus {
        &self.status
    }

    pub fn keychain(&self) -> &Support {
        &self.keychain
    }

    pub fn biometric(&self) -> &Support {
        &self.biometric
    }

    /// Which OS stores may be tried; still being checked counts as usable
    /// (a failure then falls back to the password).
    pub fn availability(&self) -> Availability {
        Availability {
            biometric: !matches!(self.biometric, Support::Unavailable(_)),
            keychain: !matches!(self.keychain, Support::Unavailable(_)),
        }
    }

    /// How a locked vault is opened first: biometrics, the keychain or the
    /// password form.
    pub fn first_unlock(&self) -> UnlockMethod {
        self.key_mode().map_or(UnlockMethod::Password, |mode| {
            mode.first(self.availability())
        })
    }

    pub fn is_unlocked(&self) -> bool {
        matches!(self.status, VaultStatus::Unlocked(_))
    }

    pub fn vault(&self) -> Option<&Vault> {
        match &self.status {
            VaultStatus::Unlocked(vault) => Some(vault),
            _ => None,
        }
    }

    /// Entries, readable whether locked or not.
    pub fn entries(&self) -> &[EntryInfo] {
        match &self.status {
            VaultStatus::Locked(locked) => locked.entries(),
            VaultStatus::Unlocked(vault) => vault.entries(),
            _ => &[],
        }
    }

    pub fn key_mode(&self) -> Option<KeyMode> {
        match &self.status {
            VaultStatus::Locked(locked) => Some(locked.key_mode()),
            VaultStatus::Unlocked(vault) => Some(vault.key_mode()),
            _ => None,
        }
    }

    pub fn imported(&self) -> Vec<Imported> {
        match &self.status {
            VaultStatus::Locked(locked) => locked.imported(),
            VaultStatus::Unlocked(vault) => vault.imported(),
            _ => Vec::new(),
        }
    }

    /// Drops the decrypted configs and the key (wiped from memory), and a
    /// copied Secret value still in the clipboard.
    pub fn lock(&mut self, cx: &mut Context<Self>) {
        if let VaultStatus::Unlocked(vault) = &self.status {
            self.status = VaultStatus::Locked(vault.locked());
            crate::secret_clipboard::clear(ClearTrigger::VaultLocked, cx);
            self.revision += 1;
            cx.notify();
        }
    }

    /// Whether the screen-lock watcher should ask the OS now: unlocked and
    /// the setting on.
    fn watches_screen(&self, cx: &App) -> bool {
        self.is_unlocked()
            && AppSettings::try_global(cx)
                .is_none_or(|settings| settings.read(cx).get().clusters.vault_lock_on_screen_lock)
    }

    /// What runs in the background, if anything.
    pub fn busy(&self) -> Option<Busy> {
        self.busy
    }

    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    fn lock_if_idle(&mut self, cx: &mut Context<Self>) {
        let lock_after = AppSettings::try_global(cx)
            .and_then(|settings| settings.read(cx).get().clusters.vault_lock_after());
        if let Some(after) = lock_after
            && self.is_unlocked()
            && self.last_activity.elapsed() >= after
        {
            self.lock(cx);
        }
    }

    /// Wait left before the next password attempt.
    pub fn retry_in(&self) -> Option<Duration> {
        let at = self.retry_at?;
        let now = Instant::now();
        (at > now).then(|| at - now)
    }

    fn unlocked(&mut self, vault: Vault, cx: &mut Context<Self>) {
        self.attempts.succeeded();
        self.retry_at = None;
        self.last_activity = Instant::now();
        self.status = VaultStatus::Unlocked(Box::new(vault));
        self.revision += 1;
        cx.notify();
    }

    fn failed(&mut self, error: VaultError, cx: &mut Context<Self>) -> SharedString {
        match error {
            VaultError::WrongPassword => {
                self.attempts.failed();
                let delay = self.attempts.delay();
                self.retry_at = (!delay.is_zero()).then(|| Instant::now() + delay);
                match delay.as_secs() {
                    0 => t!("vault-error-wrong-password").into(),
                    secs => t!("vault-wrong-password-wait", seconds = secs).into(),
                }
            }
            VaultError::Corrupted { .. } | VaultError::UnsupportedVersion(_) => {
                let message: SharedString = error.message().to_string().into();
                self.status = VaultStatus::Broken(message.clone());
                self.revision += 1;
                cx.notify();
                message
            }
            error => error.message().to_string().into(),
        }
    }

    /// Unlocks with `password`, or without one when `None` (Touch ID /
    /// Windows Hello, then the keychain). Key derivation and the system
    /// prompts run in the background.
    pub fn unlock(
        &mut self,
        password: Option<Zeroizing<String>>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        if let Some(wait) = self.retry_in() {
            let message = t!("vault-too-many-attempts", seconds = wait.as_secs() + 1);
            return Task::ready(Err(message.into()));
        }
        let locked = match &self.status {
            VaultStatus::Locked(locked) => locked.clone(),
            VaultStatus::Unlocked(_) => return Task::ready(Ok(())),
            VaultStatus::Broken(error) => return Task::ready(Err(error.clone())),
            VaultStatus::Missing | VaultStatus::Unavailable => {
                return Task::ready(Err(t!("vault-missing").into()));
            }
        };
        let available = self.availability();
        let work = cx.background_spawn(async move {
            match password {
                Some(password) => locked.unlock_with_password(&password),
                None => locked.unlock_without_password(
                    available.biometric.then(biometric_store),
                    available.keychain.then(store),
                ),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| match result {
                Ok(vault) => {
                    this.unlocked(vault, cx);
                    Ok(())
                }
                Err(error) => Err(this.failed(error, cx)),
            })
            .unwrap_or_else(|_| Err(closing()))
        })
    }

    /// Creates the vault (first import) protected by `password` and/or the
    /// keychain; it is unlocked afterwards.
    pub fn create(
        &mut self,
        password: Option<Zeroizing<String>>,
        keychain: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        let Some(path) = self.path.clone() else {
            return Task::ready(Err(t!("vault-no-settings-folder").into()));
        };
        let work = cx.background_spawn(async move {
            let derived = password
                .map(|password| DerivedPassword::derive(&password, Default::default()))
                .transpose()?;
            Vault::create(&path, derived.as_ref(), keychain.then(store))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| match result {
                Ok(vault) => {
                    this.unlocked(vault, cx);
                    Ok(())
                }
                Err(error) => Err(SharedString::from(error.message().to_string())),
            })
            .unwrap_or_else(|_| Err(closing()))
        })
    }

    /// Runs `edit` on the unlocked vault and notifies observers. Refused
    /// while a background change runs, so it can't be overwritten.
    pub fn edit<T>(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Vault) -> Result<T, VaultError>,
    ) -> Result<T, SharedString> {
        if self.is_busy() {
            return Err(busy_error());
        }
        let VaultStatus::Unlocked(vault) = &mut self.status else {
            return Err(locked_error());
        };
        let result = edit(vault).map_err(|error| SharedString::from(error.message().to_string()));
        self.revision += 1;
        cx.notify();
        result
    }

    /// Runs `edit` on a copy of the unlocked vault in the background (the
    /// OS keychain may prompt or wait on D-Bus), then takes the copy. The
    /// vault is [`busy`](Self::busy) meanwhile; core leaves the file as it
    /// was when `edit` fails.
    fn edit_in_background(
        &mut self,
        busy: Busy,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut Vault) -> Result<(), VaultError> + Send + 'static,
    ) -> Task<Result<(), SharedString>> {
        if self.is_busy() {
            return Task::ready(Err(busy_error()));
        }
        let VaultStatus::Unlocked(vault) = &self.status else {
            return Task::ready(Err(locked_error()));
        };
        let mut copy = vault.as_ref().clone();
        self.busy = Some(busy);
        cx.notify();
        let work = cx.background_spawn(async move { edit(&mut copy).map(|()| copy) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.revision += 1;
                cx.notify();
                let vault =
                    result.map_err(|error| SharedString::from(error.message().to_string()))?;
                if this.is_unlocked() {
                    this.status = VaultStatus::Unlocked(Box::new(vault));
                } else {
                    // Locked meanwhile: the file is newer than the copy
                    // kept by the lock. `vault` is dropped (wiped).
                    this.reload(cx);
                }
                Ok(())
            })
            .unwrap_or_else(|_| Err(closing()))
        })
    }

    /// Sets a new password: Argon2id and the keychain copy (if any) in the
    /// background.
    pub fn change_password(
        &mut self,
        password: Zeroizing<String>,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        self.edit_in_background(Busy::Password, cx, move |vault| {
            let derived = DerivedPassword::derive(&password, Default::default())?;
            vault.change_password(&derived, Some(store()), Some(biometric_store()))
        })
    }

    /// Keeps the key in the system keychain, or stops doing so, in the
    /// background.
    pub fn set_keychain(
        &mut self,
        on: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        self.edit_in_background(Busy::Keychain, cx, move |vault| {
            if on {
                vault.enable_keychain(store())
            } else {
                vault.disable_keychain(store())
            }
        })
    }

    /// Turns Touch ID / Windows Hello unlock on or off in the background
    /// (Windows Hello asks to confirm when turned on).
    pub fn set_biometric(
        &mut self,
        on: bool,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), SharedString>> {
        self.edit_in_background(Busy::Biometric, cx, move |vault| {
            if on {
                vault.enable_biometric(biometric_store())
            } else {
                vault.disable_biometric(biometric_store())
            }
        })
    }

    /// Deletes the vault, its backups, keychain entry and biometric key in
    /// the background. `Ok(Some(_))` warns that a key may be left in the
    /// OS store.
    pub fn reset(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<Option<SharedString>, SharedString>> {
        if self.is_busy() {
            return Task::ready(Err(busy_error()));
        }
        let Some(path) = self.path.clone() else {
            return Task::ready(Ok(None));
        };
        self.busy = Some(Busy::Reset);
        cx.notify();
        let work = cx.background_spawn(async move {
            vault::reset(&path, Some(store()), Some(biometric_store()))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.busy = None;
                this.revision += 1;
                cx.notify();
                let outcome =
                    result.map_err(|error| SharedString::from(error.message().to_string()))?;
                this.status = VaultStatus::Missing;
                this.attempts = Attempts::default();
                this.retry_at = None;
                Ok(outcome.warning().map(|warning| warning.to_string().into()))
            })
            .unwrap_or_else(|_| Err(closing()))
        })
    }
}

/// Turns "Remember in the system keychain" on or off; failures show as a
/// notification.
pub fn set_keychain(on: bool, window: &mut Window, cx: &mut App) {
    let Some(vault) = global(cx) else {
        return;
    };
    let task = vault.update(cx, |vault, cx| vault.set_keychain(on, cx));
    notify_failure(task, window, cx);
}

/// Turns Touch ID / Windows Hello unlock on or off; failures show as a
/// notification.
pub fn set_biometric(on: bool, window: &mut Window, cx: &mut App) {
    let Some(vault) = global(cx) else {
        return;
    };
    let task = vault.update(cx, |vault, cx| vault.set_biometric(on, cx));
    notify_failure(task, window, cx);
}

fn notify_failure(task: Task<Result<(), SharedString>>, window: &mut Window, cx: &mut App) {
    window
        .spawn(cx, async move |cx| {
            if let Err(error) = task.await {
                cx.update(|window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    use gpui_kit::component::notification::Notification;
                    window.push_notification(Notification::error(error), cx);
                })
                .ok();
            }
        })
        .detach();
}
