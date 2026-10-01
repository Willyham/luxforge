//! The secure store this platform provides for module secrets: the macOS Keychain, and nothing yet
//! elsewhere. The store's contract, and the in-memory store tests and evidence runs use, are the
//! core's (`luxforge_core::capabilities::secrets`).
use luxforge_core::capabilities::secrets::SecretStore;
#[cfg(not(target_os = "macos"))]
use luxforge_core::capabilities::secrets::UnavailableSecretStore;
use std::sync::Arc;

#[cfg(target_os = "macos")]
pub use crate::keychain::KeychainSecretStore;

/// The keychain service every module secret is stored under.
pub const SECRET_SERVICE: &str = "app.luxforge.module-secret";

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

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::{
        ErrorKind,
        capabilities::secrets::{SecretKey, SecretValue},
    };

    #[test]
    fn unsupported_platform_refuses_every_secret_operation() {
        // Exercise the cross-crate fallback even on macOS, without touching the Keychain.
        #[cfg(target_os = "macos")]
        let store = luxforge_core::capabilities::secrets::UnavailableSecretStore::new(
            "secure storage is not implemented on this platform",
        );
        #[cfg(not(target_os = "macos"))]
        let store = platform_secret_store();
        let key = SecretKey::new("test.module", None, "api-key");
        let value = SecretValue::new("sentinel-secret".into());
        assert_eq!(store.name(), "no secure store");
        for error in [
            store.set(&key, &value).unwrap_err(),
            store.clear(&key).unwrap_err(),
            store.present(&key).unwrap_err(),
            store.read(&key).unwrap_err(),
        ] {
            assert_eq!(error.kind, ErrorKind::NotReady);
            assert_eq!(
                error.detail,
                "secure storage is not implemented on this platform"
            );
        }
    }
}
