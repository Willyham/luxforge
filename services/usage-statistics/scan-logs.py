#!/usr/bin/env python3
"""Searches captured logs or dumps for what a usage statistics deployment must never record.

    scan-logs.py [--needle LABEL=VALUE]... [--allow ADDRESS]... [FILE...]     (standard input if no FILE)

It reports, with the file, line number and a clipped copy of the line:
  * every occurrence of a `--needle` value (your own public address, the sentinel header values
    the checklist sends, the installation IDs the probe printed, a password, ...), and
  * every IPv4 or IPv6 literal that is not allowed. 0.0.0.0, 127.0.0.1 and ::1 are always allowed,
    as are four-part software versions written as "version a.b.c.d" or "a.b.c.d (official build)"; add
    `--allow` for an address that is expected, such as the server's own.
Timestamps and MAC addresses are not mistaken for IPv6 addresses.

Exit status: 0 nothing found, 1 something found, 2 usage error. A scan of empty input is an error,
because an empty capture proves nothing (exit 2): check where the logs really go.
"""

import argparse
import ipaddress
import re
import sys

IPV4 = re.compile(r"(?<![\w.])(\d{1,3}(?:\.\d{1,3}){3})(?![\w.])")
IPV6 = re.compile(r"(?<![\w:.])((?:[0-9A-Fa-f]{0,4}:){2,7}[0-9A-Fa-f]{0,4})(?![\w:])")
ALWAYS_ALLOWED = {"0.0.0.0", "127.0.0.1", "::1", "::"}


def ip_literals(line):
    """Yield (kind, text) for each address literal in a line, ignoring software versions."""
    for m in IPV4.finditer(line):
        text = m.group(1)
        try:
            ipaddress.IPv4Address(text)
        except ValueError:
            continue
        if line[max(0, m.start() - 8):m.start()] == "version " or line[m.end():m.end() + 10] == " (official":
            continue
        yield "IPv4", text
    for m in IPV6.finditer(line):
        text = m.group(1)
        if "::" not in text and text.count(":") != 7:
            continue  # a time of day, a MAC address or a fragment, not a full address
        try:
            ipaddress.IPv6Address(text)
        except ValueError:
            continue
        yield "IPv6", text


def scan(lines, source, needles, allowed):
    findings = []
    for number, line in enumerate(lines, 1):
        clipped = line.rstrip("\n")[:160]
        for label, value in needles:
            if value in line:
                findings.append(f"{source}:{number}: contains {label}: {clipped}")
        for kind, text in ip_literals(line):
            if text.lower() not in allowed:
                findings.append(f"{source}:{number}: contains {kind} address {text}: {clipped}")
    return findings


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("files", nargs="*", help="files to scan (standard input if none)")
    parser.add_argument("--needle", action="append", default=[], metavar="LABEL=VALUE",
                        help="a value that must not appear (repeatable)")
    parser.add_argument("--allow", action="append", default=[], metavar="ADDRESS",
                        help="an address that may appear (repeatable)")
    args = parser.parse_args(argv)
    needles = []
    for item in args.needle:
        label, sep, value = item.partition("=")
        if not sep or not label or len(value) < 4:
            parser.error(f"--needle wants LABEL=VALUE with a value of at least 4 characters, got {label!r}")
        needles.append((label, value))
    allowed = {a.lower() for a in args.allow} | ALWAYS_ALLOWED
    findings, total = [], 0
    sources = [(f, None) for f in args.files] or [("<stdin>", sys.stdin)]
    for name, stream in sources:
        try:
            lines = (stream or open(name, encoding="utf-8", errors="replace")).read().splitlines(keepends=True)
        except OSError as e:
            print(f"cannot read {name}: {e}", file=sys.stderr)
            return 2
        total += len(lines)
        findings += scan(lines, name, needles, allowed)
    if total == 0:
        print("scanned no lines at all: an empty capture proves nothing", file=sys.stderr)
        return 2
    for f in findings:
        print(f)
    print(f"scanned {total} lines with {len(needles)} needles: {len(findings)} findings")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
