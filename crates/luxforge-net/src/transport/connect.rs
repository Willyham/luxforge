//! Resolution and connection. A request resolves its host exactly once, requires every answer to
//! belong to the endpoint's class and connects only to an address it checked, so no second lookup
//! can rebind the name between the check and the connection.
use super::address::address_allowed;
use luxforge_core::{
    Error,
    capabilities::endpoint::{Endpoint, EndpointClass},
};
use std::{
    io,
    net::{SocketAddr, TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};
use url::Host;

/// Turns a host name into the addresses it names.
pub trait Resolve: Send + Sync {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

/// The operating system's resolver. A lookup blocks for as long as the system resolver takes; the
/// request's deadlines are checked when it returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port).to_socket_addrs()?.collect())
    }
}

/// Opens a TCP connection to one already checked address.
pub trait Connect: Send + Sync {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream>;
}

/// A direct TCP connection. No proxy is ever consulted.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemConnector;

impl Connect for SystemConnector {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        TcpStream::connect_timeout(&address, timeout)
    }
}

fn not_in_class(address: SocketAddr, class: EndpointClass) -> Error {
    Error::validation(format!(
        "{} is not a {} address",
        address.ip(),
        class.label()
    ))
}

/// Resolve `endpoint` once and check every address against its class. An IP-literal host is
/// checked the same way without a lookup.
pub(super) fn resolve(
    endpoint: &Endpoint,
    resolver: &dyn Resolve,
) -> Result<Vec<SocketAddr>, Error> {
    let url = &endpoint.url;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Error::validation("URL has no port"))?;
    let name = url.host_str().unwrap_or_default();
    let (addresses, looked_up) = match url.host() {
        Some(Host::Domain(domain)) => (
            resolver.resolve(domain, port).map_err(|error| {
                Error::file_access(format!("cannot resolve {name}: {}", error.kind()))
            })?,
            true,
        ),
        Some(Host::Ipv4(address)) => (vec![SocketAddr::new(address.into(), port)], false),
        Some(Host::Ipv6(address)) => (vec![SocketAddr::new(address.into(), port)], false),
        None => return Err(Error::validation("URL has no host")),
    };
    if addresses.is_empty() {
        return Err(Error::file_access(format!(
            "{name} did not resolve to any address"
        )));
    }
    let class = endpoint.class;
    if let Some(&address) = addresses
        .iter()
        .find(|address| !address_allowed(address.ip(), class))
    {
        return Err(if looked_up {
            Error::validation(format!(
                "{name} resolved to {}, which is not a {} address",
                address.ip(),
                class.label()
            ))
        } else {
            not_in_class(address, class)
        });
    }
    Ok(addresses)
}

/// Connect to the first of `addresses` that accepts, checking each against `class` again,
/// spending at most `timeout` on each and never passing `deadline`.
pub(super) fn connect(
    addresses: &[SocketAddr],
    class: EndpointClass,
    name: &str,
    connector: &dyn Connect,
    timeout: Duration,
    deadline: Instant,
) -> Result<TcpStream, Error> {
    let timed_out = || Error::file_access(format!("connecting to {name} timed out"));
    let mut last = io::ErrorKind::NotConnected;
    for &address in addresses {
        if !address_allowed(address.ip(), class) {
            return Err(not_in_class(address, class));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timed_out());
        }
        match connector.connect(address, timeout.min(remaining)) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error.kind(),
        }
    }
    Err(if last == io::ErrorKind::TimedOut {
        timed_out()
    } else {
        Error::file_access(format!("cannot connect to {name}: {last}"))
    })
}
