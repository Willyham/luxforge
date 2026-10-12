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
cd /Users/will/projects/luxforge-backend
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
