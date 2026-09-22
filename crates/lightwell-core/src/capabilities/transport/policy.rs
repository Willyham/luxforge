//! Which URLs an endpoint may name, before any address is resolved. Resolution and the address
//! class checks happen when a request connects, so a name cannot rebind between check and use.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use url::{Host, Url};

fn refused(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

/// The two kinds of destination the host will contact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndpointClass {
    /// A remote service: HTTPS only, and every resolved address must be public.
    Remote,
    /// A service on this machine: `http` or `https` to `localhost`, `127.0.0.0/8` or `::1`, and
    /// every resolved address must be loopback. Labelled separately wherever it is shown.
    Loopback,
}

/// A syntactically accepted endpoint and the class it was accepted as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub url: Url,
    pub class: EndpointClass,
}

impl Endpoint {
    /// `scheme://host:port`, the unit a grant is scoped to.
    pub fn origin(&self) -> String {
        self.url.origin().ascii_serialization()
    }
}

/// Parse `text` as an endpoint of one of the `allowed` classes. The scheme must be `http` or
/// `https`; userinfo, fragments and every other scheme are refused; a loopback host is the loopback
/// class and anything else is remote, which requires `https`.
pub fn parse_endpoint(text: &str, allowed: &[EndpointClass]) -> Result<Endpoint, Error> {
    let url = Url::parse(text.trim()).map_err(|error| refused(format!("invalid URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(refused(format!("scheme {} is not allowed", url.scheme())));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(refused("URLs with credentials are not allowed"));
    }
    if url.fragment().is_some() {
        return Err(refused("URLs with fragments are not allowed"));
    }
    let loopback = match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => return Err(refused("URL has no host")),
    };
    let class = if loopback {
        EndpointClass::Loopback
    } else {
        if url.scheme() != "https" {
            return Err(refused("a remote endpoint must use https"));
        }
        EndpointClass::Remote
    };
    if !allowed.contains(&class) {
        return Err(refused(format!(
            "{} endpoints are not allowed here",
            match class {
                EndpointClass::Remote => "remote",
                EndpointClass::Loopback => "loopback",
            }
        )));
    }
    Ok(Endpoint { url, class })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &[EndpointClass] = &[EndpointClass::Remote, EndpointClass::Loopback];

    #[test]
    fn endpoints_are_classified_and_unsafe_forms_are_refused() {
        let remote = parse_endpoint("https://example.com/v1/run", BOTH).unwrap();
        assert_eq!(remote.class, EndpointClass::Remote);
        assert_eq!(remote.origin(), "https://example.com");
        for loopback in [
            "http://127.0.0.1:8080/x",
            "http://localhost:1/",
            "http://[::1]:9/",
            "https://127.0.0.2/",
        ] {
            assert_eq!(
                parse_endpoint(loopback, BOTH).unwrap().class,
                EndpointClass::Loopback,
                "{loopback}"
            );
        }
        for refused in [
            "http://example.com/",
            "file:///etc/passwd",
            "ftp://example.com/",
            "https://user:pass@example.com/",
            "https://example.com/#fragment",
            "not a url",
        ] {
            assert!(parse_endpoint(refused, BOTH).is_err(), "{refused}");
        }
        assert!(parse_endpoint("http://127.0.0.1/", &[EndpointClass::Remote]).is_err());
        assert!(parse_endpoint("https://example.com/", &[EndpointClass::Loopback]).is_err());
    }
}
