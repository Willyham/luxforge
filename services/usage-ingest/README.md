# luxforge-usage-ingest

The public ingress for Luxforge's optional usage statistics: a small, stateless, validating HTTP
service. It accepts `POST /v1/usage/batches` exactly as the
[wire format](../../docs/design/usage-statistics-wire.md) defines it, refuses anything else, turns an
accepted batch into NDJSON rows and forwards them once to a private collector (for example Vector,
then ClickHouse; see the [design](../../docs/design/usage-statistics.md#open-source-backend-reference)).

It is a standalone package with its own `[workspace]` and `Cargo.lock`, excluded from the desktop
workspace: desktop builds, core tests and `cargo xtask check` build none of it. It has no account,
no embedded secret, no disk, no database and no IP-based state. DNS, certificates and production
deployment are outside it; put it behind a reverse proxy that does not log client addresses,
headers or bodies.

Given an update store (`LUXFORGE_INGEST_UPDATES_DIR`), it also serves the API's update feeds,
`GET /v1/updates/<target>/appcast.xml`, as static content ([below](#update-feeds)).

## Build, test, run

From the repository root (agents use `tools/cargo-cached`; plain `cargo` works the same):

```sh
tools/cargo-cached test --manifest-path services/usage-ingest/Cargo.toml
tools/cargo-cached clippy --manifest-path services/usage-ingest/Cargo.toml --all-targets -- -D warnings
tools/cargo-cached build --release --manifest-path services/usage-ingest/Cargo.toml

LUXFORGE_INGEST_COLLECTOR_URL=http://127.0.0.1:8686 \
  services/usage-ingest/target/release/luxforge-usage-ingest
```

`cargo xtask check-repository` checks the manifest and lockfile rules (own workspace, exact `=x.y.z`
pins, crates.io checksummed sources, GPL licence, `NOTICE.md`), and `cargo xtask audit` runs the
repository's licence, source and advisory policy over this lockfile too, with no exceptions.

## Behaviour

| Request | Status |
| --- | --- |
| Valid batch, queued for forwarding | `202`, empty body |
| Any shape, range, ordering, key, type or duplicate-key violation; empty body; nesting over 5 containers | `400` |
| Body over 65,536 bytes (a declared length is refused before reading; a chunked body is cut off while read) | `413` |
| Content type other than `application/json` (optionally `; charset=utf-8`) | `415` |
| Another method on the route (`Allow: POST`) | `405` |
| Another path, or the route with a query string | `404` |
| Global rate limit, per-installation limit, full forwarding queue, or no free connection | `429`, with `Retry-After` seconds |

Refusals carry a fixed short status word as the body and `Connection: close`. A request that stalls
past the request timeout is closed with no reply. Decode order, cheapest first: path, method,
content type, declared length, global admission, the body read under the cap, strict decode, the
per-installation limit, the forwarding queue. The limits touch only this route.

The `feature` and `tool` enums come from `registry.json` in this directory, compiled in. It is a
byte-for-byte copy of `crates/luxforge-core/src/usage/registry.json` (the desktop's checked-in
registry) and **must stay equal to it**: `tests/desktop.rs` fails when the two differ, and also runs
every synthetic batch the desktop's core accepts or refuses (`crates/luxforge-core/tests/fixtures/usage/`)
through this validator, which must agree. A test fails if the file is not sorted, unique and kebab-case.

The wire format counts nesting depth as "at most 4"; its schema has a deepest value (`driver_version`
inside a GPU object inside `gpus` inside `device` inside the envelope) at the fifth container, so
this service allows five containers deep and refuses a sixth before parsing.

## Update feeds

With `LUXFORGE_INGEST_UPDATES_DIR` set, `GET` and `HEAD /v1/updates/<target>/appcast.xml`, for
`macos-arm64`, `linux-x86_64` and `windows-x86_64`, are answered from `<dir>/<target>/appcast.xml`
([design](../../docs/design/auto-update.md#publishing-and-serving-the-feeds)). The directory is
normally the update store's `current` link, which `services/usage-statistics/install-feeds.py`
replaces at each publication; the service only reads it.

| Request | Status |
| --- | --- |
| A target whose feed exists | `200`, the file, `Content-Type: application/rss+xml; charset=utf-8`, `Cache-Control: public, max-age=900` (`HEAD`: no body, same length) |
| A target with no feed, a feed over 4 MiB, or any query string | `404` |
| Another method (`Allow: GET, HEAD`) | `405` |

Without the setting the route does not exist and every such path is `404`. A feed is read again only
when its file changes (length, time or inode), so at most one copy of each target's feed is held in
memory; the read runs off the async workers. The global and per-installation limits are the statistics
route's and do not apply; the connection cap and the request timeout do. No request header is read.
`tests/updates.rs` covers the route.

## Rows

Each accepted batch makes two POSTs of `application/x-ndjson` to the collector, each tried once and
never retried or spooled: `{COLLECTOR_URL}/events` (one line per event) and `{COLLECTOR_URL}/profiles`
(one line). Times are `YYYY-MM-DDTHH:MM:SSZ` UTC (ClickHouse needs `date_time_input_format =
best_effort`, or Vector's `date_time_best_effort`). Only validated content and the server's receive
time are in a row: no header, address, body or request identifier.

Event row, columns in this order, absent fields `null`: `received_at`, `installation_id`,
`session_id`, `session_start`, `batch_seq`, `seq`, `t`, `name`, `release`, then `trigger`, `state`,
`workspace`, `image`, `feature`, `tool`, `count`, `operation`, `outcome`, `duration_ms`, `renderer`,
`cpu_percent`, `memory_mib`, `memory_kind`, `gpu_percent`, `gpu_memory_mib`.

Profile row: `received_at`, `installation_id`, `session_id`, `release`, `batch_seq`, `dropped` (the
batch's count of its window's events that were not sent), then `os_family`,
`os_version`, `os_build`, `os_distro`, `os_distro_version`, `os_kernel` (versions are integer
arrays), `arch`, `cpu_vendor`, `cpu_physical_cores`, `cpu_logical_cores`, `cpu_generation`,
`cpu_tier`, `cpu_family`, `cpu_model`, `cpu_stepping`, `memory_gib_band` (a string: `"8"` to
`"1024"` or `"over1024"`), `gpu_count`, `gpu0_*` and `gpu1_*` (`vendor`, `backend`, `type`,
`unified_memory`, `vendor_id`, `device_id`, `generation`, `tier`, `memory_gib_band`,
`driver_version`; `null` for a GPU that is not present), `renderer`, `renderer_fallback`.

## Configuration

Environment variables; only the first is required. A bad value stops startup naming the variable.

| Variable | Default | Meaning |
| --- | --- | --- |
| `LUXFORGE_INGEST_COLLECTOR_URL` | none | Private collector base, `http://host:port[/path]` (plain HTTP on a private network only) |
| `LUXFORGE_INGEST_LISTEN` | `127.0.0.1:8080` | Listen address |
| `LUXFORGE_INGEST_MAX_CONNECTIONS` | `128` | Connections (and requests) in flight; more get a brief `429` or are closed |
| `LUXFORGE_INGEST_REQUEST_TIMEOUT_SECS` | `10` | Longest a connection may live, accept to reply |
| `LUXFORGE_INGEST_GLOBAL_PER_SECOND`, `_GLOBAL_BURST` | `50`, `100` | Global admission for the route |
| `LUXFORGE_INGEST_ID_INTERVAL_SECS`, `_ID_BURST` | `20`, `5` | Per-installation limiter: one batch per interval, burst; interval `0` turns it off |
| `LUXFORGE_INGEST_ID_TTL_SECS` | `900` | Limiter entry lifetime after the last request (capped at 900) |
| `LUXFORGE_INGEST_ID_MAX_ENTRIES` | `100000` | Limiter table size; when full of live entries the limiter stands aside |
| `LUXFORGE_INGEST_QUEUE_BATCHES`, `_QUEUE_BYTES` | `256`, `33554432` | Forwarding queue bounds; full means `429` |
| `LUXFORGE_INGEST_FORWARD_TIMEOUT_SECS` | `5` | Longest one request to the collector |
| `LUXFORGE_INGEST_STATS_INTERVAL_SECS` | `60` | How often the aggregate counters are logged |
| `LUXFORGE_INGEST_RETRY_AFTER_SECS` | `30` | `Retry-After` for a full queue |
| `LUXFORGE_INGEST_UPDATES_DIR` | none | The update store whose `<target>/appcast.xml` files the feed route serves; unset, there is no feed route |

## Privacy

The service never reads a peer address (the accept loop discards it), sets no tracking state keyed by
one, and reads no header but `Content-Type` and `Content-Length`. The per-installation limiter is
in memory, keyed by the validated installation ID only, allocated after size, shape and global
admission, and forgets an entry within 15 minutes. It logs two fixed messages and one line of
aggregate counters (`accepted`, `bad_request`, `payload_too_large`, `limited_global`, `feeds_served`,
`feeds_missing`, and so on);
no header, body, rejection reason, identifier or address. The tests capture the log and check this
against sentinel addresses, headers and IDs. Shut down with Ctrl-C; whatever is queued is dropped.
