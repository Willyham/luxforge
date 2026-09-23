//! TLS for `https` endpoints: rustls with the ring provider, TLS 1.3 and 1.2, SNI from the URL's
//! host, and either the operating system's verifier or an explicit set of roots.
use super::policy::Endpoint;
use crate::{Error, ErrorKind};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, StreamOwned,
    pki_types::{CertificateDer, ServerName},
};
use rustls_platform_verifier::BuilderVerifierExt;
use std::{
    net::{IpAddr, TcpStream},
    sync::Arc,
};
use url::Host;

/// Which certificates an `https` endpoint must chain to.
#[derive(Clone, Debug)]
pub enum TlsTrust {
    /// The operating system's trust store, evaluated by its own verifier.
    Platform,
    /// Exactly these roots and no others. For tests and fixtures.
    Roots(Vec<CertificateDer<'static>>),
}

fn setup_failed(error: rustls::Error) -> Error {
    Error::new(
        ErrorKind::FileAccess,
        format!("cannot configure TLS: {error}"),
    )
}

/// The client configuration every `https` request of one transport shares.
pub(super) fn client_config(trust: &TlsTrust) -> Result<Arc<ClientConfig>, Error> {
    let builder =
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(setup_failed)?;
    let mut config = match trust {
        TlsTrust::Platform => builder
            .with_platform_verifier()
            .map_err(setup_failed)?
            .with_no_client_auth(),
        TlsTrust::Roots(roots) => {
            let mut store = RootCertStore::empty();
            for root in roots {
                store.add(root.clone()).map_err(setup_failed)?;
            }
            builder.with_root_certificates(store).with_no_client_auth()
        }
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The name the server's certificate must match, and the SNI for a domain: `endpoint`'s host.
pub(super) fn server_name(endpoint: &Endpoint) -> Result<ServerName<'static>, Error> {
    Ok(match endpoint.url.host() {
        Some(Host::Domain(name)) => ServerName::try_from(name.to_owned()).map_err(|_| {
            Error::new(
                ErrorKind::Validation,
                format!("{name} is not a valid TLS server name"),
            )
        })?,
        Some(Host::Ipv4(address)) => ServerName::from(IpAddr::V4(address)),
        Some(Host::Ipv6(address)) => ServerName::from(IpAddr::V6(address)),
        None => return Err(Error::new(ErrorKind::Validation, "URL has no host")),
    })
}

/// Wrap a connected socket in a client session for `name`. The handshake happens on the first
/// write, under the request's deadlines.
pub(super) fn wrap(
    config: &Arc<ClientConfig>,
    name: ServerName<'static>,
    socket: TcpStream,
) -> Result<StreamOwned<ClientConnection, TcpStream>, Error> {
    let session = ClientConnection::new(Arc::clone(config), name).map_err(setup_failed)?;
    Ok(StreamOwned::new(session, socket))
}
