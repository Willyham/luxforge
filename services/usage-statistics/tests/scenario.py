"""Synthetic usage batches for the stack test, and the results they must produce.

The expectations here are computed from the batches by plain Python, independently of every SQL
view and dashboard query: each expected number is derived by a short loop over the events below. Only
synthetic identifiers appear; no real report is ever used.
"""

import copy
import time

RELEASE_1 = "1.0.0+aaaaaaaaaaaa"
RELEASE_2 = "1.1.0+bbbbbbbbbbbb"


def uuid(n):
    return f"{n:08x}-1b78-4cbe-8d1c-{n:012x}"


def image(n):
    return f"{n:032x}"


INSTALL_A, INSTALL_B, INSTALL_C, INSTALL_D = uuid(0xA1), uuid(0xB1), uuid(0xC1), uuid(0xD1)
IMG1, IMG2, IMG3, IMG4, IMG5 = (image(0x1000 + i) for i in range(5))

DEVICES = {
    "A": {
        "os": {"family": "macos", "version": [15, 5]},
        "arch": "arm64",
        "cpu": {"vendor": "apple", "generation": 4, "tier": "pro", "physical_cores": 12, "logical_cores": 12},
        "memory_gib_band": 32,
        "gpus": [{"vendor": "apple", "backend": "metal", "type": "integrated", "generation": 4,
                  "tier": "pro", "unified_memory": True}],
        "renderer": "gpu",
    },
    "B": {
        "os": {"family": "macos", "version": [14, 6]},
        "arch": "arm64",
        "cpu": {"vendor": "apple", "generation": 3, "tier": "base", "physical_cores": 8, "logical_cores": 8},
        "memory_gib_band": 16,
        "gpus": [{"vendor": "apple", "backend": "metal", "type": "integrated", "generation": 3,
                  "tier": "base", "unified_memory": True}],
        "renderer": "gpu",
    },
    "C": {
        "os": {"family": "windows", "version": [10, 0, 22631]},
        "arch": "x86_64",
        "cpu": {"vendor": "intel", "physical_cores": 16, "logical_cores": 24, "family": 6, "model": 183,
                "stepping": 1},
        "memory_gib_band": 64,
        "gpus": [{"vendor": "nvidia", "backend": "vulkan", "type": "discrete", "unified_memory": False,
                  "vendor_id": 4318, "device_id": 9988, "memory_gib_band": 16,
                  "driver_version": [560, 94]}],
        "renderer": "gpu",
    },
    "D": {
        "os": {"family": "linux", "version": [6, 8, 0], "distro": "ubuntu", "distro_version": [24, 4]},
        "arch": "x86_64",
        "cpu": {"vendor": "amd", "physical_cores": 4, "logical_cores": 8, "family": 23, "model": 113,
                "stepping": 0},
        "memory_gib_band": 8,
        "gpus": [{"vendor": "mesa", "backend": "vulkan", "type": "cpu", "unified_memory": False}],
        "renderer": "reference",
        "renderer_fallback": "no-adapter",
    },
}


def ev(seq, t, name, **fields):
    return {"seq": seq, "t": t, "name": name, **fields}


# Each session: installation, release, device, session_start (offset seconds before "now") and the
# batches as (batch_seq, dropped, events). A session's events are listed in `seq` order.
SESSIONS = {
    "S1": dict(
        installation=INSTALL_A, release=RELEASE_1, device="A", start_ago=7200, session=uuid(0x51),
        batches=[
            (0, 0, [
                ev(0, 0, "session.start", trigger="launch"),
                ev(1, 0, "app.presented", state="visible"),
                ev(2, 1000, "workspace.enter", workspace="select"),
                ev(3, 5000, "images.added", count=50),
                ev(4, 20000, "workspace.enter", workspace="develop"),
                ev(5, 21000, "image.open", image=IMG1),
                ev(6, 30000, "tool.commit", tool="basic", image=IMG1),
                ev(7, 30500, "preview.settled", image=IMG1, renderer="gpu", duration_ms=120),
                ev(8, 31000, "process.sample", cpu_percent=40, memory_mib=2000, memory_kind="footprint"),
            ]),
            (1, 0, [
                ev(9, 60000, "preview.settled", image=IMG1, renderer="gpu", duration_ms=80),
                ev(10, 70000, "app.idle", state="idle"),
                ev(11, 100000, "app.idle", state="active"),
                ev(12, 110000, "operation.end", operation="export", outcome="completed",
                   duration_ms=3000, count=3, image=IMG1),
                ev(13, 111000, "process.sample", cpu_percent=60, memory_mib=2200, memory_kind="footprint"),
                ev(14, 120000, "app.presented", state="hidden"),
                ev(15, 150000, "app.presented", state="visible"),
                ev(16, 151000, "interaction", feature="export"),
                ev(17, 152000, "preview.settled", image=IMG2, renderer="gpu", duration_ms=200),
            ]),
        ],
    ),
    "S2": dict(
        installation=INSTALL_A, release=RELEASE_1, device="A", start_ago=3600, session=uuid(0x52),
        batches=[
            (0, 0, [
                ev(0, 0, "session.start", trigger="opt-in"),
                ev(1, 2000, "workspace.enter", workspace="develop"),
                ev(2, 3000, "image.open", image=IMG1),
                ev(3, 4000, "preview.settled", image=IMG1, renderer="gpu", duration_ms=100),
                ev(4, 9000, "operation.end", operation="export", outcome="failed", duration_ms=500,
                   count=0, image=IMG1),
                ev(5, 9500, "operation.end", operation="source-preparation", outcome="completed",
                   duration_ms=700, image=IMG1),
                ev(6, 9600, "process.sample", cpu_percent=20, memory_mib=1800, memory_kind="footprint"),
            ]),
        ],
    ),
    # Batch 1 is never sent: a gap of three events (3, 4, 5) and one missing batch. Batch 2 says
    # that four of its window's events were dropped.
    "S3": dict(
        installation=INSTALL_B, release=RELEASE_1, device="B", start_ago=5400, session=uuid(0x53),
        batches=[
            (0, 0, [
                ev(0, 0, "session.start", trigger="launch"),
                ev(1, 500, "images.added", count=10),
                ev(2, 800, "workspace.enter", workspace="develop"),
            ]),
            (2, 4, [
                ev(6, 40000, "operation.end", operation="catalog-scan", outcome="failed", duration_ms=40),
                ev(7, 41000, "preview.settled", image=IMG3, renderer="gpu", duration_ms=300),
                ev(8, 42000, "operation.end", operation="export", outcome="completed",
                   duration_ms=2000, count=1, image=IMG3),
            ]),
        ],
    ),
    # Batch 0 is delivered twice (a client retry): every row must count once.
    "S4": dict(
        installation=INSTALL_C, release=RELEASE_2, device="C", start_ago=1800, session=uuid(0x54),
        duplicate_batches=[0],
        batches=[
            (0, 2, [
                ev(0, 0, "session.start", trigger="launch"),
                ev(1, 1000, "workspace.enter", workspace="develop"),
                ev(2, 2000, "preview.settled", image=IMG4, renderer="gpu", duration_ms=400),
                ev(3, 3000, "preview.settled", image=IMG4, renderer="gpu", duration_ms=500),
                ev(4, 4000, "process.sample", cpu_percent=90, memory_mib=4000, memory_kind="private",
                   gpu_percent=50, gpu_memory_mib=1500),
                ev(5, 5000, "operation.end", operation="auto-tone", outcome="failed", duration_ms=100,
                   image=IMG4),
            ]),
        ],
    ),
    "S5": dict(
        installation=INSTALL_D, release=RELEASE_2, device="D", start_ago=900, session=uuid(0x55),
        batches=[
            (0, 0, [
                ev(0, 0, "session.start", trigger="launch"),
                ev(1, 1000, "workspace.enter", workspace="select"),
                ev(2, 2000, "images.added", count=5),
                ev(3, 3000, "preview.settled", image=IMG5, renderer="reference", duration_ms=900),
                ev(4, 4000, "workspace.enter", workspace="develop"),
                ev(5, 5000, "operation.end", operation="export", outcome="failed", duration_ms=50,
                   count=0, image=IMG5),
            ]),
        ],
    ),
}


def iso(unix):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(unix))


def build_batch(session, now, batch_seq, dropped, events):
    s = SESSIONS[session]
    return {
        "schema": 1,
        "disclosure_version": 1,
        "release": s["release"],
        "installation_id": s["installation"],
        "session_id": s["session"],
        "session_start": iso(now - s["start_ago"]),
        "batch_seq": batch_seq,
        "dropped": dropped,
        "device": copy.deepcopy(DEVICES[s["device"]]),
        "events": copy.deepcopy(events),
    }


def batches(now):
    """Every request body to send, in order, as (session name, batch)."""
    out = []
    for name, s in SESSIONS.items():
        for batch_seq, dropped, events in s["batches"]:
            batch = build_batch(name, now, batch_seq, dropped, events)
            out.append((name, batch))
            if batch_seq in s.get("duplicate_batches", []):
                out.append((name, copy.deepcopy(batch)))
    return out


# ---------------------------------------------------------------------------------------------
# Expectations, computed from SESSIONS only.
# ---------------------------------------------------------------------------------------------

def all_events():
    """Unique events as dicts with session name and device, in (session, seq) order."""
    rows = []
    for name, s in SESSIONS.items():
        for batch_seq, dropped, events in s["batches"]:
            for e in events:
                rows.append(dict(e, session_name=name, batch_seq=batch_seq, installation=s["installation"],
                                 session=s["session"], device=s["device"], release=s["release"]))
    rows.sort(key=lambda r: (r["session_name"], r["seq"]))
    return rows


def percentile(values, p):
    """Linear interpolation between closest ranks (numpy's default, R-7, ClickHouse's
    quantileExactInclusive)."""
    v = sorted(values)
    if not v:
        return None
    pos = (len(v) - 1) * p
    lo = int(pos)
    hi = min(lo + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (pos - lo)


def cohort_label(session_name, field):
    device_key = SESSIONS[session_name]["device"]
    d = DEVICES[device_key]
    if field == "os":
        return f"{d['os']['family']} {d['os']['version'][0]}"
    if field == "cpu":
        c = d["cpu"]
        if c["vendor"] == "apple":
            return f"apple gen{c['generation']} {c['tier']}"
        return f"{c['vendor']} family {c['family']} model {c['model']}"
    if field == "memory_gib_band":
        return str(d["memory_gib_band"])
    if field == "renderer":
        return d["renderer"]
    if field == "gpu":
        g = d["gpus"][0]
        if g["vendor"] == "apple":
            return f"apple gen{g['generation']} {g['tier']} {g['backend']}"
        if "vendor_id" in g:
            return f"{g['vendor']} {g['type']} device {g['device_id']} {g['backend']}"
        return f"{g['vendor']} {g['type']} {g['backend']}"
    if field == "app":
        return SESSIONS[session_name]["release"].split("+")[0]
    raise KeyError(field)


def label(e):
    n = e["name"]
    detail = ""
    if n in ("app.presented", "app.idle"):
        detail = f": {e['state']}"
    elif n == "session.start":
        detail = f": {e['trigger']}"
    elif n == "workspace.enter":
        detail = f": {e['workspace']}"
    elif n == "interaction":
        detail = f": {e['feature']}"
    elif n == "tool.commit":
        detail = f": {e['tool']}"
    elif n == "operation.end":
        detail = f": {e['operation']} {e['outcome']}"
    elif n == "preview.settled":
        detail = f": {e['renderer']}"
    return n + detail


def active_ms(session_name):
    """Visible and not idle, between state events and up to the last received event."""
    events = [e for e in all_events() if e["session_name"] == session_name]
    visible = active = True
    last_t = 0
    total = 0
    for e in events:
        if e["name"] in ("app.presented", "app.idle"):
            if visible and active:
                total += e["t"] - last_t
            last_t = e["t"]
            if e["name"] == "app.presented":
                visible = e["state"] == "visible"
            else:
                active = e["state"] == "active"
    end = max(e["t"] for e in events)
    if visible and active:
        total += end - last_t
    return total


def ordered_match(session_name, predicates):
    """Whether the session's events contain the predicates as an ordered (not adjacent) subsequence."""
    i = 0
    for e in (e for e in all_events() if e["session_name"] == session_name):
        if i < len(predicates) and predicates[i](e):
            i += 1
    return i == len(predicates)


def funnel():
    added = lambda e: e["name"] == "images.added"
    develop = lambda e: e["name"] == "workspace.enter" and e["workspace"] == "develop"
    exported = lambda e: (e["name"] == "operation.end" and e["operation"] == "export"
                          and e["outcome"] == "completed")
    result = dict(sessions=len(SESSIONS), added=0, develop=0, exported=0)
    for name in SESSIONS:
        result["added"] += ordered_match(name, [added])
        result["develop"] += ordered_match(name, [added, develop])
        result["exported"] += ordered_match(name, [added, develop, exported])
    return result


def failures():
    """(operation, label of the preceding kept event, events missing just before the failure) per
    failed operation."""
    out = []
    for name in SESSIONS:
        events = [e for e in all_events() if e["session_name"] == name]
        for i, e in enumerate(events):
            if e["name"] == "operation.end" and e["outcome"] == "failed":
                prev = label(events[i - 1]) if i else "(start of known events)"
                before = e["seq"] - (events[i - 1]["seq"] if i else -1) - 1
                out.append((e["operation"], prev, before))
    return sorted(out)


def session_facts(name):
    s = SESSIONS[name]
    events = [e for e in all_events() if e["session_name"] == name]
    batches_seen = {b for b, _, _ in s["batches"]}
    last_batch = max(batches_seen)
    return dict(
        events=len(events),
        expected_events=max(e["seq"] for e in events) + 1,
        missing_events=max(e["seq"] for e in events) + 1 - len(events),
        batches=len(batches_seen),
        expected_batches=last_batch + 1,
        missing_batches=last_batch + 1 - len(batches_seen),
        dropped=sum(d for _, d, _ in s["batches"]),
        active_ms=active_ms(name),
        last_t=max(e["t"] for e in events),
    )
