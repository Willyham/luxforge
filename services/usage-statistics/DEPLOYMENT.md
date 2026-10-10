# Deploying the usage statistics backend: activation checklist

This is the checklist for putting the [reference stack](README.md) behind `api.luxforge.app` and then
turning the desktop's production switch on. It is written from the implemented stack: every command
below was run against the local reference stack unless it needs infrastructure that stack does not have
(DNS, a certificate, a proxy, a host firewall, a provider), and those steps name what to look at and what
result is required instead of showing a configuration nobody has run.

**Nothing here has been done.** No host, DNS record, certificate or proxy exists for `api.luxforge.app`,
the desktop's `PRODUCTION_ACTIVE` switch is `false`, and delivery to the real endpoint is **unverified**:
a client that cannot reach it is not a client failure. Provisioning, DNS, certificates, running the probe
against the production name and the switch change are each a separate action that needs the owner's
authorization. The [live-endpoint probe](#12-run-the-synthetic-live-probe) and this checklist exist so they
can be requested and reviewed before any of that happens.

The disclosure promises that only a real deployment can keep: **no client IP address is recorded
anywhere**, and **reports are deleted 90 days after they are received**. Every step that follows exists to
make those two sentences true in the place they are said. A step that cannot be completed is a stop, not
a note: do not continue and do not turn the switch on.

Work through the steps in order and keep a deployment record (the date, the commit, and the result of
each check, with the output of every command that has one) outside the repository. Never put a real
password, a client address or a real report in the repository, the record or a ticket.

## 1. Fix what is being deployed

- Deploy a commit whose `services/usage-statistics/test.sh` passed (the local stack test: configuration,
  ingestion, queries, retention, pressure, authentication and privacy against real containers) and whose
  `tools/cargo-cached test --manifest-path services/usage-ingest/Cargo.toml` passed. Note the commit.
- The ingress and the desktop must agree on the event registry:
  `cmp services/usage-ingest/registry.json crates/luxforge-core/src/usage/registry.json` prints
  nothing. The ingress validates against its own compiled-in copy.
- Images are pinned by digest in `compose.yaml` and `grafana/Dockerfile`; deploy those pins. Raising one
  means rerunning `test.sh` first.
- Create `.env` from `.env.example` on the host only, with a different strong random value for every
  password (`openssl rand -hex 16`). It never enters version control, an image or a log.
- Compose forwards only three ingress settings from `.env` (`LUXFORGE_INGRESS_ID_INTERVAL_SECS`,
  `_ID_BURST`, `_STATS_INTERVAL_SECS`). To change any other [limit](#7-the-ingress-limits), add the
  variable to the `ingress.environment` block of the compose file you deploy, then rerun `test.sh` and
  this checklist against that file.

## 2. Choose hosting that cannot record client addresses

The statistics route must never reach infrastructure that records client IP addresses or keeps state
keyed by one. Source and destination addresses exist in packets and connection state while a connection
lasts; that is unavoidable and is not what this step is about. It is about **records**: a log line, a
flow record, an analytics row, a counter or a ban list. Check every layer a request crosses, from the
internet to the ingress, and record for each one how you checked and what you found:

| Layer | Unsuitable if it does this | How to check |
| --- | --- | --- |
| Provider network | Flow logs, DDoS-protection or "network analytics" dashboards, and packet mirroring that retain source addresses | List the provider's flow-log, traffic-mirroring and DDoS-analytics settings for every network, interface and address the host uses. Each must be absent or off, and you must be unable to enable them silently (an organisation-wide default counts) |
| Edge services | A CDN, WAF, bot-management, API gateway, serverless platform or managed TLS front door in front of the host. These log or analyse client addresses by default and are unsuitable unless every such feature is provably off | Resolve the name ([step 5](#5-dns)): the address must be the host's own (or a plain load balancer's) and not a CDN or proxy range. List the provider's access-log setting on any load balancer; it must be off |
| Host firewall and kernel | Rules or defaults that log packets (`LOG`, `NFLOG`, `ulog`, a firewall "logging" level that records blocked connections), connection-tracking accounting or export, intrusion detection, `fail2ban`, `crowdsec` or any IP-keyed ban or rate list | Read the firewall's logging setting and rule list; list installed intrusion, ban and accounting services and show they are absent or disabled; scan the kernel log ([step 10](#10-check-every-hop-for-addresses-and-request-data)) |
| TLS terminator or reverse proxy | An access log with a client address, headers or paths; an error log that prints the peer address; a status or metrics page that lists clients; IP-based rate or connection limits (`limit_req`/`limit_conn` keyed by address, an "IP rate limit" product); adding `X-Forwarded-For`, `Forwarded`, `Via`, `X-Real-IP`, a request ID or a cookie | Read the effective configuration for these; provoke errors and scan its logs ([step 10](#10-check-every-hop-for-addresses-and-request-data)); the probe fails on added response headers |
| Host monitoring | An agent, APM or log shipper that collects connection tables, process network activity, container logs or request metadata and sends them anywhere | List agents and shippers on the host; each must be absent or configured to exclude the proxy, the ingress and the stack's containers |
| Docker | Anything other than the capped `json-file` logs of `compose.yaml` | `docker compose config` shows the logging block on every service; `docker info --format '{{.LoggingDriver}}'` for the daemon default |

Administration of the host (SSH) records the administrator's address; that is outside the statistics
route and must use a different listener, but keep it off the same proxy, logs and tools as the route.

If a layer cannot be shown clean, choose different hosting. Do not "mitigate" with a log that is later
filtered: nothing may be written in the first place.

## 3. Network layout

- The public surface is **one TCP port, 443, on the host name `api.luxforge.app`**. Port 80 either is
  closed or answers only a redirect to HTTPS and never takes a batch; the probe checks this.
- The reference compose file publishes the ingress on `127.0.0.1` only, for a proxy on the same host.
  Keep that, or put the proxy on the `edge` network; do not publish the ingress on a public interface.
  On Linux hosts Docker publishes ports with its own firewall rules, which can bypass a host
  firewall's default-deny, so a published port may be reachable from outside whatever the firewall's own
  rule file says: verify from outside.
- Vector, ClickHouse and Grafana publish nothing, and `collector`, `store` and `viz` are `internal`
  networks with no route out. Check on the host:

  ```sh
  docker compose ps --format '{{.Service}} {{.Ports}}'
  docker network ls --filter label=com.docker.compose.project=luxforge-usage-statistics --format '{{.Name}} internal={{.Internal}}'
  ```

  Only the ingress may show a `->` mapping, and it must be `127.0.0.1:PORT->8080/tcp`. `edge` is the
  one network with `internal=false`; the other three are `internal=true`. (`8123/tcp` without `->` is an
  exposed port inside the private network, not a published one.)
- From a machine on another network, scan the host's public addresses (IPv4 and IPv6) for the host you
  own: only 443 (and 80 if it redirects) may answer, and nothing on 3000, 8080, 8123, 8686, 9000 or
  9009. Repeat after every change of firewall, compose file or provider setting.
- The proxy's health check, if any, must be a TCP connect: the ingress has no health route (it answers
  `404`), by design.

## 4. TLS terminator

Whatever proxy or load balancer is used, it must:

- terminate TLS 1.2 and 1.3 only, with a certificate for exactly `api.luxforge.app` ([step 6](#6-certificate));
- forward every method and path to the ingress **unchanged**. The ingress alone answers `202`, `400`,
  `404`, `405`, `413`, `415` and `429`, so a request the proxy filters itself must produce the same
  status. The only accepted request is `POST /v1/usage/batches` with `Content-Type: application/json`,
  a body of at most 65,536 bytes; the ingress refuses everything else, and the probe checks that it does;
- not follow, issue or rewrite redirects for that route (the client treats any `3xx`, and any other `4xx`
  such as a misrouted `404`, as a reason to stop sending until the next launch), not retry a failed `POST` upstream (batches are
  sent once), not compress or cache, and not buffer a body to disk;
- apply a request-body limit no lower than 65,536 bytes plus headers and a timeout no shorter than the
  ingress's ([limits](#7-the-ingress-limits)), so the ingress's own limits are what a client sees;
- write **no** access log, or one with no client address, header, path, user agent, body, request ID or
  timestamp finer than needed; keep error logging off or free of the peer address; expose no status page;
- add no header to requests or responses beyond what HTTP itself needs, and set no cookie;
- key no limit, ban list, metric or cache on a client address.

## 5. DNS

- `api.luxforge.app` has `A` and `AAAA` records pointing directly at the host (or at the plain load
  balancer that fronts it). Publish `AAAA` only if the proxy listens on IPv6 and the checks in this
  document were also run over IPv6. No `CNAME` to a CDN or "proxied" record.
- Turn off query logging and analytics for the zone where the DNS provider offers them. (Authoritative
  servers see resolvers' addresses, not users', but there is no reason to keep even that.)
- Check from outside: `dig +short api.luxforge.app A` and `AAAA` return only addresses you control, and a
  reverse lookup or the provider's console confirms they are not a CDN or shared proxy.

## 6. Certificate

- A certificate from a public CA trusted by the platforms' stores, valid for exactly `api.luxforge.app`,
  renewed automatically; confirm the renewal path works (a dry run if the client has one) and that its
  logs hold no client data. A certificate's name appears in public Certificate Transparency logs; that is
  the host name only.
- Check from outside, TLS 1.2 and 1.3 work and 1.1 does not:

  ```sh
  openssl s_client -connect api.luxforge.app:443 -servername api.luxforge.app -tls1_2 </dev/null
  openssl s_client -connect api.luxforge.app:443 -servername api.luxforge.app -tls1_3 </dev/null
  openssl s_client -connect api.luxforge.app:443 -servername api.luxforge.app -tls1_1 </dev/null
  ```

  The first two end in `Verify return code: 0 (ok)`; the third fails the handshake. The probe repeats
  the certificate, expiry (at least 14 days left) and version checks.

## 7. The ingress limits

Configured by environment variables on the ingress container (defaults are the ingress's own; the
service refuses to start on a bad value and names the variable). Limits touch only the statistics route.

| Variable | Default | Meaning and what to confirm |
| --- | --- | --- |
| `LUXFORGE_INGEST_LISTEN` | `127.0.0.1:8080` (compose: `0.0.0.0:8080` inside its network) | Listen address. Reachable only through the proxy |
| `LUXFORGE_INGEST_MAX_CONNECTIONS` | `128` | Connections in flight; more get a brief `429` or are closed |
| `LUXFORGE_INGEST_REQUEST_TIMEOUT_SECS` | `10` | Longest a connection may live; the proxy's timeout must not be shorter |
| `LUXFORGE_INGEST_GLOBAL_PER_SECOND`, `_GLOBAL_BURST` | `50`, `100` | Global admission for the route: one budget for **all** clients, since nothing is keyed by address. A flood can use it all and starve real clients; that is the accepted best-effort trade (and why the limit exists) |
| `LUXFORGE_INGEST_ID_INTERVAL_SECS`, `_ID_BURST` | `20`, `5` | Per-installation limiter (one batch per interval, burst), in memory, keyed by the validated installation ID only. IDs can be forged, so it is not authentication. The desktop sends at most one batch a minute. `0` turns it off |
| `LUXFORGE_INGEST_ID_TTL_SECS` | `900` | Limiter entry lifetime after the last request (capped at 900: 15 minutes) |
| `LUXFORGE_INGEST_ID_MAX_ENTRIES` | `100000` | Limiter table size; full of live entries it stands aside rather than grow |
| `LUXFORGE_INGEST_QUEUE_BATCHES`, `_QUEUE_BYTES` | `256`, `33554432` | Forwarding queue; full means `429`, never disk |
| `LUXFORGE_INGEST_FORWARD_TIMEOUT_SECS` | `5` | Longest one request to Vector |
| `LUXFORGE_INGEST_STATS_INTERVAL_SECS` | `60` | How often the aggregate counters are logged |
| `LUXFORGE_INGEST_RETRY_AFTER_SECS` | `30` | `Retry-After` for a full queue |

The request cap (65,536 bytes), the route and the nesting limit are compiled in. Container limits are in
`compose.yaml` (128 MB, one CPU for the ingress). Confirm the effective values with
`docker compose config` (the `ingress.environment` block). The live probe exercises the per-installation
limiter; it does **not** flood the endpoint, so the global limit, the queue, the connection cap and the
request timeout are verified by the ingress's tests and `test.sh` at the deployed commit and by reading
this configuration, not against the live host.

## 8. Private services and authenticated dashboards

On the host, from `services/usage-statistics`, with a shorthand for the ClickHouse administrator (it can
connect only from inside its own container):

```sh
chq() { docker compose exec -T clickhouse sh -c 'clickhouse-client -u luxforge_admin --password "$LUXFORGE_CLICKHOUSE_ADMIN_PASSWORD" -n'; }
```

- **ClickHouse has no default user and the roles are narrow.** Expect `Authentication failed` from
  `docker compose exec -T clickhouse clickhouse-client -u default -q 'SELECT 1'`, and from
  `chq <<< "SHOW GRANTS FOR luxforge_writer; SHOW GRANTS FOR luxforge_reader"`: the writer has `INSERT`
  on `luxforge.events` and `luxforge.profile_batches` only, the reader `SELECT ON luxforge.*` only.
- **Grafana requires a sign-in.** Without credentials every API call is refused:

  ```sh
  docker compose exec -T grafana wget -S -O- http://127.0.0.1:3000/api/dashboards/home
  docker compose exec -T grafana wget -S -O- http://127.0.0.1:3000/api/search
  ```

  Both answer `401 Unauthorized`. Anonymous access, sign-up, public dashboards, external snapshots,
  Gravatar, telemetry, update checks, plugin downloads, `/metrics` and router logging are off in
  `compose.yaml`; do not override them.
- **Strong, unique passwords** for the three ClickHouse users and Grafana, as in
  [step 1](#1-fix-what-is-being-deployed); rotate by changing `.env` and recreating the containers.
- **Reaching the dashboards.** The compose file gives Grafana no route out and publishes nothing, so on a
  server it can only be reached by an administrator who has chosen a path to it. The reference has no
  tested remote-access path: `compose.grafana-local.yaml` publishes it on the host's `127.0.0.1` for an
  SSH tunnel, but attaches a network with a route out (its telemetry, update and plugin checks stay
  off, but it is no longer isolated). Decide and record how the dashboards are reached before
  activation; if you use that override, add an egress rule that denies the Grafana container any
  outbound traffic, verify it, and check that the reach path (the tunnel, or a private proxy) logs no
  client addresses. Never expose Grafana on a public address.
- Dashboards are for the owner. Sparse cohorts, timelines and device profiles do not make records
  anonymous, and installation IDs are shown to everyone who signs in.

## 9. Check 90-day retention on the deployed database

Retention is a table property that the server enforces by merging, so check the deployed server, not the
repository. Rows past 90 days are hidden from the read views immediately, and removed from disk by a merge
at least hourly (`merge_with_ttl_timeout` is 3600 seconds), so physical removal can trail the 90th day by
about an hour or more under load; the disclosure's "90 days" means that.

1. The three tables carry the TTL:

   ```sh
   chq <<'SQL'
   SELECT name, engine_full FROM system.tables WHERE database = 'luxforge' AND engine LIKE '%MergeTree' ORDER BY name
   SQL
   ```

   `events`, `profiles` and `batches` must each show `TTL received_at + toIntervalDay(90)`.
2. The read views hide anything older:

   ```sh
   chq <<'SQL'
   SELECT name, position(create_table_query, 'toIntervalDay(90)') > 0 AS hides_past_90_days FROM system.tables
   WHERE database = 'luxforge' AND name IN ('events_unique', 'batches_unique', 'session_profiles') ORDER BY name
   SQL
   ```

   All three show `1`.
3. Expiry runs on this server. Copy the three tables' definitions into a scratch database, shorten the TTL
   to 2 seconds, insert one synthetic row into each, and require a merge to remove them. Nothing here
   touches `luxforge.*`:

   ```sh
   chq <<'SQL'
   CREATE DATABASE luxforge_ttltest;
   CREATE TABLE luxforge_ttltest.events AS luxforge.events;
   CREATE TABLE luxforge_ttltest.profiles AS luxforge.profiles;
   CREATE TABLE luxforge_ttltest.batches AS luxforge.batches;
   ALTER TABLE luxforge_ttltest.events MODIFY TTL received_at + INTERVAL 2 SECOND;
   ALTER TABLE luxforge_ttltest.profiles MODIFY TTL received_at + INTERVAL 2 SECOND;
   ALTER TABLE luxforge_ttltest.batches MODIFY TTL received_at + INTERVAL 2 SECOND;
   INSERT INTO luxforge_ttltest.events (received_at, installation_id, session_id, session_start, batch_seq, seq, t, name, release) VALUES (now(), generateUUIDv4(), generateUUIDv4(), now(), 0, 0, 0, 'session.start', '0.0.0+000000000000');
   INSERT INTO luxforge_ttltest.profiles (received_at, installation_id, session_id, release, batch_seq, dropped, os_family, arch, cpu_vendor, cpu_physical_cores, cpu_logical_cores, memory_gib_band, gpu_count, renderer) VALUES (now(), generateUUIDv4(), generateUUIDv4(), '0.0.0+000000000000', 0, 0, 'other', 'other', 'other', 1, 1, '8', 0, 'reference');
   INSERT INTO luxforge_ttltest.batches VALUES (now(), generateUUIDv4(), generateUUIDv4(), 0, 0);
   SELECT 'before', (SELECT count() FROM luxforge_ttltest.events), (SELECT count() FROM luxforge_ttltest.profiles), (SELECT count() FROM luxforge_ttltest.batches);
   SELECT sleep(3);
   OPTIMIZE TABLE luxforge_ttltest.events FINAL;
   OPTIMIZE TABLE luxforge_ttltest.profiles FINAL;
   OPTIMIZE TABLE luxforge_ttltest.batches FINAL;
   SELECT 'after', (SELECT count() FROM luxforge_ttltest.events), (SELECT count() FROM luxforge_ttltest.profiles), (SELECT count() FROM luxforge_ttltest.batches);
   DROP DATABASE luxforge_ttltest;
   SQL
   ```

   Expect `before 1 1 1` and `after 0 0 0`. The copies take the TTL from the real tables, so a table
   without one would show `after 1 1 1`. (`sleep(3)` prints a `0`.)
4. No query log or other log table holds inputs; this returns no rows:

   ```sh
   chq <<< "SELECT name FROM system.tables WHERE database = 'system' AND name LIKE '%log%'"
   ```
5. After activation, on or after day 91 of real operation, `SELECT min(received_at) FROM luxforge.events`
   (the table itself, not a view; likewise `profiles` and `batches`) must be no more than 90 days and a
   few hours old. Put this in the operator's routine; if it fails, the disclosure is not being kept.

The data volume must be on an encrypted disk, so that removed parts do not remain readable on the
medium after the host is retired.

## 10. Check every hop for addresses and request data

Adapted from `test.sh`'s privacy checks, which prove it for the local containers. Repeat it on the
deployment, over every place a request leaves a trace.

1. Note the public address of the machine you will send from (your router or ISP page tells you). It is
   the needle to find; do not write it into the repository.
2. From that machine, send a rejected request carrying sentinel values, and malformed traffic that
   makes the proxy log an error (a plain-text request to the TLS port, and a connection that closes at
   once):

   ```sh
   curl -sS -o /dev/null -w '%{http_code}\n' -X POST https://api.luxforge.app/v1/usage/batches \
     -H 'Content-Type: application/json' -H 'X-Forwarded-For: 203.0.113.77' -H 'X-Real-IP: 198.51.100.23' \
     -H 'User-Agent: SentinelAgent/9.9' -H 'Cookie: lf_sentinel=c00k1e5ent1nel' -H 'X-Sentinel-Header: hdr-5ent1nel-7788' \
     --data '{"RAW-PAYLOAD-SENTINEL-4242":1}'              # 400
   printf 'GET / HTTP/1.1\r\nHost: x\r\n\r\n' | nc api.luxforge.app 443
   nc -z api.luxforge.app 443
   ```

3. Run the [probe](#12-run-the-synthetic-live-probe) as well, so accepted batches are in the stores; it
   prints the installation IDs it used. Then wait longer than `LUXFORGE_INGEST_STATS_INTERVAL_SECS`.
4. Collect, from every hop: the proxy's access and error logs and its journal; the host kernel and
   firewall log; the Docker daemon's and each container's logs; the logs of any monitoring agent or
   shipper; and a dump of the three tables. Then scan each with `scan-logs.py` (in a Bash shell), which reports every
   needle and every IPv4 or IPv6 literal that is not allowed and fails on an empty capture:

   ```sh
   LEAKS=(--needle me=YOUR.PUBLIC.ADDRESS --needle xff=203.0.113.77 --needle realip=198.51.100.23
          --needle ua=SentinelAgent --needle cookie=c00k1e5ent1nel --needle header=hdr-5ent1nel-7788
          --needle body=RAW-PAYLOAD-SENTINEL-4242)        # add every password of .env
   IDS=(--needle installation1=PROBE-INSTALLATION-ID)     # one per ID the probe printed
   for s in ingress vector clickhouse grafana; do
     docker compose logs --no-color --no-log-prefix $s | ./scan-logs.py "${LEAKS[@]}" "${IDS[@]}"; done
   for t in events profiles batches; do                   # stored rows hold IDs by design: no IDS here
     chq <<< "SELECT * FROM luxforge.$t FORMAT TSV" | ./scan-logs.py "${LEAKS[@]}"; done
   journalctl -k --no-pager | ./scan-logs.py "${LEAKS[@]}" "${IDS[@]}"            # host kernel and firewall
   journalctl -u docker --no-pager | ./scan-logs.py "${LEAKS[@]}" "${IDS[@]}"     # the Docker daemon
   journalctl -u PROXY_UNIT --no-pager | ./scan-logs.py "${LEAKS[@]}" "${IDS[@]}" --allow SERVER.OWN.ADDRESS
   ```

   (`chq` is defined in [step 8](#8-private-services-and-authenticated-dashboards). A proxy that logs to
   a file: pass the file path instead, and scan each file a shipper reads, each monitoring agent's
   own store, and every provider log you enabled to check.) Every scan must report `0 findings`.
   `--allow` may name only the server's own addresses, never a client's. The kernel log
   of a host with firewall logging on will fail here; that is the point.
5. Also confirm by reading configuration, since a quiet log is not proof of no log: the proxy writes no
   access log or an address-free one; the ingress logs only `usage-ingest listening`, its fixed messages
   and `usage-ingest stats accepted=N ...` counter lines (the scan above checks the ingress's actual
   output); Vector captures no header and has no console or file sink; ClickHouse's system log tables
   are removed ([step 9](#9-check-90-day-retention-on-the-deployed-database)); Grafana's request logging
   is off; Docker's log driver is capped `json-file`.

A `me=` or sentinel finding anywhere is a failure of the hosting choice, not of the scan. Fix the
source, delete what it wrote, and redo this step from the top.

## 11. Backups

- **There are no raw bodies to back up, and none may be created.** The ingress and the relays hold a
  request in memory only; no layer may capture, mirror or log a body (proxy body logging, WAF capture,
  packet capture, debug dumps). Check this as part of step 10.
- What may be backed up: this repository's files, the compose and proxy configuration, the image pins
  and the secrets in `.env` (in a secret store, encrypted), and the deployment record. None of it holds a
  report.
- Data (the `clickhouse-data` volume, a ClickHouse backup or a disk or VM snapshot of the host) is a copy
  of reports, and a copy is subject to the same promise: **every copy of a row must be gone no later
  than 90 days after the row was received**, and a restore must not bring back an expired row. A backup
  contains rows up to 89 days old, which expire within a day of it being taken, so a backup of this
  data cannot honestly be kept longer than about a day, or must be rewritten to drop expired rows before
  it is kept. The reference takes none and the disclosure is simplest and strongest with none: also turn
  off automatic provider snapshots of the data disk, and delete any that exist.
- If the owner decides otherwise, record the decision, the exact retention of every copy and how a
  restore re-applies the 90 days (the views hide older rows at once, and a merge removes them), and test
  a restore with synthetic rows before relying on it.

## 12. Run the synthetic live probe

`probe.sh` ([probe.py](probe.py)) runs the live checks with generated, clearly synthetic data only: random
installation and session IDs and release `0.0.0+000000000000`, which no real build carries. It reads no
file and sends no real report. It never follows a redirect, reads at most 4 KiB of a reply and sends no
header of its own beyond `Host`, `Content-Type`, `Content-Length` and `Accept-Encoding` (and
`Transfer-Encoding` for the chunked test). It checks:

- over HTTPS: the certificate verifies for the host name against the default trust store and has at least
  14 days left; TLS 1.2 or later is negotiated; TLS 1.0 and 1.1 are refused (reported as skipped, not as a
  pass, when the probing Python cannot offer them);
- a valid batch with every kind of event and a device profile, a minimal valid batch and one sent with
  `charset=utf-8` are accepted (`202`, empty body);
- thirteen kinds of bad batch (empty, truncated, not JSON, an array, an unknown top-level or event key, a
  wrong schema number, an upper-case ID, a release without a revision, no events, events out of order, an
  unregistered feature, a free-form system string) are rejected with `400`;
- a body over 65,536 bytes, declared or chunked, is `413`; `text/plain`, a form type and no
  `Content-Type` are `415`; `GET`, `PUT`, `DELETE` and `HEAD` on the route are `405` with `Allow: POST`;
  twelve other requests (other paths, and the route with a trailing slash, an extra segment or a query
  string) are `404`;
- across **all** replies: every request got a reply, none is a redirect or carries `Location`, every body
  is at most 64 bytes, no reply has a cookie, trace or request identifier, forwarding or CDN header, and
  every status is one of `202`, `400`, `404`, `405`, `413`, `415`;
- `--check-plain-http`: a batch sent to plain HTTP on port 80 is not accepted (closed, refused, an error
  or a redirect to HTTPS);
- `--check-id-limiter` (with `--id-burst`, default 5, set to the deployed value): the burst is admitted
  and the next batch from the same installation gets `429` with a numeric `Retry-After`, while another
  installation still gets `202`.

`--strict` runs all of them and fails (exit 3) if any check was skipped: a skipped check is not a pass.
It refuses plain HTTP, so it can run only against a real HTTPS endpoint. Exit status: 0 passed, 1 a check
failed, 2 the target was refused (nothing sent), 3 strict with a skip.

First, against a staging deployment of the same configuration (a different host name that you control),
and as often as needed:

```sh
./probe.sh --strict https://STAGING.HOST --id-burst 5
```

The probe **refuses the production domain** (`luxforge.app` and every subdomain) unless
`--confirm-production` is given, and that flag is for the production run only, after the owner has
authorized contacting the real endpoint:

```sh
./probe.sh --strict https://api.luxforge.app --confirm-production --id-burst 5
```

It contacts nothing but the target, from this one machine, in about 50 requests. The locally verified
results for the reference stack are in the [README](README.md#tests); no run against a real endpoint has
happened.

## 13. Remove the probe's rows

Every accepted probe batch is stored like a real one: the probe lists its installation IDs and the
`ALTER` statements to remove them. They carry release `0.0.0+000000000000`, so they are also easy to
exclude, and they expire after 90 days with everything else. To delete them now, run the printed
statements with `chq`, for example:

```sh
chq <<'SQL'
ALTER TABLE luxforge.events DELETE WHERE installation_id IN ('ID1', 'ID2') SETTINGS mutations_sync = 2;
ALTER TABLE luxforge.profiles DELETE WHERE installation_id IN ('ID1', 'ID2') SETTINGS mutations_sync = 2;
ALTER TABLE luxforge.batches DELETE WHERE installation_id IN ('ID1', 'ID2') SETTINGS mutations_sync = 2;
SQL
```

Do this before activation so that the first dashboard numbers contain no synthetic sessions; sessions
that carry the synthetic release can be told apart from every real build's.

## 14. Sign-off

Every earlier step passed on this deployment, on the commit of [step 1](#1-fix-what-is-being-deployed),
with the output recorded: hosting layers ([2](#2-choose-hosting-that-cannot-record-client-addresses)),
exposure ([3](#3-network-layout)), terminator ([4](#4-tls-terminator)), DNS ([5](#5-dns)), certificate
([6](#6-certificate)), limits ([7](#7-the-ingress-limits)), private services and authentication
([8](#8-private-services-and-authenticated-dashboards)), retention ([9](#9-check-90-day-retention-on-the-deployed-database)),
the hop-by-hop scan ([10](#10-check-every-hop-for-addresses-and-request-data)), backups
([11](#11-backups)) and the strict probe against the real name ([12](#12-run-the-synthetic-live-probe)),
with the probe's rows removed ([13](#13-remove-the-probes-rows)). Anything not done is listed as not done.
Also required before an eligible build ships: the client-to-backend end-to-end check, which exercises a
synthetic, isolated, opted-in desktop against this endpoint.

Any later change to the host, provider, proxy, DNS, compose file, an image pin or a limit sends you back
to the steps it touches, and to steps 10 and 12 in every case.

## 15. Activation: the last step

Only after step 14, and only as its own separately authorized action, make the one change that enables
production delivery in the desktop:

```rust
// crates/luxforge-core/src/usage/eligibility.rs
pub const PRODUCTION_ACTIVE: bool = true;
```

and adjust the test that asserts the switch is off
(`usage_the_activation_switch_is_off_and_every_production_launch_reports_it`) and the docs that state
it, in that same change and nothing else. Until then, and in every build made before then, the client
is ineligible: no notice, no prompt, no collection and no Privacy tab, and `usage.read` reports
`inactive destination`. A build ships with the switch on only when this checklist's record says the
backend that keeps the notice's promises (no IP records, 90-day expiry) is running.

If a later check fails, the promises are not being kept. Stop accepting batches (remove the public
route at the proxy; each batch is sent once, so refused or unreachable deliveries are lost, and clients
back off or stop for the launch), fix the
cause, delete what was recorded wrongly, repeat the steps it touches, and release a build with the
switch off if the fix will take long.
