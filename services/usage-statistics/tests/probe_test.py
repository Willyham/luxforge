"""Tests of ../probe.py against a fake ingress in this process; no Docker and no external network.

    python3 -I services/usage-statistics/tests/probe_test.py

The fake answers like the real ingress (wire format statuses) in its `good` mode, and in the others
misbehaves in one way the deployment checklist cares about; the probe must fail exactly those checks.
The local stack test (../test.sh) runs the same probe against the real containers.
"""

import contextlib
import http.server
import io
import json
import os
import re
import shutil
import ssl
import subprocess
import sys
import tempfile
import threading
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import probe  # noqa: E402

UUID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
RELEASE = re.compile(r"^\d+\.\d+\.\d+\+[0-9a-f]{12}$")
KEYS = {"schema", "disclosure_version", "release", "installation_id", "session_id", "session_start",
        "batch_seq", "dropped", "device", "events"}


def acceptable(body):
    """A rough stand-in for the ingress's validator: enough to tell the probe's good bodies from its bad ones."""
    try:
        b = json.loads(body)
    except ValueError:
        return False
    if not isinstance(b, dict) or set(b) != KEYS or b["schema"] != 1:
        return False
    if not (UUID.match(str(b["installation_id"])) and UUID.match(str(b["session_id"]))
            and RELEASE.match(str(b["release"]))):
        return False
    events = b["events"]
    if not events or any(set(e) & {"path", "filename"} for e in events):
        return False
    if [e["seq"] for e in events] != sorted(e["seq"] for e in events):
        return False
    if any(e.get("feature") not in (None, "zoom", "develop-picks", "export") for e in events):
        return False
    return "version_string" not in b["device"]["os"]


class Fake(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, mode, tls=None):
        super().__init__(("127.0.0.1", 0), Handler)
        self.mode = mode
        if tls:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(*tls)
            self.socket = context.wrap_socket(self.socket, server_side=True)
        self.seen = {}
        self.desktop = []
        self.lock = threading.Lock()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def answer(self, status, word, extra=()):
        mode = self.server.mode
        if mode == "redirect":
            status, word, extra = 301, "", [("Location", "https://example.invalid/")]
        if mode == "leaky":
            extra = [*extra, ("X-Request-Id", "abc"), ("Set-Cookie", "a=b")]
        if mode == "verbose":
            word = word + " " + "x" * 200
        body = word.encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        for k, v in extra:
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.close_connection = True

    def handle_any(self):
        mode = self.server.mode
        if self.path.split("?")[0] != probe.ROUTE or "?" in self.path:
            return self.answer(404, "not found")
        if self.command != "POST":
            if mode == "no-405":
                return self.answer(404, "not found")
            return self.answer(405, "method not allowed", [("Allow", "POST")])
        if self.headers.get("Content-Type", "").replace(" ", "").lower() not in (
                "application/json", "application/json;charset=utf-8"):
            return self.answer(415, "unsupported media type")
        if "chunked" in self.headers.get("Transfer-Encoding", ""):
            if mode == "no-413":
                return self.answer(202, "")
            return self.answer(413, "payload too large")
        length = int(self.headers.get("Content-Length", "0"))
        if length > probe.MAX_BODY and mode != "no-413":
            return self.answer(413, "payload too large")
        body = self.rfile.read(length)
        if not acceptable(body):
            return self.answer(400, "bad request")
        b = json.loads(body)
        with self.server.lock:
            if self.headers.get("User-Agent", "").startswith("Luxforge/"):
                self.server.desktop.append([name.lower() for name in self.headers.keys()])
            n = self.server.seen[b["installation_id"]] = self.server.seen.get(b["installation_id"], 0) + 1
        if mode in ("good", "redirect") and n > 5:
            return self.answer(429, "too many requests", [("Retry-After", "20")])
        return self.answer(202, "")

    do_GET = do_PUT = do_DELETE = do_HEAD = do_POST = do_OPTIONS = handle_any


@contextlib.contextmanager
def fake(mode, tls=None):
    server = Fake(mode, tls)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"{'https' if tls else 'http'}://{'localhost' if tls else '127.0.0.1'}:{server.server_address[1]}", server
    finally:
        server.shutdown()
        server.server_close()


def run_probe(mode, *flags, tls=None, seen=None):
    out, err = io.StringIO(), io.StringIO()
    with fake(mode, tls) as (url, server), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = probe.main([url, "--pause", "0", "--timeout", "5", *flags])
        if seen is not None:
            seen.extend(server.desktop)
    return rc, out.getvalue() + err.getvalue()


class ProbeTests(unittest.TestCase):
    def test_good_endpoint_passes_and_a_skip_is_reported_not_counted(self):
        desktop = []
        rc, out = run_probe("good", "--check-id-limiter", seen=desktop)
        self.assertEqual(rc, 0, out)
        # The desktop-shaped batch carries exactly the desktop transport's header fields, in its order.
        self.assertEqual(desktop, [["content-length", "host", "user-agent", "accept-encoding", "connection",
                                    "content-type"]])
        self.assertNotIn("FAIL ", out)
        self.assertIn("SKIPPED (not a pass): TLS certificate", out)
        self.assertIn("the per-installation limiter admits a burst of 5", out)
        self.assertRegex(out, r"\d+ passed, 0 failed, 1 skipped")

    @unittest.skipUnless(shutil.which("openssl"), "openssl is needed to make a test certificate (this skip is not a pass)")
    def test_tls_checks_run_against_a_trusted_certificate_and_fail_against_an_untrusted_one(self):
        with tempfile.TemporaryDirectory() as d:
            key, cert = os.path.join(d, "key.pem"), os.path.join(d, "cert.pem")
            subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert,
                            "-days", "30", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost"],
                           check=True, capture_output=True)
            rc, out = run_probe("good", tls=(cert, key))  # untrusted: the default store does not hold it
            self.assertEqual(rc, 1, out)
            self.assertRegex(out, r"FAIL .*the certificate verifies for the host name")
            old = os.environ.get("SSL_CERT_FILE")
            os.environ["SSL_CERT_FILE"] = cert  # trust it, as a system store would
            try:
                rc, out = run_probe("good", tls=(cert, key))
            finally:
                if old is None:
                    del os.environ["SSL_CERT_FILE"]
                else:
                    os.environ["SSL_CERT_FILE"] = old
            self.assertIn("PASS  the certificate verifies for the host name", out)
            self.assertIn("PASS  the certificate is valid for at least 14 more days", out)
            self.assertIn("PASS  TLS 1.2 or later is negotiated", out)
            self.assertRegex(out, r"(PASS|SKIP)  TLS 1.0 and 1.1 are refused")
            self.assertEqual(rc, 0, out)

    def test_strict_fails_on_skips(self):
        rc, out = run_probe("good", "--strict")
        self.assertEqual(rc, 3, out)
        self.assertIn("strict: a skipped check is not a pass", out)

    def test_each_misbehaviour_is_caught(self):
        cases = {
            "redirect": "no reply was a redirect",
            "leaky": "no reply carries a cookie, trace or request identifier",
            "verbose": "every reply body is at most 64 bytes",
            "no-405": "GET on the route is refused (405",
            "no-413": "a chunked body over 65,536 bytes is refused (413)",
        }
        for mode, expected in cases.items():
            with self.subTest(mode=mode):
                rc, out = run_probe(mode)
                self.assertEqual(rc, 1, out)
                self.assertRegex(out, r"FAIL .*" + re.escape(expected))

    def test_a_missing_limiter_is_caught(self):
        rc, out = run_probe("leaky", "--check-id-limiter")  # the fake limits only in good/redirect modes
        self.assertEqual(rc, 1)
        self.assertRegex(out, r"FAIL .*the per-installation limiter admits")

    def test_an_unreachable_endpoint_is_one_failure(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = probe.main(["http://127.0.0.1:1", "--timeout", "2"])
        self.assertEqual(rc, 1)
        self.assertIn("unreachable", out.getvalue())

    def test_production_is_refused_without_confirmation_and_before_any_connection(self):
        for url in ("https://api.luxforge.app", "https://API.LuxForge.App.", "https://api.luxforge.app:443",
                    "https://luxforge.app", "https://deep.sub.luxforge.app", "http://api.luxforge.app"):
            with self.subTest(url=url):
                with self.assertRaises(probe.Refused):
                    probe.parse_target(url, False)
                err = io.StringIO()
                with contextlib.redirect_stderr(err):
                    self.assertEqual(probe.main([url]), 2)
                self.assertIn("refused", err.getvalue())

    def test_confirmation_lifts_only_the_production_refusal(self):
        self.assertEqual(probe.parse_target("https://api.luxforge.app", True), ("https", "api.luxforge.app", 443))
        with self.assertRaises(probe.Refused):
            probe.parse_target("http://api.luxforge.app", True)  # plain HTTP is still refused

    def test_odd_urls_are_refused(self):
        for url in ("https://user@host", "https://host/v1/usage/batches", "https://host?x=1", "https://%61pi.luxforge.app",
                    "ftp://host", "https://host:99999", "http://example.org", "https://", "https://ho st",
                    "https://api.luxforge.app\\@evil.example", "https://evil.example#@api.luxforge.app"):
            with self.subTest(url=url):
                with self.assertRaises(probe.Refused):
                    probe.parse_target(url, False)

    def test_lookalike_domains_are_not_production_but_are_probed_as_ordinary_hosts(self):
        self.assertEqual(probe.parse_target("https://notluxforge.app", False)[1], "notluxforge.app")
        self.assertEqual(probe.parse_target("https://luxforge.app.example.org", False)[1], "luxforge.app.example.org")

    def test_synthetic_batches_use_the_synthetic_release_and_fresh_ids(self):
        p = probe.Probe("http", "127.0.0.1", 1, 1, 0)
        a, b = p.batch(rich=True), p.batch()
        self.assertEqual(a["release"], probe.SYNTHETIC_RELEASE)
        self.assertNotEqual(a["session_id"], b["session_id"])
        self.assertEqual(a["installation_id"], b["installation_id"])
        self.assertLess(len(json.dumps(a["device"], separators=(",", ":"))), 2048)
        self.assertTrue(acceptable(p.encode(a)) and acceptable(p.encode(b)))


if __name__ == "__main__":
    unittest.main(verbosity=2)
