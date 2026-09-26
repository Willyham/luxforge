//! Which URLs an endpoint may name, before any address is resolved, and which addresses each class
//! may connect to. Resolution and the address checks happen when a request connects, so a name
//! cannot rebind between check and use.
use crate::Error;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

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
    let url = Url::parse(text.trim())
        .map_err(|error| Error::validation(format!("invalid URL: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::validation(format!(
            "scheme {} is not allowed",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::validation("URLs with credentials are not allowed"));
    }
    if url.fragment().is_some() {
        return Err(Error::validation("URLs with fragments are not allowed"));
    }
    let loopback = match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => return Err(Error::validation("URL has no host")),
    };
    let class = if loopback {
        EndpointClass::Loopback
    } else {
        if url.scheme() != "https" {
            return Err(Error::validation("a remote endpoint must use https"));
        }
        EndpointClass::Remote
    };
    if !allowed.contains(&class) {
        return Err(Error::validation(format!(
            "{} endpoints are not allowed here",
            class.label()
        )));
    }
    Ok(Endpoint { url, class })
}

impl EndpointClass {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Remote => "remote",
            Self::Loopback => "loopback",
        }
    }
}

/// IPv4 networks a remote endpoint may never reach, as `(network, prefix length)`. Everything else
/// in IPv4 is globally routable unicast.
const REFUSED_V4: &[(Ipv4Addr, u32)] = &[
    (Ipv4Addr::new(0, 0, 0, 0), 8), // "this network", including unspecified
    (Ipv4Addr::new(10, 0, 0, 0), 8), // private
    (Ipv4Addr::new(100, 64, 0, 0), 10), // shared address space (carrier-grade NAT)
    (Ipv4Addr::new(127, 0, 0, 0), 8), // loopback
    (Ipv4Addr::new(169, 254, 0, 0), 16), // link-local
    (Ipv4Addr::new(172, 16, 0, 0), 12), // private
    (Ipv4Addr::new(192, 0, 0, 0), 24), // IETF protocol assignments
    (Ipv4Addr::new(192, 0, 2, 0), 24), // documentation (TEST-NET-1)
    (Ipv4Addr::new(192, 88, 99, 0), 24), // deprecated 6to4 relay anycast
    (Ipv4Addr::new(192, 168, 0, 0), 16), // private
    (Ipv4Addr::new(198, 18, 0, 0), 15), // benchmarking
    (Ipv4Addr::new(198, 51, 100, 0), 24), // documentation (TEST-NET-2)
    (Ipv4Addr::new(203, 0, 113, 0), 24), // documentation (TEST-NET-3)
    (Ipv4Addr::new(224, 0, 0, 0), 4), // multicast
    (Ipv4Addr::new(240, 0, 0, 0), 4), // reserved, including the limited broadcast address
];

/// The IPv6 global unicast block; nothing outside it is routable.
const GLOBAL_UNICAST: (Ipv6Addr, u32) = (Ipv6Addr::new(0x2000, 0, 0, 0, 0, 0, 0, 0), 3);
/// Parts of the global unicast block that are not globally routable.
const REFUSED_V6: &[(Ipv6Addr, u32)] = &[
    // IETF protocol assignments, including Teredo, benchmarking and ORCHID.
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 23),
    (Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0), 32), // documentation
    (Ipv6Addr::new(0x3fff, 0, 0, 0, 0, 0, 0, 0), 20),     // documentation
];
/// Prefixes whose last 32 bits are the IPv4 address a packet reaches: IPv4-mapped and the NAT64
/// well-known prefix.
const EMBEDDING_V6: &[(Ipv6Addr, u32)] = &[
    (Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0, 0), 96),
    (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96),
];
/// 6to4, which embeds an IPv4 address in the 32 bits after its prefix.
const SIX_TO_FOUR: (Ipv6Addr, u32) = (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16);

fn within_v4(address: Ipv4Addr, (network, prefix): (Ipv4Addr, u32)) -> bool {
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    u32::from(address) & mask == u32::from(network)
}

fn within_v6(address: Ipv6Addr, (network, prefix): (Ipv6Addr, u32)) -> bool {
    let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
    u128::from(address) & mask == u128::from(network)
}

fn public_v4(address: Ipv4Addr) -> bool {
    !REFUSED_V4
        .iter()
        .any(|&network| within_v4(address, network))
}

fn public_v6(address: Ipv6Addr) -> bool {
    let bits = u128::from(address);
    if EMBEDDING_V6
        .iter()
        .any(|&network| within_v6(address, network))
    {
        return public_v4(Ipv4Addr::from(bits as u32));
    }
    if within_v6(address, SIX_TO_FOUR) {
        return public_v4(Ipv4Addr::from((bits >> 80) as u32));
    }
    // Outside the global unicast block lie unspecified, loopback, the IPv4-compatible `::/96` and
    // IPv4-translated forms, discard-only `100::/64`, local-use NAT64 `64:ff9b:1::/48`,
    // unique-local `fc00::/7`, link-local `fe80::/10`, site-local `fec0::/10` and multicast.
    within_v6(address, GLOBAL_UNICAST)
        && !REFUSED_V6
            .iter()
            .any(|&network| within_v6(address, network))
}

/// Whether a request of `class` may connect to `address`. `Loopback` allows only `127.0.0.0/8` and
/// `::1`. `Remote` allows only globally routable unicast; an IPv6 address that embeds an IPv4 one
/// (mapped, NAT64 or 6to4) takes the class of the address it embeds.
pub fn address_allowed(address: IpAddr, class: EndpointClass) -> bool {
    match (class, address) {
        (EndpointClass::Loopback, IpAddr::V4(address)) => address.is_loopback(),
        (EndpointClass::Loopback, IpAddr::V6(address)) => address.is_loopback(),
        (EndpointClass::Remote, IpAddr::V4(address)) => public_v4(address),
        (EndpointClass::Remote, IpAddr::V6(address)) => public_v6(address),
    }
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

    fn v4(text: &str) -> Ipv4Addr {
        text.parse().unwrap()
    }

    fn remote(address: impl Into<IpAddr>) -> bool {
        address_allowed(address.into(), EndpointClass::Remote)
    }

    /// The NAT64 well-known-prefix and 6to4 forms of an IPv4 address.
    fn embedded(address: Ipv4Addr) -> [Ipv6Addr; 2] {
        let bits = u128::from(u32::from(address));
        [
            Ipv6Addr::from((0x64_ff9b_u128 << 96) | bits),
            Ipv6Addr::from((0x2002_u128 << 112) | (bits << 80) | 1),
        ]
    }

    #[test]
    fn remote_endpoints_refuse_every_non_public_ipv4_range_in_every_form() {
        for text in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "10.255.255.255",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.10",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.255",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            let address = v4(text);
            assert!(!remote(address), "{text}");
            assert!(!remote(address.to_ipv6_mapped()), "mapped {text}");
            assert!(!remote(address.to_ipv6_compatible()), "compatible {text}");
            for form in embedded(address) {
                assert!(!remote(form), "{form} embeds {text}");
            }
        }
    }

    #[test]
    fn remote_endpoints_allow_public_addresses_and_their_embedded_forms() {
        for text in [
            "1.1.1.1",
            "8.8.8.8",
            "9.9.9.9",
            "100.128.0.1",
            "172.32.0.1",
            "192.0.3.1",
            "198.20.0.1",
            "223.255.255.254",
        ] {
            let address = v4(text);
            assert!(remote(address), "{text}");
            assert!(remote(address.to_ipv6_mapped()), "mapped {text}");
            for form in embedded(address) {
                assert!(remote(form), "{form} embeds {text}");
            }
            // The IPv4-compatible form is deprecated and not routable, whatever it embeds.
            assert!(!remote(address.to_ipv6_compatible()), "compatible {text}");
        }
        for text in [
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2a00:1450:4001:80b::200e",
        ] {
            assert!(remote(text.parse::<Ipv6Addr>().unwrap()), "{text}");
        }
    }

    #[test]
    fn remote_endpoints_refuse_every_non_public_ipv6_range() {
        for text in [
            "::",
            "::1",
            "::ffff:0:a00:1",
            "100::1",
            "64:ff9b:1::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf:ffff::1",
            "fec0::1",
            "feff::1",
            "ff02::1",
            "ff0e::1",
            "2001::1",
            "2001:2::1",
            "2001:1ff::1",
            "2001:db8::1",
            "2001:db8:ffff::1",
            "3fff::1",
            "3fff:fff::1",
            "4000::1",
        ] {
            assert!(!remote(text.parse::<Ipv6Addr>().unwrap()), "{text}");
        }
    }

    #[test]
    fn loopback_endpoints_allow_only_loopback_addresses() {
        let loopback = |address: IpAddr| address_allowed(address, EndpointClass::Loopback);
        for allowed in ["127.0.0.1", "127.255.255.254", "::1"] {
            assert!(loopback(allowed.parse().unwrap()), "{allowed}");
        }
        for refused in [
            "0.0.0.0",
            "10.0.0.1",
            "8.8.8.8",
            "::",
            "::ffff:127.0.0.1",
            "::127.0.0.1",
            "fe80::1",
            "2606:4700:4700::1111",
        ] {
            assert!(!loopback(refused.parse().unwrap()), "{refused}");
        }
    }
}
