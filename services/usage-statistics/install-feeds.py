#!/usr/bin/env python3
"""Installs a publication of Luxforge's update feeds into the API host's update store.

    install-feeds.py STORE      (the deploy archive on standard input)

The release workflow's feed job sends the deploy archive that `cargo xtask update-feed --deploy`
writes: an uncompressed tar holding exactly `<target>/appcast.xml` for macos-arm64,
linux-x86_64 and linux-arm64 (and windows-x86_64 when one exists), nothing else. On the host this script is the
forced command of the publication account's one SSH key, so that key can do nothing but this.

It refuses, changing nothing: an archive over 16 MiB, a member that is not one of those regular
files, a feed over 4 MiB, a missing released target, and a feed that is not UTF-8 RSS (a DTD or
entity declaration is refused before parsing). Otherwise it writes the feeds into a new
`STORE/feeds-<UTC time>/`, syncs them, and replaces the `STORE/current` link at once, so the API
(which serves `STORE/current/<target>/appcast.xml`) answers either the old or the new feeds, never
a mixture. It keeps the previous publication for a manual rollback and removes older ones.
Publications are serialised with a lock on `STORE/.lock`.

Exit status: 0 installed, 1 refused, 2 usage error or an unusable store.
"""

import fcntl
import io
import os
import re
import shutil
import sys
import tarfile
import tempfile
import time
import xml.etree.ElementTree as ElementTree

MAX_ARCHIVE = 16 * 1024 * 1024
MAX_FEED = 4 * 1024 * 1024
TARGETS = ("macos-arm64", "linux-x86_64", "linux-arm64", "windows-x86_64")
REQUIRED = ("macos-arm64", "linux-x86_64", "linux-arm64")
MEMBER = re.compile(r"^(macos-arm64|linux-x86_64|linux-arm64|windows-x86_64)/appcast\.xml$")
DECLARATION = re.compile(rb"<!\s*(DOCTYPE|ENTITY)", re.IGNORECASE)
CURRENT = "current"
PUBLICATION = re.compile(r"^feeds-\d{8}T\d{12}Z$")


class Refused(Exception):
    pass


def feeds_from(archive):
    """The feeds an archive holds, by target, or `Refused` naming why not."""
    if len(archive) > MAX_ARCHIVE:
        raise Refused(f"the archive is larger than {MAX_ARCHIVE} bytes")
    try:
        tar = tarfile.open(fileobj=io.BytesIO(archive), mode="r:")
        members = tar.getmembers()
    except (tarfile.TarError, EOFError) as error:
        raise Refused(f"not an uncompressed tar archive ({error})") from None
    feeds = {}
    for member in members:
        match = MEMBER.match(member.name)
        if not match or not member.isreg():
            raise Refused(f"{member.name!r} is not a target's appcast.xml file")
        target = match.group(1)
        if target in feeds:
            raise Refused(f"{member.name} appears twice")
        if member.size > MAX_FEED:
            raise Refused(f"{member.name} is larger than {MAX_FEED} bytes")
        data = tar.extractfile(member).read()
        check_feed(member.name, data)
        feeds[target] = data
    missing = [t for t in REQUIRED if t not in feeds]
    if missing:
        raise Refused(f"no feed for {', '.join(missing)}")
    return feeds


def check_feed(name, data):
    if DECLARATION.search(data):
        raise Refused(f"{name} declares a DTD or an entity")
    try:
        data.decode("utf-8")
        root = ElementTree.fromstring(data)
    except (UnicodeDecodeError, ElementTree.ParseError) as error:
        raise Refused(f"{name} is not well-formed UTF-8 XML ({error})") from None
    if root.tag != "rss" or root.find("channel") is None:
        raise Refused(f"{name} is not an RSS feed with a channel")


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def install(store, feeds, now=None):
    """Writes `feeds` as a new publication and points `current` at it. Returns (new, previous)."""
    link = os.path.join(store, CURRENT)
    previous = None
    if os.path.lexists(link):
        if not os.path.islink(link):
            raise Refused(f"{link} is not a symbolic link")
        previous = os.readlink(link)
    now = time.time() if now is None else now
    stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime(now))
    micros = int((now % 1) * 1_000_000)
    name = f"feeds-{stamp}{micros:06d}Z"
    if os.path.lexists(os.path.join(store, name)):
        raise Refused(f"{name} already exists; publish again")
    incoming = tempfile.mkdtemp(prefix=".incoming-", dir=store)
    try:
        os.chmod(incoming, 0o755)
        for target, data in sorted(feeds.items()):
            directory = os.path.join(incoming, target)
            os.mkdir(directory, 0o755)
            path = os.path.join(directory, "appcast.xml")
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
            with os.fdopen(fd, "wb") as f:
                f.write(data)
                f.flush()
                os.fsync(f.fileno())
            os.chmod(path, 0o644)
            sync_dir(directory)
        sync_dir(incoming)
        os.rename(incoming, os.path.join(store, name))
    except BaseException:
        shutil.rmtree(incoming, ignore_errors=True)
        raise
    staged = os.path.join(store, ".current-new")
    if os.path.lexists(staged):
        os.remove(staged)
    os.symlink(name, staged)
    os.replace(staged, link)
    sync_dir(store)
    keep = {name, previous}
    for entry in os.listdir(store):
        path = os.path.join(store, entry)
        stale = (PUBLICATION.match(entry) and entry not in keep) or entry.startswith(".incoming-")
        if stale and os.path.isdir(path) and not os.path.islink(path):
            shutil.rmtree(path, ignore_errors=True)
    return name, previous


def main(argv=None, stdin=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 1 or argv[0].startswith("-"):
        print("usage: install-feeds.py STORE  (the deploy archive on standard input)", file=sys.stderr)
        return 2
    store = argv[0]
    if not os.path.isdir(store) or os.path.islink(store):
        print(f"install-feeds: {store} is not a directory", file=sys.stderr)
        return 2
    stream = stdin if stdin is not None else sys.stdin.buffer
    archive = stream.read(MAX_ARCHIVE + 1)
    with open(os.path.join(store, ".lock"), "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            feeds = feeds_from(archive)
            name, previous = install(store, feeds)
        except Refused as error:
            print(f"install-feeds: refused: {error}", file=sys.stderr)
            return 1
    sizes = ", ".join(f"{t} {len(d)} bytes" for t, d in sorted(feeds.items()))
    print(f"install-feeds: installed {name} ({sizes}); previous {previous or 'none'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
