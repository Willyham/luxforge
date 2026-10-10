# Notices

The files in this directory (compose files, configuration, dashboards, SQL, scripts and tests) are
part of Luxforge and are licensed GPL-3.0-or-later; the full text is the repository's top-level
`LICENSE`.

The reference stack runs the following separate open-source programs. Each is a separately licensed
work that keeps its own licence and notices; nothing here relicenses or modifies them, and they are
not distributed by this repository (a person builds or pulls the images themselves).

| Component | Version | Licence | Source of the licence text |
| --- | --- | --- | --- |
| [Vector](https://github.com/vectordotdev/vector) | 0.59.0 (`timberio/vector`, Alpine image) | MPL-2.0 | the project's `LICENSE` and the image's `/usr/share/vector/LICENSE` |
| [ClickHouse](https://github.com/ClickHouse/ClickHouse) | 25.8.33.6 (`clickhouse/clickhouse-server`) | Apache-2.0 | the project's `LICENSE` |
| [Grafana OSS](https://github.com/grafana/grafana) | 13.0.2 (`grafana/grafana-oss`) | AGPL-3.0 | the project's `LICENSE` |
| [ClickHouse data source plugin](https://github.com/grafana/clickhouse-datasource) for Grafana | 4.22.1 | Apache-2.0 | the project's `LICENSE` |
| `luxforge-usage-ingest` | this repository | GPL-3.0-or-later | `services/usage-ingest/NOTICE.md` |

The ingress image is built from the Rust toolchain image `rust` (`rust:1.94.0-bookworm`, the
Rust project's MIT and Apache-2.0 licences with Debian's packages) and runs on Google's distroless
`cc-debian12` image (Apache-2.0, containing Debian packages under their own licences). The Grafana
and Vector images contain Alpine Linux packages, and the ClickHouse image Ubuntu packages, each under
its own licence.

Grafana is licensed AGPL-3.0. The stack runs it unmodified and does not offer it to other people over
a network as a Luxforge feature; anyone who deploys it for others takes on AGPL-3.0's obligations
themselves, including offering its corresponding source.

Pins: every image is selected by version and digest in `compose.yaml` and the two `Dockerfile`s; the
plugin is selected by version and SHA-256 in `grafana/Dockerfile`; the ingress crates are pinned by
exact version and checksum in `services/usage-ingest/Cargo.lock`.

The manual licence and notice review of these components and of the image contents is deferred by
the owner and has not been done. This file names the components and their upstream licences; do not
read it as an audit.
