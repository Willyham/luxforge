//! The macOS Keychain as the host's secret store.
use crate::secrets::SECRET_SERVICE;
use luxforge_core::{
    Error,
    capabilities::secrets::{SecretKey, SecretStore, SecretValue},
};
use security_framework::{
    base::Error as KeychainError,
    item::{ItemClass, ItemSearchOptions, Limit},
    passwords::{PasswordOptions, delete_generic_password, generic_password, set_generic_password},
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The native journey against the login keychain, under a service no module uses, cleaned up
    /// whatever happens. Opt in with `--ignored`; tests and evidence runs use the memory store.
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
        let key = SecretKey::new("luxforge.capabilities", Some("profile-test"), "api-key");
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
