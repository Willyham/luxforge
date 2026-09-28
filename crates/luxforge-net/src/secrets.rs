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
