"""Tests of ../install-feeds.py. Run with: python3 -I services/usage-statistics/tests/install_feeds_test.py"""

import contextlib
import importlib.util
import io
import os
import sys
import tarfile
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("install_feeds", os.path.join(os.path.dirname(HERE), "install-feeds.py"))
install_feeds = importlib.util.module_from_spec(spec)
spec.loader.exec_module(install_feeds)


def feed(build):
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n<rss version="2.0" '
        'xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle"><channel><title>Luxforge</title>'
        f"<item><sparkle:version>{build}</sparkle:version></item></channel></rss>\n"
    ).encode()


def archive(members):
    """A ustar archive of (name, bytes) regular files, or (name, TarInfo type) for other kinds."""
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name, content in members:
            info = tarfile.TarInfo(name)
            if isinstance(content, bytes):
                info.size = len(content)
                tar.addfile(info, io.BytesIO(content))
            else:
                info.type = content
                if content == tarfile.SYMTYPE:
                    info.linkname = "../../etc/passwd"
                tar.addfile(info)
    return out.getvalue()


def good(build=1):
    return archive([("linux-x86_64/appcast.xml", feed(build)), ("macos-arm64/appcast.xml", feed(build + 1)),
                    ("linux-arm64/appcast.xml", feed(build + 2))])


def run(store, data):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        rc = install_feeds.main([store], stdin=io.BytesIO(data))
    return rc, out.getvalue() + err.getvalue()


def served(store, target):
    with open(os.path.join(store, "current", target, "appcast.xml"), "rb") as f:
        return f.read()


def publications(store):
    return sorted(e for e in os.listdir(store) if e.startswith("feeds-"))


class InstallTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.store = self.dir.name

    def tearDown(self):
        self.dir.cleanup()

    def test_a_publication_is_installed_and_replaces_the_current_link(self):
        rc, out = run(self.store, good(1))
        self.assertEqual(rc, 0, out)
        self.assertIn("linux-x86_64 ", out)
        self.assertIn("previous none", out)
        self.assertEqual(served(self.store, "linux-x86_64"), feed(1))
        self.assertEqual(served(self.store, "macos-arm64"), feed(2))
        self.assertEqual(served(self.store, "linux-arm64"), feed(3))
        self.assertTrue(os.path.islink(os.path.join(self.store, "current")))
        first = os.readlink(os.path.join(self.store, "current"))
        mode = os.stat(os.path.join(self.store, "current", "linux-x86_64", "appcast.xml")).st_mode & 0o777
        self.assertEqual(mode, 0o644)

        # The next publications keep only the previous one for a rollback.
        for build in (3, 5):
            rc, out = run(self.store, good(build))
            self.assertEqual(rc, 0, out)
        self.assertEqual(served(self.store, "linux-x86_64"), feed(5))
        kept = publications(self.store)
        self.assertEqual(len(kept), 2, kept)
        self.assertNotIn(first, kept)
        self.assertFalse([e for e in os.listdir(self.store) if e.startswith(".incoming-") or e == ".current-new"])

    def test_a_windows_feed_is_accepted_beside_the_released_targets(self):
        data = archive([("linux-x86_64/appcast.xml", feed(1)), ("macos-arm64/appcast.xml", feed(1)),
                        ("linux-arm64/appcast.xml", feed(1)), ("windows-x86_64/appcast.xml", feed(1))])
        rc, out = run(self.store, data)
        self.assertEqual(rc, 0, out)
        self.assertEqual(served(self.store, "windows-x86_64"), feed(1))

    def test_refusals_change_nothing(self):
        self.assertEqual(run(self.store, good(1))[0], 0)
        before = sorted(os.listdir(self.store)), os.readlink(os.path.join(self.store, "current"))
        linux = ("linux-x86_64/appcast.xml", feed(1))
        mac = ("macos-arm64/appcast.xml", feed(1))
        arm = ("linux-arm64/appcast.xml", feed(1))
        cases = {
            "empty input": b"",
            "not a tar": b"not a tar archive" * 64,
            "compressed": __import__("gzip").compress(good(1)),
            "missing macOS": archive([linux, arm]),
            "missing ARM64 Linux": archive([linux, mac]),
            "an extra file": archive([linux, mac, arm, ("README", b"x")]),
            "another name": archive([linux, mac, arm, ("linux-x86_64/appcast.xml.bak", feed(1))]),
            "a traversal": archive([linux, mac, arm, ("../current/linux-x86_64/appcast.xml", feed(1))]),
            "an absolute path": archive([linux, mac, arm, ("/linux-x86_64/appcast.xml", feed(1))]),
            "an unknown target": archive([linux, mac, arm, ("linux-riscv64/appcast.xml", feed(1))]),
            "a directory": archive([("linux-x86_64", tarfile.DIRTYPE), linux, mac, arm]),
            "a link": archive([linux, arm, ("macos-arm64/appcast.xml", tarfile.SYMTYPE)]),
            "twice": archive([linux, mac, arm, linux]),
            "a DTD": archive([linux, arm, ("macos-arm64/appcast.xml", b'<?xml version="1.0"?><!DOCTYPE rss [<!ENTITY a "b">]><rss><channel/></rss>')]),
            "not XML": archive([linux, arm, ("macos-arm64/appcast.xml", b"<rss><channel>")]),
            "not RSS": archive([linux, arm, ("macos-arm64/appcast.xml", b"<feed><channel/></feed>")]),
            "no channel": archive([linux, arm, ("macos-arm64/appcast.xml", b"<rss/>")]),
            "not UTF-8": archive([linux, arm, ("macos-arm64/appcast.xml", b'<?xml version="1.0" encoding="latin-1"?><rss><channel><title>\xe9</title></channel></rss>')]),
            "a large feed": archive([linux, arm, ("macos-arm64/appcast.xml", b"<rss><channel/></rss>" + b" " * install_feeds.MAX_FEED)]),
        }
        for why, data in cases.items():
            with self.subTest(why=why):
                rc, out = run(self.store, data)
                self.assertEqual(rc, 1, out)
                self.assertIn("refused", out)
                after = sorted(os.listdir(self.store)), os.readlink(os.path.join(self.store, "current"))
                self.assertEqual(after, before)
        rc, out = run(self.store, b"\0" * (install_feeds.MAX_ARCHIVE + 1))
        self.assertEqual(rc, 1, out)
        self.assertIn("larger than", out)

    def test_an_unusable_store_or_usage_is_an_error(self):
        self.assertEqual(run(os.path.join(self.store, "missing"), good())[0], 2)
        os.mkdir(os.path.join(self.store, "current"))
        rc, out = run(self.store, good())
        self.assertEqual(rc, 1, out)
        self.assertIn("not a symbolic link", out)
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(install_feeds.main([], stdin=io.BytesIO(good())), 2)
            self.assertEqual(install_feeds.main(["--help"], stdin=io.BytesIO(good())), 2)


if __name__ == "__main__":
    unittest.main()
