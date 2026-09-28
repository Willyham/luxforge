//! The host's network transport and secure secret store, which the desktop and `luxforge-json`
//! give the catalog owner through `HostConfig`: [`HttpTransport`], TLS on rustls with the platform
//! verifier and HTTP/1.1 on `ureq`'s agent behind the address policy, and the macOS Keychain
//! ([`platform_secret_store`]). The core defines what a transport and a secret store must do
//! (`luxforge_core::capabilities::{transport, secrets}`) and links neither, so its own tests and
//! every build that does not ship them compile no TLS, HTTP or Keychain code. See
//! `docs/design/module-capabilities.md#transport`.
mod address;
mod agent;
mod connect;
#[cfg(target_os = "macos")]
mod keychain;
mod secrets;
#[cfg(test)]
mod tests;
mod tls;
mod transport;

pub use connect::{Connect, Resolve, SystemConnector, SystemResolver};
pub use rustls::pki_types::CertificateDer;
#[cfg(target_os = "macos")]
pub use secrets::KeychainSecretStore;
pub use secrets::{SECRET_SERVICE, platform_secret_store};
pub use tls::TlsTrust;
pub use transport::{HttpTransport, TransportConfig};
