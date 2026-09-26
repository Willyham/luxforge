//! Module secrets live only in a secure store the host owns. Nothing else holds one: a descriptor
//! declares a secret field's presence, a read reports whether it is set, and only the module worker
//! reads the value, as a [`SecretValue`] that cannot be serialized, prints as redacted and is
//! zeroed when dropped. A locked, missing or unsupported store fails with `not-ready` naming the
//! store; there is no plaintext fallback. See `docs/design/module-capabilities.md#secrets-and-redaction`.
use crate::Error;
#[cfg(test)]
use crate::ErrorKind;
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use zeroize::Zeroizing;

/// The keychain service every module secret is stored under.
pub const SECRET_SERVICE: &str = "app.lightwell.module-secret";

/// Which secret: one secret field of a module, or of one of its provider profiles.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretKey {
    pub module_id: String,
    pub profile_id: Option<String>,
    pub setting_id: String,
}

impl SecretKey {
    pub fn new(module_id: &str, profile_id: Option<&str>, setting_id: &str) -> Self {
        Self {
            module_id: module_id.to_owned(),
            profile_id: profile_id.map(str::to_owned),
            setting_id: setting_id.to_owned(),
        }
    }

    /// `<module_id>/<profile_id or ->/<setting_id>`: the store's account name. Module, profile and
    /// setting identities never contain `/`, and `-` is never a profile identity, so the account
    /// names exactly one secret.
    pub fn account(&self) -> String {
        format!(
            "{}/{}/{}",
            self.module_id,
            self.profile_id.as_deref().unwrap_or("-"),
            self.setting_id
        )
    }
}

/// One secret's value. It is deliberately not `Serialize` and not `Clone`, its `Debug` prints
/// `SecretValue(<redacted>)`, and its buffer is zeroed when it is dropped, so a value can only
/// reach an observable surface by someone calling [`SecretValue::expose`] on purpose.
pub struct SecretValue(Zeroizing<String>);

impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// A value read back from a store as bytes. The bytes are zeroed whether or not they are UTF-8.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, Error> {
        let bytes = Zeroizing::new(bytes);
        match std::str::from_utf8(&bytes) {
            Ok(text) => Ok(Self::new(text.to_owned())),
            Err(_) => Err(Error::validation("a stored secret is not UTF-8 text")),
        }
    }

    /// The value itself, for the one place that sends or stores it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The value's length in characters, which the caller checks against the field's limit.
    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

/// A host secure store. Every call is short: `present` reads attributes only and never the secret's
/// data, and `clear` of an absent secret succeeds, because both reduce what is held.
pub trait SecretStore: Send + Sync {
    /// The store's name as a person knows it, used in every `not-ready` message.
    fn name(&self) -> &str;
    fn set(&self, key: &SecretKey, value: &SecretValue) -> Result<(), Error>;
    /// Remove one secret. An absent secret is not an error.
    fn clear(&self, key: &SecretKey) -> Result<(), Error>;
    /// Whether the secret is set, without reading its value.
    fn present(&self, key: &SecretKey) -> Result<bool, Error>;
    /// The value, for the module worker only. `None` when absent.
    fn read(&self, key: &SecretKey) -> Result<Option<SecretValue>, Error>;
}

/// The store this platform supports: the macOS Keychain, and nothing yet elsewhere.
pub fn platform_secret_store() -> Arc<dyn SecretStore> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(KeychainSecretStore::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(UnavailableSecretStore::new(
            "secure storage is not implemented on this platform",
        ))
    }
}

/// A store that refuses every call with `not-ready: <reason>`: an unsupported platform, or a host
/// configured without a secure store.
#[derive(Clone, Debug)]
pub struct UnavailableSecretStore {
    reason: String,
}

impl UnavailableSecretStore {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn refuse<T>(&self) -> Result<T, Error> {
        Err(Error::not_ready(self.reason.clone()))
    }
}

impl SecretStore for UnavailableSecretStore {
    fn name(&self) -> &str {
        "no secure store"
    }
    fn set(&self, _: &SecretKey, _: &SecretValue) -> Result<(), Error> {
        self.refuse()
    }
    fn clear(&self, _: &SecretKey) -> Result<(), Error> {
        self.refuse()
    }
    fn present(&self, _: &SecretKey) -> Result<bool, Error> {
        self.refuse()
    }
    fn read(&self, _: &SecretKey) -> Result<Option<SecretValue>, Error> {
        self.refuse()
    }
}

/// How many times each operation of a [`MemorySecretStore`] was called, including failed calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SecretCalls {
    pub set: usize,
    pub clear: usize,
    pub present: usize,
    pub read: usize,
}

impl SecretCalls {
    pub fn total(&self) -> usize {
        self.set + self.clear + self.present + self.read
    }
}

/// An in-process store for tests and evidence runs, which must never touch the login keychain. It
/// counts every call, so a test can prove a path made none, and it can be made to fail every call
/// with a chosen error, such as the `not-ready` a locked keychain reports.
#[derive(Default)]
pub struct MemorySecretStore {
    values: Mutex<HashMap<SecretKey, SecretValue>>,
    fault: Mutex<Option<Error>>,
    set: AtomicUsize,
    clear: AtomicUsize,
    present: AtomicUsize,
    read: AtomicUsize,
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fail every later call with this error until it is cleared with `None`.
    pub fn fail_with(&self, fault: Option<Error>) {
        *self.fault.lock().expect("secret store fault") = fault;
    }

    pub fn calls(&self) -> SecretCalls {
        SecretCalls {
            set: self.set.load(Ordering::Relaxed),
            clear: self.clear.load(Ordering::Relaxed),
            present: self.present.load(Ordering::Relaxed),
            read: self.read.load(Ordering::Relaxed),
        }
    }

    /// How many secrets the store holds.
    pub fn len(&self) -> usize {
        self.values.lock().expect("secret store").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn call(&self, counter: &AtomicUsize) -> Result<(), Error> {
        counter.fetch_add(1, Ordering::Relaxed);
        match &*self.fault.lock().expect("secret store fault") {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

impl SecretStore for MemorySecretStore {
    fn name(&self) -> &str {
        "in-memory secret store"
    }
    fn set(&self, key: &SecretKey, value: &SecretValue) -> Result<(), Error> {
        self.call(&self.set)?;
        self.values
            .lock()
            .expect("secret store")
            .insert(key.clone(), SecretValue::new(value.expose().to_owned()));
        Ok(())
    }
    fn clear(&self, key: &SecretKey) -> Result<(), Error> {
        self.call(&self.clear)?;
        self.values.lock().expect("secret store").remove(key);
        Ok(())
    }
    fn present(&self, key: &SecretKey) -> Result<bool, Error> {
        self.call(&self.present)?;
        Ok(self.values.lock().expect("secret store").contains_key(key))
    }
    fn read(&self, key: &SecretKey) -> Result<Option<SecretValue>, Error> {
        self.call(&self.read)?;
        Ok(self
            .values
            .lock()
            .expect("secret store")
            .get(key)
            .map(|value| SecretValue::new(value.expose().to_owned())))
    }
}

#[cfg(target_os = "macos")]
pub use keychain::KeychainSecretStore;

#[cfg(target_os = "macos")]
mod keychain {
    use super::{SECRET_SERVICE, SecretKey, SecretStore, SecretValue};
    use crate::Error;
    use security_framework::{
        base::Error as KeychainError,
        item::{ItemClass, ItemSearchOptions, Limit},
        passwords::{
            PasswordOptions, delete_generic_password, generic_password, set_generic_password,
        },
    };

    /// `errSecItemNotFound`: the item is absent, which is an answer rather than a failure.
    const ITEM_NOT_FOUND: i32 = -25300;
    /// `errSecInteractionNotAllowed`: the keychain is locked and may not ask to be unlocked.
    const INTERACTION_NOT_ALLOWED: i32 = -25308;
    /// `errSecAuthFailed`: the person declined, or the keychain refused this executable.
    const AUTH_FAILED: i32 = -25293;
    /// `errSecUserCanceled`: the person cancelled the OS prompt.
    const USER_CANCELED: i32 = -128;
    /// `errSecNoSuchKeychain`: there is no default keychain to use.
    const NO_SUCH_KEYCHAIN: i32 = -25294;

    /// Generic passwords in the login keychain, one per secret, under a service name and the
    /// secret's account. `present` asks for attributes only, so it never needs the item's data and
    /// never prompts. The OS may prompt when a rebuilt executable reads or replaces an item an
    /// earlier build stored; that prompt is the OS's, and the call waits on it.
    #[derive(Clone, Debug)]
    pub struct KeychainSecretStore {
        service: String,
    }

    impl Default for KeychainSecretStore {
        fn default() -> Self {
            Self::new()
        }
    }

    impl KeychainSecretStore {
        pub fn new() -> Self {
            Self::with_service(SECRET_SERVICE)
        }

        /// A store under another service name, so the native journey test never touches a real
        /// module's items.
        pub fn with_service(service: impl Into<String>) -> Self {
            Self {
                service: service.into(),
            }
        }

        pub fn service(&self) -> &str {
            &self.service
        }

        fn failure(&self, error: KeychainError) -> Error {
            let reason = match error.code() {
                INTERACTION_NOT_ALLOWED => "is locked".to_owned(),
                AUTH_FAILED | USER_CANCELED => "refused access".to_owned(),
                NO_SUCH_KEYCHAIN => "has no default keychain".to_owned(),
                code => format!(
                    "failed: {} (OSStatus {code})",
                    error
                        .message()
                        .unwrap_or_else(|| "unknown error".to_owned())
                ),
            };
            Error::not_ready(format!("the macOS Keychain {reason}"))
        }
    }

    impl SecretStore for KeychainSecretStore {
        fn name(&self) -> &str {
            "the macOS Keychain"
        }

        fn set(&self, key: &SecretKey, value: &SecretValue) -> Result<(), Error> {
            set_generic_password(&self.service, &key.account(), value.expose().as_bytes())
                .map_err(|error| self.failure(error))
        }

        fn clear(&self, key: &SecretKey) -> Result<(), Error> {
            match delete_generic_password(&self.service, &key.account()) {
                Ok(()) => Ok(()),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(()),
                Err(error) => Err(self.failure(error)),
            }
        }

        fn present(&self, key: &SecretKey) -> Result<bool, Error> {
            let found = ItemSearchOptions::new()
                .class(ItemClass::generic_password())
                .service(&self.service)
                .account(&key.account())
                .load_attributes(true)
                .limit(Limit::Max(1))
                .search();
            match found {
                Ok(items) => Ok(!items.is_empty()),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(false),
                Err(error) => Err(self.failure(error)),
            }
        }

        fn read(&self, key: &SecretKey) -> Result<Option<SecretValue>, Error> {
            match generic_password(PasswordOptions::new_generic_password(
                &self.service,
                &key.account(),
            )) {
                Ok(bytes) => SecretValue::from_bytes(bytes).map(Some),
                Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
                Err(error) => Err(self.failure(error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(profile: Option<&str>) -> SecretKey {
        SecretKey::new("test.module", profile, "api-key")
    }

    #[test]
    fn a_secret_key_names_one_account_per_module_profile_and_setting() {
        assert_eq!(key(None).account(), "test.module/-/api-key");
        assert_eq!(
            key(Some("profile-0a1b")).account(),
            "test.module/profile-0a1b/api-key"
        );
    }

    #[test]
    fn a_secret_value_never_prints_itself() {
        let value = SecretValue::new("hunter2".into());
        assert_eq!(format!("{value:?}"), "SecretValue(<redacted>)");
        assert_eq!(value.expose(), "hunter2");
        assert_eq!(value.chars(), 7);
        assert!(SecretValue::from_bytes(vec![0xff, 0xfe]).is_err());
        assert_eq!(
            SecretValue::from_bytes(b"abc".to_vec()).unwrap().expose(),
            "abc"
        );
    }

    #[test]
    fn the_memory_store_sets_reads_clears_counts_and_fails_on_request() {
        let store = MemorySecretStore::new();
        assert!(!store.present(&key(None)).unwrap());
        store
            .set(&key(None), &SecretValue::new("one".into()))
            .unwrap();
        assert!(store.present(&key(None)).unwrap());
        assert!(!store.present(&key(Some("profile-1"))).unwrap());
        assert_eq!(store.read(&key(None)).unwrap().unwrap().expose(), "one");
        store.clear(&key(None)).unwrap();
        store.clear(&key(None)).unwrap();
        assert!(store.read(&key(None)).unwrap().is_none());
        store.fail_with(Some(Error::not_ready("locked")));
        let error = store.present(&key(None)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotReady);
        store.fail_with(None);
        assert_eq!(
            store.calls(),
            SecretCalls {
                set: 1,
                clear: 2,
                present: 4,
                read: 2,
            }
        );
        assert!(store.is_empty());
    }

    #[test]
    fn an_unavailable_store_refuses_every_call_as_not_ready() {
        let store = UnavailableSecretStore::new("no secure store is configured");
        let value = SecretValue::new("x".into());
        for error in [
            store.set(&key(None), &value).unwrap_err(),
            store.clear(&key(None)).unwrap_err(),
            store.present(&key(None)).unwrap_err(),
            store.read(&key(None)).map(|_| ()).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::NotReady);
            assert_eq!(error.detail, "no secure store is configured");
        }
    }

    /// The native journey against the login keychain, under a service no module uses, cleaned up
    /// whatever happens. Opt in with `--ignored`; tests and evidence runs use the memory store.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "touches the login keychain"]
    fn keychain_sets_reports_presence_reads_and_clears_a_secret() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let store = KeychainSecretStore::with_service(format!(
            "{SECRET_SERVICE}.test-{}-{nanos}",
            std::process::id()
        ));
        let key = SecretKey::new("lightwell.capabilities", Some("profile-test"), "api-key");
        struct Cleanup<'a>(&'a KeychainSecretStore, &'a SecretKey);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.clear(self.1);
            }
        }
        let _cleanup = Cleanup(&store, &key);
        println!("service {}", store.service());
        println!("present before set: {:?}", store.present(&key));
        assert!(!store.present(&key).unwrap());
        store
            .set(&key, &SecretValue::new("native-journey-value".into()))
            .unwrap();
        let present = store.present(&key);
        println!("present after set: {present:?}");
        assert!(present.unwrap());
        let read = store.read(&key).unwrap().expect("the secret was stored");
        println!("read after set: {read:?}, {} characters", read.chars());
        assert_eq!(read.expose(), "native-journey-value");
        store
            .set(&key, &SecretValue::new("replaced".into()))
            .unwrap();
        assert_eq!(store.read(&key).unwrap().unwrap().expose(), "replaced");
        store.clear(&key).unwrap();
        let present = store.present(&key);
        println!("present after clear: {present:?}");
        assert!(!present.unwrap());
        assert!(store.read(&key).unwrap().is_none());
        store.clear(&key).unwrap();
        println!("clear of an absent secret: ok");
    }
}
