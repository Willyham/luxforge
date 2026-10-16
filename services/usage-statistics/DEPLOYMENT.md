# Hosted usage statistics and production activation

The hosted backend is live at `https://api.luxforge.app/v1/usage/batches`. It is implemented in
[Willyham/luxforge-backend](https://github.com/Willyham/luxforge-backend), deployed from commit
`106bb41`. Its README, `docs/runbook.md` and `docs/analytics-engine.md` are the deployment and
analysis references. Backend changes belong to Will; this app repository's Vector/ClickHouse/Grafana
stack remains a local reference, with its own limits and retention.

The backend pins the app wire contract at `f8456a94`; the registry and usage fixtures are unchanged
at `23241d22`. Changes to the wire format, registry or fixtures require telling Will to bump that
pin. Developer routing changes only the destination URL; batches have the same validated shape.

`PRODUCTION_ACTIVE` in `crates/luxforge-core/src/usage/eligibility.rs` remains `false`. Enabling it
is Will's separate decision after the checklist below. Visible developer builds can already send
consented test data; this does not activate production reporting.

## Destinations and checks

- Production: `https://api.luxforge.app/v1/usage/batches`, without a query string.
- Test: exactly `https://api.luxforge.app/v1/usage/batches?tenant=test`. The same checks, limits and
  stores apply, labelled `test`, which production dashboards exclude. Every other query string is
  refused, so a typo cannot store production data.
- The usage Worker serves `/v1/usage/*`. Other paths, including `/v1/usage` without the slash,
  reach a fallback Worker and get an empty `404`.

Before the Worker, the WAF limits `/v1/usage/batches`, with or without a query string, to **50
requests per 10 seconds per IP per data centre**. For a request without `Accept` (as the desktop
sends), the refusal is `429`, `Retry-After: 10`, `Content-Type: text/plain; charset=UTF-8` and the
17-byte body `error code: 1015`. This transient IP-based protection is distinct from statistics:
no IP addresses or headers are stored in the usage stores, and Worker logs are off. Do not describe
the deployed edge as having no IP-based limiter or promise that no address exists anywhere.

The Worker's checks run in this order:

| Order | Check | Refusal |
| --- | --- | --- |
| 1 | Wrong path under `/v1/usage/`, or any query except exactly `tenant=test` | `404`, `not found` |
| 2 | Method other than POST | `405`, `Allow: POST` |
| 3 | Declared Content-Length over 65,536 bytes, before reading | `413` |
| 4 | Content-Type other than `application/json`, optionally `; charset=utf-8` | `415` |
| 5 | Read body over 65,536 bytes | `413` |
| 6 | Strict validation against the app wire contract | `400`; nothing from the batch stored |
| 7 | Per-installation rate limit: 10 batches/minute per Cloudflare location, approximate and eventually consistent | `429`, `Retry-After: 60` |
| 8 | Store queue refuses | `503` |
| 9 | Accepted | `202`, empty body |

Worker refusal bodies are fixed short words, at most 32 bytes. No response carries a command.
Both 429 bodies fit the client's 1 KiB cap. The client ignores response bodies, clamps either
Retry-After to its ten-minute minimum, and discards the attempted batch. `503` takes the ordinary
failure cooldown; acceptance does not promise durable storage.

## Storage, retention and analysis

- **R2:** one unsampled object per batch, kept 90 days. Use the raw copy for exact per-session,
  per-installation and per-image event history.
- **Analytics Engine:** one point per event, repeating its device profile. Cloudflare retains it
  for **three months**, not a configurable 90 days. It samples at write time even at low volume;
  weighted aggregates are estimates and individual timelines can be incomplete. Dashboards weight
  `_sample_interval` and separate the test tenant from production.
- **Queue:** messages retained at most four days. Failed downstream writes can lose data.

No store retains client IP addresses or request headers. Verify privacy and retention using the
backend runbook's raw-object checks, Analytics Engine queries, R2 lifecycle and logging settings.
The local reference's exact ClickHouse timelines and 90-day TTL do not prove hosted behavior.

## The desktop against the live test tenant

Start the relay from the separate backend checkout in one terminal:

```sh
cd ../luxforge-backend
npm run relay
```

From the app repository in another terminal:

```sh
tools/cargo-cached build --release --locked -p luxforge-app -p luxforge-cli
LUXFORGE_USAGE_INGRESS=http://127.0.0.1:8788 LUXFORGE_USAGE_BACKEND_PROFILE=live \
  tools/cargo-cached xtask smoke --scenario usage-backend --output artifacts/usage-backend-live
```

The ingress value is an **origin only**, without a path. The relay adds `?tenant=test`, forwards the
method, path, Content-Type and body, and returns status/body and Content-Type, Retry-After and Allow.
For another evidence launch, `--usage-test-receiver http://127.0.0.1:8788/v1/usage/batches` uses
that relay too. The app also accepts exactly the hosted test URL as an evidence test receiver.

The live profile expects six accepted batches and one misrouted `404`; it does not force the hosted
limiter. The journey still checks a withheld accepted reply, no replay, expiry during failure
cooldown, consent/ID rotation and 404 suspension. The default `reference` profile retains the
local stack's forced 429 at one batch per five seconds and tests its Retry-After cooldown. Run that
full storage/logging journey with `services/usage-statistics/test.sh --client NEW_DIR`.

The live smoke records requests and replies in `receiver.json`, with correlated state, logs and
captures. It proves the client/HTTP journey only; inspecting the hosted stores, dashboard filtering,
retention and infrastructure settings remains separate. Synthetic evidence must always use the test
tenant. The old `probe.sh` is for the local reference contract; its production flag does not select
a test tenant and its checks do not match the hosted Worker, so do not run it against the live host.

## Activation checklist

Keep a reviewable deployment record outside source control, without real reports, credentials or
client addresses. Deployment existence is reported by Will; it is not proof that this checklist passed.

1. Confirm the backend deployment's app pin matches the current wire format, registry and fixtures.
2. Run the live test-tenant desktop journey; inspect its bounded replies and correlated state/logs.
3. Inspect R2 objects and Analytics Engine fields for only validated ID/device/event data, with no
   IPs or request headers; confirm Worker logs/traces/Logpush settings in the backend runbook.
4. Confirm R2's 90-day lifecycle, Analytics Engine's three-month retention and queue's four-day bound.
5. Confirm dashboards filter production/test correctly and use sampling weights; use R2 for exact
   histories. Confirm the WAF and per-installation limits and their plain-text refusals.
6. Record checks still unrun, including natural retention expiry or native platform qualification.
7. Will decides whether to activate production. Only then change `PRODUCTION_ACTIVE`, verify the
   revisioned release desktop's consent/disclosure/routing and distribute the eligible build.

Never infer production activation from test traffic, deployment availability or a successful `202`.

## 16. The update feed route

These are deployment instructions for the local reference ingress's implemented feed route.
They do not describe a live Cloudflare update service: the hosted backend currently serves usage
batches only, with other paths reaching its fallback Worker. Serving update feeds there requires
Will's separate backend and publication configuration work. The app's update implementation and
the reference installer remain available for independent local testing.

The reference ingress also answers Luxforge's update feeds,
`GET` and `HEAD /v1/updates/<target>/appcast.xml`, from the same ingress ([design](../../docs/design/auto-update.md#publishing-and-serving-the-feeds)). The
release workflow's **Update feeds** run regenerates them after every release and sends them over SSH to
an account whose only command installs them into the update store. The route is independent of the
statistics switch: it can go live before or after [activation](#activation-checklist). A person
fetching a feed from a deployment of this reference crosses its hosting and proxy too, so apply the
[reference privacy requirements](README.md#reference-deployment-privacy-requirements) to that traffic.
Hosted feed deployment remains unqualified. The commands below that need no host ran against a local
ingress serving a store filled by `install-feeds.py`; host-dependent provisioning remains external work.

The update feed URL is compiled into every release, so once a release that uses it is published the
route must stay for as long as that release is in use. What serves it behind the API can change.

1. **The store and the publication account.** On the host, create a system account, for example
   `luxforge-feeds`, with no password and no other rights, and the store directory it alone owns, for
   example `/srv/luxforge/updates` (mode 0755). Install `install-feeds.py` from the deployed commit
   where that account can run but not change it, for example `/opt/luxforge/install-feeds.py`, owned by
   root, mode 0755.
2. **The publication key.** On an administrator's machine, generate a key used for nothing else:

   ```sh
   ssh-keygen -t ed25519 -N '' -C luxforge-update-feeds -f luxforge-update-feeds
   ```

   Make its public half the account's only `~/.ssh/authorized_keys` entry, restricted and forced to the
   installer:

   ```text
   restrict,command="/usr/bin/python3 -I /opt/luxforge/install-feeds.py /srv/luxforge/updates" ssh-ed25519 AAAA… luxforge-update-feeds
   ```

   `restrict` turns off forwarding, a terminal and the account's rc file, and the forced command runs
   in place of whatever a client asks for. Store the private half as the repository secret
   `LUXFORGE_UPDATE_PUBLISH_SSH_KEY` ([operator guide](../../docs/engineering/releases.md#automatic-updates)),
   then delete both files.
3. **The publication listener.** GitHub-hosted runners publish from changing addresses, so the listener
   must accept SSH from the internet. Keep it apart from administration: a separate `sshd` listener, or
   a `Match User luxforge-feeds` block, that admits only that account, by public key only, with
   `DisableForwarding yes` and `PermitTTY no`. Its log records the runners' addresses, which are
   GitHub's and not a client's; keep it out of the route's proxy and logs anyway. Take the host key line
   for the repository variable `LUXFORGE_UPDATE_PUBLISH_KNOWN_HOSTS` from outside with
   `ssh-keyscan -p PORT -t ed25519 api.luxforge.app`, and confirm that `ssh-keygen -lf -` on that line
   prints the fingerprint `ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub` prints on the host. Set
   `LUXFORGE_UPDATE_PUBLISH_DESTINATION` to `ssh://luxforge-feeds@api.luxforge.app:PORT`.
4. **The ingress.** Set `LUXFORGE_UPDATE_STORE=/srv/luxforge/updates` in `.env`. Compose mounts it
   read-only at `/srv/updates` and points `LUXFORGE_INGEST_UPDATES_DIR` at `/srv/updates/current`.
   The ingress runs as the image's non-root user, which reads the store through the modes the
   installer writes (0755 directories, 0644 files). Recreate the ingress. Until the first publication
   every feed answers `404`.
5. **A first publication.** With `LUXFORGE_PUBLISH_UPDATES` set to `true`, run **Actions → Update
   feeds → Run workflow** on `main`, or let the next release run it. The publication step's log ends
   with `install-feeds: installed feeds-…`, and the run keeps the archive it sent as its
   `update-feeds` artifact. To check the installer on the host by hand, as the account and from the
   deployed commit:

   ```sh
   python3 -I /opt/luxforge/install-feeds.py /srv/luxforge/updates < luxforge-update-feeds.tar
   ```

6. **Check the route from outside**, from another network:

   ```sh
   for target in linux-x86_64 macos-arm64; do
     curl -sS -D - -o "$target.xml" "https://api.luxforge.app/v1/updates/$target/appcast.xml"
     tar -xOf luxforge-update-feeds.tar "$target/appcast.xml" | cmp - "$target.xml"
   done
   curl -sS -o /dev/null -w '%{http_code}
' 'https://api.luxforge.app/v1/updates/linux-x86_64/appcast.xml?x=1'   # 404
   curl -sS -o /dev/null -w '%{http_code}
' -X POST https://api.luxforge.app/v1/updates/linux-x86_64/appcast.xml  # 405
   curl -sS -o /dev/null -w '%{http_code}
' http://api.luxforge.app/v1/updates/linux-x86_64/appcast.xml  # closed, refused or a redirect to HTTPS
   ```

   Each feed answers `200` with `content-type: application/rss+xml; charset=utf-8` and
   `cache-control: public, max-age=900`, no `set-cookie`, no header the proxy added, and the bytes of
   the run's `update-feeds` artifact (`luxforge-update-feeds.tar`). The macOS feed has no item until a
   notarized release is published. Then repeat [privacy checks](#activation-checklist)
   with its sentinel request also sent as a `GET` of a feed: no hop may hold it.
7. **Rollback.** Run **Update feeds** again to replace a wrong publication. In an emergency, an
   administrator points the link back at the previous publication the installer kept:

   ```sh
   cd /srv/luxforge/updates && ln -s feeds-PREVIOUS .current-new && mv -T .current-new current
   ```

Record each step's output in the deployment record. The route counts as live only when step 6 passes
against the real name.
