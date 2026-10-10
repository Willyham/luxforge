"""Tests of ../scan-logs.py. Run with: python3 -I services/usage-statistics/tests/scan_logs_test.py"""

import contextlib
import io
import os
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import importlib.util  # noqa: E402

spec = importlib.util.spec_from_file_location("scan_logs", os.path.join(os.path.dirname(HERE), "scan-logs.py"))
scan_logs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scan_logs)


def run(text, *args):
    with tempfile.TemporaryDirectory() as d:
        path = os.path.join(d, "log")
        with open(path, "w") as f:
            f.write(text)
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = scan_logs.main([*args, path])
    return rc, out.getvalue() + err.getvalue()


class ScanTests(unittest.TestCase):
    def test_clean_logs_pass(self):
        rc, out = run("usage-ingest listening\nusage-ingest stats accepted=3 bad_request=1\n2026-10-10T17:42:41Z up on 127.0.0.1\n")
        self.assertEqual(rc, 0, out)

    def test_ip_addresses_are_found(self):
        for line in ("peer 203.0.113.9 connected", "from [2001:db8::1]:443", "x 2001:db8:0:0:0:0:0:7 y", "172.18.0.4 - -"):
            with self.subTest(line=line):
                rc, out = run(line + "\n")
                self.assertEqual(rc, 1, out)

    def test_allowed_and_non_addresses_are_not_findings(self):
        text = "version 25.8.33.6 (official build)\n25.8.33.6 (official build)\n12:34:56 started\nmac 02:42:ac:11:00:02\n999.1.1.1\n::1 ok\n"
        text += "DB::Exception: no @ Poco::PooledThread::run() std::vector<String> vector::config::loading\n"
        rc, out = run(text)
        self.assertEqual(rc, 0, out)
        rc, _ = run("seen 10.0.0.5\n", "--allow", "10.0.0.5")
        self.assertEqual(rc, 0)

    def test_needles(self):
        rc, out = run("a Sentinel-UA/9 b\n", "--needle", "user agent=Sentinel-UA/9")
        self.assertEqual(rc, 1)
        self.assertIn("contains user agent", out)
        rc, _ = run("nothing\n", "--needle", "user agent=Sentinel-UA/9")
        self.assertEqual(rc, 0)

    def test_empty_input_is_an_error_not_a_pass(self):
        rc, out = run("")
        self.assertEqual(rc, 2, out)


if __name__ == "__main__":
    unittest.main(verbosity=2)
