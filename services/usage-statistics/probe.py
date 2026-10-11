#!/usr/bin/env python3
"""Synthetic live-endpoint probe for Luxforge's usage statistics ingress.

Runs the deployment checklist's live checks (../usage-statistics/DEPLOYMENT.md) against a base URL
with generated, clearly synthetic data only: random installation and session IDs, release
`0.0.0+000000000000` (no real build carries it), no file read and no real report. It follows no
redirect, reads at most 4 KiB of any reply, and sends no header but the request's own, except one
batch sent exactly as the desktop sends it: its header fields, in its order, with its fixed
User-Agent.

    ./probe.sh http://127.0.0.1:8080                       # a local reference stack
    ./probe.sh --strict https://staging.example.org        # every check, a skip is a failure

Exit status: 0 every check passed (skips are listed and are not passes), 1 a check failed, 2 usage
error or a refused target, 3 `--strict` and a check was skipped.

The probe refuses the production domain (`luxforge.app` and its subdomains) unless
`--confirm-production` is given. That flag is for the separately authorized activation step only.
"""

import argparse
import http.client
import json
import re
import secrets
import socket
import ssl
import sys
import time
import warnings
import uuid
from datetime import datetime, timedelta, timezone

ROUTE = "/v1/usage/batches"
MAX_BODY = 65536
SYNTHETIC_RELEASE = "0.0.0+000000000000"
PRODUCTION_DOMAIN = "luxforge.app"
MAX_REPLY_BODY = 64  # the ingress answers with a fixed short status word
READ_LIMIT = 4096
LOOPBACK = ("localhost", "127.0.0.1", "::1")
# What the desktop's transport (crates/luxforge-net/src/transport.rs) writes between Host and
# Content-Type; it writes Content-Length first. The version is the workspace's, 0.0.0 in every build
# today. `services/usage-statistics/test.sh --client` records the desktop's actual header fields.
DESKTOP_HEADERS = (("User-Agent", "Luxforge/0.0.0"), ("Accept-Encoding", "identity"), ("Connection", "close"))

# Response headers that would mean something between the client and the ingress identifies,
# traces or tracks requests. Matched by exact name or prefix, case-insensitively.
FORBIDDEN_REPLY_HEADERS = (
    "set-cookie", "location", "via", "forwarded", "x-forwarded-", "x-real-ip", "x-request-id",
    "x-correlation-id", "x-trace-id", "traceparent", "tracestate", "x-b3-", "x-amzn-", "x-amz-",
    "cf-", "x-cache", "x-served-by", "x-envoy-", "x-azure-", "x-vercel-", "fly-", "server-timing",
    "report-to", "nel", "x-client-ip", "true-client-ip", "x-datadog-", "x-sentry-",
)

URL_PATTERN = re.compile(
    r"^(?P<scheme>https?)://(?P<host>[a-z0-9]([a-z0-9.-]*[a-z0-9])?\.?|\[[0-9a-f:]+\])(:(?P<port>[0-9]{1,5}))?/?$",
    re.IGNORECASE,
)


class Refused(Exception):
    """The target or the arguments are not acceptable; nothing was sent."""


def parse_target(url, confirm_production):
    """Validate a base URL strictly and return (scheme, host, port). Sends nothing."""
    match = URL_PATTERN.match(url)
    if not match:
        raise Refused("the target must be a base URL such as https://host or https://host:8443, "
                      "with no path, query, user information or escapes")
    scheme = match.group("scheme").lower()
    host = match.group("host").lower().rstrip(".")
    if host.startswith("["):
        host = host[1:-1]
    port = int(match.group("port")) if match.group("port") else (443 if scheme == "https" else 80)
    if not 0 < port < 65536:
        raise Refused("the port is out of range")
    production = host == PRODUCTION_DOMAIN or host.endswith("." + PRODUCTION_DOMAIN)
    if production and not confirm_production:
        raise Refused(f"{host} is under the production domain {PRODUCTION_DOMAIN}. This probe sends synthetic "
                      "batches that the production database would store for 90 days. Run it against "
                      "production only as the separately authorized activation step, with --confirm-production.")
    if scheme != "https" and host not in LOOPBACK:
        raise Refused("plain HTTP is only for loopback addresses (127.0.0.1, localhost, ::1)")
    return scheme, host, port


class Reply:
    def __init__(self, status=None, headers=(), body=b"", error=None):
        self.status, self.headers, self.body, self.error = status, list(headers), body, error

    def header(self, name):
        return next((v for k, v in self.headers if k.lower() == name.lower()), None)

    def describe(self):
        if self.error:
            return f"no reply ({self.error})"
        return f"{self.status} {self.body[:40]!r}"


class Probe:
    def __init__(self, scheme, host, port, timeout, pause):
        self.scheme, self.host, self.port, self.timeout, self.pause = scheme, host, port, timeout, pause
        self.replies = []  # every reply to the route family, for the all-replies checks
        self.results = []  # (status, name, detail) with status in "pass", "fail", "skip"
        self.installation = self.new_id()
        self.installations = [self.installation]

    @staticmethod
    def new_id():
        return str(uuid.UUID(bytes=secrets.token_bytes(16), version=4))

    # -- reporting
    def record(self, status, name, detail=""):
        self.results.append((status, name, detail))
        print(f"{status.upper():4}  {name}" + (f"  [{detail}]" if detail and status != "pass" else ""))

    def check(self, name, ok, detail=""):
        self.record("pass" if ok else "fail", name, detail)
        return ok

    def skip(self, name, why):
        self.record("skip", name, why)

    # -- transport
    def connection(self, scheme=None, port=None, context=None):
        scheme = scheme or self.scheme
        port = port or self.port
        if scheme == "https":
            context = context or ssl.create_default_context()
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            return http.client.HTTPSConnection(self.host, port, timeout=self.timeout, context=context)
        return http.client.HTTPConnection(self.host, port, timeout=self.timeout)

    def send(self, method, path, body=None, content_type="application/json", chunked=False, extra=(),
             scheme=None, port=None, track=True, desktop=False):
        """One request on its own connection; never follows a redirect; reads at most READ_LIMIT bytes.
        `desktop` writes the desktop's header fields in its order: Content-Length, Host, User-Agent,
        Accept-Encoding, Connection, Content-Type."""
        time.sleep(self.pause)
        conn = self.connection(scheme, port)
        try:
            conn.putrequest(method, path, skip_host=desktop, skip_accept_encoding=desktop)
            if desktop:
                default = (scheme or self.scheme) == "https" and (port or self.port) == 443 or \
                    (scheme or self.scheme) == "http" and (port or self.port) == 80
                host = f"[{self.host}]" if ":" in self.host else self.host
                conn.putheader("Content-Length", str(len(body)))
                conn.putheader("Host", host if default else f"{host}:{port or self.port}")
                for name, value in DESKTOP_HEADERS:
                    conn.putheader(name, value)
            if content_type is not None:
                conn.putheader("Content-Type", content_type)
            if body is not None and not chunked and not desktop:
                conn.putheader("Content-Length", str(len(body)))
            if chunked:
                conn.putheader("Transfer-Encoding", "chunked")
            for name, value in extra:
                conn.putheader(name, value)
            conn.endheaders()
            try:
                if chunked:
                    for i in range(0, len(body), 8192):
                        part = body[i:i + 8192]
                        conn.send(f"{len(part):x}\r\n".encode() + part + b"\r\n")
                    conn.send(b"0\r\n\r\n")
                elif body is not None:
                    conn.send(body)
            except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
                pass  # the server may answer and close before the whole body is sent; read the answer
            response = conn.getresponse()
            data = response.read(READ_LIMIT)
            reply = Reply(response.status, response.getheaders(), data)
        except (OSError, http.client.HTTPException) as e:
            reply = Reply(error=f"{type(e).__name__}: {e}")
        finally:
            conn.close()
        if track:
            self.replies.append(reply)
        return reply

    # -- synthetic data
    def batch(self, rich=False, installation=None):
        """A valid synthetic batch. Every call is a new session on this run's one installation."""
        session = self.new_id()
        start = (datetime.now(timezone.utc) - timedelta(seconds=30)).strftime("%Y-%m-%dT%H:%M:%SZ")
        device = {"os": {"family": "other", "version": [0]}, "arch": "other",
                  "cpu": {"vendor": "other", "physical_cores": 1, "logical_cores": 1},
                  "memory_gib_band": 8, "gpus": [], "renderer": "reference", "renderer_fallback": "other"}
        events = [{"seq": 0, "t": 0, "name": "session.start", "trigger": "launch"}]
        if rich:
            device = {"os": {"family": "linux", "version": [6, 8, 0], "distro": "other", "distro_version": [1],
                             "kernel": [6, 8, 0, 1]},
                      "arch": "x86_64",
                      "cpu": {"vendor": "other", "physical_cores": 2, "logical_cores": 4},
                      "memory_gib_band": 16,
                      "gpus": [{"vendor": "other", "backend": "vulkan", "type": "virtual", "unified_memory": False,
                                "memory_gib_band": 8, "driver_version": [1, 2, 3]}],
                      "renderer": "gpu"}
            image = "00" * 16
            events += [
                {"seq": 1, "t": 5, "name": "app.presented", "state": "visible"},
                {"seq": 2, "t": 10, "name": "workspace.enter", "workspace": "develop"},
                {"seq": 3, "t": 20, "name": "image.open", "image": image},
                {"seq": 4, "t": 30, "name": "interaction", "feature": "zoom", "image": image},
                {"seq": 5, "t": 40, "name": "tool.commit", "tool": "basic", "image": image},
                {"seq": 6, "t": 50, "name": "images.added", "count": 1},
                {"seq": 7, "t": 60, "name": "preview.settled", "image": image, "renderer": "gpu", "duration_ms": 10},
                {"seq": 8, "t": 70, "name": "operation.end", "operation": "export", "outcome": "completed",
                 "duration_ms": 20, "count": 1, "image": image},
                {"seq": 9, "t": 80, "name": "process.sample", "cpu_percent": 1, "memory_mib": 100,
                 "memory_kind": "footprint"},
            ]
        return {"schema": 1, "disclosure_version": 1, "release": SYNTHETIC_RELEASE,
                "installation_id": installation or self.installation, "session_id": session,
                "session_start": start, "batch_seq": 0, "dropped": 0, "device": device, "events": events}

    def desktop_batch(self):
        """A valid synthetic batch in the shape the desktop sends: a macOS profile with every optional
        field an Apple silicon Mac reports, and the events of an opt-in and a Develop journey."""
        b = self.batch(installation=self.new_id())
        self.installations.append(b["installation_id"])
        image = "11" * 16
        b["device"] = {"os": {"family": "macos", "version": [26, 5], "build": "25F71"}, "arch": "arm64",
                       "cpu": {"vendor": "apple", "physical_cores": 12, "logical_cores": 12, "generation": 4,
                               "tier": "pro"},
                       "memory_gib_band": 32,
                       "gpus": [{"vendor": "apple", "backend": "metal", "type": "integrated", "unified_memory": True,
                                 "vendor_id": 4203, "generation": 4, "tier": "pro"}],
                       "renderer": "gpu"}
        b["events"] = [
            {"seq": 0, "t": 0, "name": "session.start", "trigger": "opt-in"},
            {"seq": 1, "t": 900, "name": "interaction", "feature": "develop-picks"},
            {"seq": 2, "t": 1100, "name": "operation.end", "operation": "source-preparation", "outcome": "completed",
             "duration_ms": 180, "image": image},
            {"seq": 3, "t": 1120, "name": "image.open", "image": image},
            {"seq": 4, "t": 2400, "name": "tool.commit", "tool": "basic", "image": image},
            {"seq": 5, "t": 3000, "name": "interaction", "feature": "zoom", "image": image},
            {"seq": 6, "t": 4000, "name": "interaction", "feature": "export", "image": image},
            {"seq": 7, "t": 4800, "name": "operation.end", "operation": "export", "outcome": "completed",
             "duration_ms": 790, "count": 1, "image": image},
        ]
        return b

    @staticmethod
    def encode(value):
        return json.dumps(value, separators=(",", ":")).encode()

    # -- checks
    def expect(self, name, reply, status, *, body=None, allow=None):
        ok = reply.status == status
        if ok and body is not None:
            ok = reply.body == body
        if ok and allow is not None:
            ok = reply.header("Allow") == allow
        self.check(name, ok, f"expected {status}, got {reply.describe()}")

    def post(self, value=None, raw=None, **kw):
        return self.send("POST", ROUTE, raw if raw is not None else self.encode(value), **kw)

    def accepted(self):
        good = self.batch(rich=True)
        self.expect("a valid batch with every kind of event and a device profile is accepted (202, empty body)",
                    self.post(good), 202, body=b"")
        self.expect("a minimal valid batch is accepted (202)", self.post(self.batch()), 202, body=b"")
        self.expect("Content-Type application/json; charset=utf-8 is accepted (202)",
                    self.post(self.batch(), content_type="application/json; charset=utf-8"), 202)
        self.expect("a batch sent exactly as the desktop sends it (its header fields and User-Agent, a macOS "
                    "profile, a Develop journey) is accepted (202, empty body)",
                    self.post(self.desktop_batch(), desktop=True), 202, body=b"")

    def rejected(self):
        def mutated(change):
            b = self.batch(rich=True)
            change(b)
            return b

        def set_key(key, value):
            return lambda b: b.__setitem__(key, value)

        bad = [
            ("an empty body", dict(raw=b"")),
            ("truncated JSON", dict(raw=b'{"schema":1')),
            ("a JSON array", dict(raw=b"[]")),
            ("not JSON", dict(raw=b"hello")),
            ("an unknown top-level key", dict(value=mutated(set_key("filename", "x.jpg")))),
            ("an unknown event field", dict(value=mutated(lambda b: b["events"][0].update({"path": "/tmp/x"})))),
            ("a wrong schema number", dict(value=mutated(set_key("schema", 2)))),
            ("an upper-case identifier", dict(value=mutated(lambda b: b.update(
                {"session_id": b["session_id"].upper()})))),
            ("a release without a revision", dict(value=mutated(set_key("release", "0.0.0")))),
            ("an empty event list", dict(value=mutated(set_key("events", [])))),
            ("events out of order", dict(value=mutated(lambda b: b["events"].reverse()))),
            ("an unregistered feature", dict(value=mutated(lambda b: b["events"][4].update(
                {"feature": "not-in-the-registry"})))),
            ("a free-form system string in the device", dict(value=mutated(lambda b: b["device"]["os"].update(
                {"version_string": "Ubuntu 24.04"})))),
        ]
        for what, kw in bad:
            self.expect(f"{what} is rejected (400)", self.post(**kw), 400)

    def refused_requests(self):
        padded = b'{"pad":"' + b"a" * (MAX_BODY + 4096) + b'"}'
        self.expect("a body over 65,536 bytes is refused (413)", self.post(raw=padded), 413)
        self.expect("a chunked body over 65,536 bytes is refused (413)", self.post(raw=padded, chunked=True), 413)
        good = self.encode(self.batch())
        self.expect("Content-Type text/plain is refused (415)", self.post(raw=good, content_type="text/plain"), 415)
        self.expect("Content-Type application/x-www-form-urlencoded is refused (415)",
                    self.post(raw=good, content_type="application/x-www-form-urlencoded"), 415)
        self.expect("a missing Content-Type is refused (415)", self.post(raw=good, content_type=None), 415)
        for method in ("GET", "PUT", "DELETE", "HEAD"):
            self.expect(f"{method} on the route is refused (405, Allow: POST)",
                        self.send(method, ROUTE, body=None), 405, allow="POST")
        not_found = [("GET", "/"), ("POST", "/"), ("GET", "/health"), ("GET", "/metrics"), ("GET", "/robots.txt"),
                     ("GET", "/.env"), ("POST", ROUTE + "/"), ("POST", "/v1/usage"), ("POST", "/v2/usage/batches"),
                     ("POST", "/v1/usage/batch"), ("POST", ROUTE + "?probe=1"), ("POST", ROUTE + "/../batches")]
        for method, path in not_found:
            body = good if method == "POST" else None
            self.expect(f"{method} {path} is not found (404)", self.send(method, path, body=body), 404)

    def all_replies(self):
        replies = [r for r in self.replies if r.error is None]
        self.check(f"every one of the {len(self.replies)} requests got a reply (no connection failure)",
                   len(replies) == len(self.replies), "; ".join(r.error for r in self.replies if r.error)[:200])
        redirects = [r for r in replies if 300 <= r.status < 400 or r.header("Location")]
        self.check("no reply was a redirect and none carried a Location header", not redirects,
                   ", ".join(r.describe() for r in redirects[:3]))
        big = [r for r in replies if len(r.body) > MAX_REPLY_BODY]
        self.check(f"every reply body is at most {MAX_REPLY_BODY} bytes", not big,
                   ", ".join(f"{len(r.body)} bytes" for r in big[:3]))
        seen = {}
        for r in replies:
            for name, _ in r.headers:
                low = name.lower()
                if any(low == p or (p.endswith("-") and low.startswith(p)) for p in FORBIDDEN_REPLY_HEADERS):
                    seen[low] = seen.get(low, 0) + 1
        self.check("no reply carries a cookie, trace or request identifier, forwarding or CDN header",
                   not seen, ", ".join(sorted(seen)))
        self.check("every refusal and success status is one of 202, 400, 404, 405, 413 or 415",
                   all(r.status in (202, 400, 404, 405, 413, 415) for r in replies),
                   ", ".join(sorted({str(r.status) for r in replies})))

    def tls(self):
        if self.scheme != "https":
            self.skip("TLS certificate, version and protocol checks", "the target is plain HTTP on loopback")
            return
        context = ssl.create_default_context()
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        try:
            with socket.create_connection((self.host, self.port), timeout=self.timeout) as raw:
                with context.wrap_socket(raw, server_hostname=self.host) as tls:
                    version = tls.version()
                    cert = tls.getpeercert()
        except (OSError, ssl.SSLError) as e:
            self.check("the certificate verifies for the host name (default trust store)", False, str(e)[:160])
            return
        self.check("the certificate verifies for the host name (default trust store)", True)
        left = (datetime.strptime(cert["notAfter"], "%b %d %H:%M:%S %Y %Z").replace(tzinfo=timezone.utc)
                - datetime.now(timezone.utc)).days
        self.check("the certificate is valid for at least 14 more days", left >= 14, f"{left} days left")
        self.check("TLS 1.2 or later is negotiated", version in ("TLSv1.2", "TLSv1.3"), str(version))
        name = "TLS 1.0 and 1.1 are refused"
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", DeprecationWarning)
            old = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            old.check_hostname = False
            old.verify_mode = ssl.CERT_NONE
            try:
                old.minimum_version = ssl.TLSVersion.TLSv1
                old.maximum_version = ssl.TLSVersion.TLSv1_1
                old.set_ciphers("ALL:@SECLEVEL=0")
            except (ValueError, ssl.SSLError) as e:
                self.skip(name, f"this Python's TLS library cannot offer them ({e})")
                return
        try:
            with socket.create_connection((self.host, self.port), timeout=self.timeout) as raw:
                with old.wrap_socket(raw, server_hostname=self.host) as tls:
                    self.check(name, False, f"negotiated {tls.version()}")
        except ssl.SSLError as e:
            # Only the server's own refusal counts; a client that cannot offer the version is no evidence.
            if any(word in str(e) for word in ("ALERT_PROTOCOL_VERSION", "ALERT_HANDSHAKE_FAILURE", "ALERT_INTERNAL_ERROR",
                                                "UNEXPECTED_EOF", "EOF occurred")):
                self.check(name, True)
            else:
                self.skip(name, f"cannot tell whether the server or this Python refused it ({str(e)[:80]})")
        except OSError:
            self.check(name, True)  # the server closed or reset the connection

    def plain_http(self):
        if self.scheme != "https":
            self.skip("plain HTTP on port 80 does not accept a batch", "the target is plain HTTP on loopback")
            return
        reply = self.send("POST", ROUTE, self.encode(self.batch()), scheme="http", port=80, track=False)
        accepted = reply.status is not None and 200 <= reply.status < 300
        location = reply.header("Location") or ""
        redirect_ok = reply.status in (301, 302, 307, 308) and location.lower().startswith(f"https://{self.host}")
        self.check("plain HTTP on port 80 does not accept a batch (closed, refused or redirected to HTTPS)",
                   not accepted and (reply.status is None or reply.status >= 400 or redirect_ok),
                   reply.describe())

    def id_limiter(self, burst):
        installation = self.new_id()
        self.installations.append(installation)
        statuses = []
        replies = []
        for _ in range(burst + 1):
            reply = self.post(self.batch(installation=installation))
            replies.append(reply)
            statuses.append(reply.status)
        refused = replies[-1]
        retry = refused.header("Retry-After")
        self.check(f"the per-installation limiter admits a burst of {burst} and refuses the next (429)",
                   statuses == [202] * burst + [429], f"statuses {statuses}")
        self.check("the limiter's 429 carries a numeric Retry-After", bool(retry and retry.isdigit()), str(retry))
        other = self.post(self.batch())
        self.check("a different installation is unaffected by that limit (202)", other.status == 202,
                   other.describe())

    def cleanup_hint(self):
        ids = ", ".join(f"'{i}'" for i in self.installations)
        print(f"\nAccepted synthetic batches are stored: installations {', '.join(self.installations)}, all with "
              f"release {SYNTHETIC_RELEASE}. They expire with everything else after 90 days. To remove them "
              "now, run as the database administrator (DEPLOYMENT.md, step 13):")
        for table in ("events", "profiles", "batches"):
            print(f"  ALTER TABLE luxforge.{table} DELETE WHERE installation_id IN ({ids}) SETTINGS mutations_sync = 2")


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("url", help="base URL of the ingress, for example https://host or http://127.0.0.1:8080")
    parser.add_argument("--strict", action="store_true",
                        help="run every check including --check-plain-http and --check-id-limiter, "
                             "and fail if any check is skipped")
    parser.add_argument("--check-plain-http", action="store_true", help="check that plain HTTP on port 80 takes no batch")
    parser.add_argument("--check-id-limiter", action="store_true",
                        help="send burst+1 synthetic batches from one installation and expect the last to be 429")
    parser.add_argument("--id-burst", type=int, default=5,
                        help="the deployed LUXFORGE_INGEST_ID_BURST (default 5)")
    parser.add_argument("--timeout", type=float, default=15.0, help="seconds per request (default 15)")
    parser.add_argument("--pause", type=float, default=0.1, help="seconds between requests (default 0.1)")
    parser.add_argument("--confirm-production", action="store_true",
                        help="allow a target under luxforge.app (the separately authorized activation step only)")
    args = parser.parse_args(argv)
    if not 1 <= args.id_burst <= 100:
        parser.error("--id-burst must be 1 to 100")
    try:
        scheme, host, port = parse_target(args.url, args.confirm_production)
    except Refused as e:
        print(f"refused: {e}", file=sys.stderr)
        return 2

    probe = Probe(scheme, host, port, args.timeout, args.pause)
    print(f"probe of {scheme}://{host}:{port}{ROUTE} with synthetic data only (installation {probe.installation})")
    if host == PRODUCTION_DOMAIN or host.endswith("." + PRODUCTION_DOMAIN):
        print("PRODUCTION TARGET confirmed by --confirm-production")
    probe.tls()
    first = probe.send("GET", "/", track=False)
    if first.error:
        probe.check("the endpoint answers at all", False, first.error)
        print(f"\nFAILED: the endpoint is unreachable, so no other check ran: {first.error}")
        return 1
    probe.accepted()
    probe.rejected()
    probe.refused_requests()
    probe.all_replies()
    if args.check_plain_http or args.strict:
        probe.plain_http()
    if args.check_id_limiter or args.strict:
        probe.id_limiter(args.id_burst)
    else:
        probe.skip("per-installation limiter", "not requested (--check-id-limiter)")
    passed = sum(1 for r in probe.results if r[0] == "pass")
    failed = [r for r in probe.results if r[0] == "fail"]
    skipped = [r for r in probe.results if r[0] == "skip"]
    print(f"\n{passed} passed, {len(failed)} failed, {len(skipped)} skipped")
    for _, name, why in skipped:
        print(f"SKIPPED (not a pass): {name}: {why}")
    for _, name, detail in failed:
        print(f"FAILED: {name}\n    {detail}")
    probe.cleanup_hint()
    if failed:
        return 1
    if skipped and args.strict:
        print("strict: a skipped check is not a pass")
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
