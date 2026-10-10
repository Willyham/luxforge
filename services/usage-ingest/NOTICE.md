# Notices

`luxforge-usage-ingest` is part of Luxforge and is licensed GPL-3.0-or-later; the full text is the
repository's top-level `LICENSE`.

It links only open-source crates from crates.io, each pinned by `=x.y.z` in `Cargo.toml` and by
version and checksum in this directory's `Cargo.lock`. Their licences are permissive (MIT,
Apache-2.0 and similar) and are checked against the repository's `deny.toml` allowlist by
`cargo xtask audit`, together with the registry sources and the RustSec advisories, with no
advisory exceptions. That is an automated policy check. The manual licence and notice review of
these dependencies is deferred by the owner and has not been done; do not read this file as an
audit. The licence text of each crate is in its published source.

Direct dependencies: `bytes`, `http-body-util`, `hyper` and `hyper-util` (MIT), `serde`,
`serde_json` and `tokio` (MIT or Apache-2.0), with `tokio` also used by the tests.
