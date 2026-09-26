//! Resolution and connection. A request resolves its host exactly once, requires every answer to
//! belong to the endpoint's class and connects only to an address it checked, so no second lookup
//! can rebind the name between the check and the connection.
use super::policy::{Endpoint, address_allowed};
use crate::Error;
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

fn timed_out(name: &str) -> Error {
    Error::file_access(format!("connecting to {name} timed out"))
}

/// Resolve `endpoint` once, check every address against its class and connect to the first
/// checked address that accepts, spending at most `timeout` on each and never passing `deadline`.
/// An IP-literal host is checked the same way without a lookup.
pub(super) fn connect(
    endpoint: &Endpoint,
    resolver: &dyn Resolve,
    connector: &dyn Connect,
    timeout: Duration,
    deadline: Instant,
) -> Result<TcpStream, Error> {
    let url = &endpoint.url;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| Error::validation("URL has no port"))?;
    let name = url.host_str().unwrap_or_default();
    let addresses = match url.host() {
        Some(Host::Domain(domain)) => resolver.resolve(domain, port).map_err(|error| {
            Error::file_access(format!("cannot resolve {name}: {}", error.kind()))
        })?,
        Some(Host::Ipv4(address)) => vec![SocketAddr::new(address.into(), port)],
        Some(Host::Ipv6(address)) => vec![SocketAddr::new(address.into(), port)],
        None => return Err(Error::validation("URL has no host")),
    };
    if addresses.is_empty() {
        return Err(Error::file_access(format!(
            "{name} did not resolve to any address"
        )));
    }
    if let Some(address) = addresses
        .iter()
        .map(SocketAddr::ip)
        .find(|&address| !address_allowed(address, endpoint.class))
    {
        let class = endpoint.class.label();
        return Err(Error::validation(match url.host() {
            Some(Host::Domain(_)) => {
                format!("{name} resolved to {address}, which is not a {class} address")
            }
            _ => format!("{address} is not a {class} address"),
        }));
    }
    let mut last = io::ErrorKind::NotConnected;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(timed_out(name));
        }
        match connector.connect(address, timeout.min(remaining)) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error.kind(),
        }
    }
    Err(if last == io::ErrorKind::TimedOut {
        timed_out(name)
    } else {
        Error::file_access(format!("cannot connect to {name}: {last}"))
    })
}
