//! The host's only network path: endpoint classification, resolution checks, redirects, limits and
//! TLS. See `docs/design/module-capabilities.md#transport`.
pub mod policy;

pub use policy::{Endpoint, EndpointClass, parse_endpoint};
