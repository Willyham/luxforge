"""The real desktop against the reference stack, run by ../test.sh --client NEW_DIR.

It builds the release editor and `luxforge-ctl`, runs the xtask `usage-backend` scenario into
NEW_DIR/smoke with LUXFORGE_USAGE_INGRESS naming the stack's loopback ingress (the scenario's relay
forwards the client's requests to it byte for byte and records them in receiver.json), and then
checks, from what the client actually sent:

- storage: every batch the ingress accepted is stored exactly, event by event and column by column,
  with one profile per session and one loss counter per batch; nothing from a refused batch, a
  misrouted one or a negative fixture is stored, and the batch whose reply was withheld is stored
  once and was never sent again;
- dashboards: the session timeline, its gaps, the installation and per-image histories, through
  Grafana's own query API;
- negative fixtures built from the client's own batch and device fields: identifying or free-form
  additions, raw hashes and malformed shapes are refused before forwarding, with nothing stored;
- the registry: every `feature` and `tool` of the desktop's registry is accepted and stored as sent;
- retention: the client's rows expire on the stores' own engines with an accelerated TTL;
- privacy at every hop: the client's identifiers, image IDs and image keys, the machine's host and
  user names and home path, the photograph's file names and the client's User-Agent appear in no
  container log; stored rows hold no key, name, path, User-Agent or address; the client's own logs
  and frames hold no identifier or key; and no request body holds any of the machine's names.

Every capture is kept in NEW_DIR/hops for review. Usage: client_test.py PROJECT ENV_FILE PORT NEW_DIR
"""

import calendar
import copy
import getpass
import json
import os
import re
import socket
import subprocess
import sys
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.abspath(sys.argv[4])
sys.argv = sys.argv[:4]
sys.path.insert(0, HERE)
import scenario  # noqa: E402
import stack_test as st  # noqa: E402  (reads PROJECT, ENV_FILE and PORT from sys.argv)
from stack_test import ch_ok, ch_rows, ch_scalar, check, dc, panel, post_batch, variable_values, wait_for  # noqa: E402

STACK = st.STACK
REPO = os.path.abspath(os.path.join(STACK, "..", ".."))
SMOKE = os.path.join(OUT, "smoke")
HOPS = os.path.join(OUT, "hops")
SCAN = os.path.join(STACK, "scan-logs.py")
TEST_RELEASE = "0.0.0+000000e71de5"
EV_COLS = ["trigger", "state", "workspace", "image", "feature", "tool", "count", "operation", "outcome",
           "duration_ms", "renderer", "cpu_percent", "memory_mib", "memory_kind", "gpu_percent", "gpu_memory_mib"]
TABLES = ("events", "profiles", "batches")
STARTED = time.time()


def new_id():
    return str(uuid.uuid4())


def sh(cmd, env=None, timeout=3600):
    print("$ " + " ".join(cmd), flush=True)
    return subprocess.run(cmd, cwd=REPO, env=env, timeout=timeout)


def load_requests():
    received = json.load(open(os.path.join(SMOKE, "receiver.json")))
    requests = received["requests"]
    for r in requests:
        r["batch"] = json.loads(r["body"])
    return requests


def prefs(path):
    return json.load(open(os.path.join(SMOKE, path, "preferences.json")))["usage_statistics"]


# ------------------------------------------------------------------------------------------------
# Expected rows, from what the client sent
# ------------------------------------------------------------------------------------------------

def db_time(text):
    return text.replace("T", " ").rstrip("Z")


def event_rows(batch):
    rows = {}
    for e in batch["events"]:
        row = {"installation_id": batch["installation_id"], "session_id": batch["session_id"],
               "session_start": db_time(batch["session_start"]), "batch_seq": batch["batch_seq"],
               "seq": e["seq"], "t": e["t"], "name": e["name"], "release": batch["release"]}
        for c in EV_COLS:
            row[c] = e.get(c)
        rows[(batch["session_id"], e["seq"])] = row
    return rows


def profile_row(batch):
    d = batch["device"]
    os_, cpu = d["os"], d["cpu"]
    row = {"installation_id": batch["installation_id"], "session_id": batch["session_id"],
           "release": batch["release"], "batch_seq": batch["batch_seq"], "dropped": batch["dropped"],
           "os_family": os_["family"], "os_version": os_["version"], "os_build": os_.get("build"),
           "os_distro": os_.get("distro"), "os_distro_version": os_.get("distro_version", []),
           "os_kernel": os_.get("kernel", []), "arch": d["arch"], "cpu_vendor": cpu["vendor"],
           "cpu_physical_cores": cpu["physical_cores"], "cpu_logical_cores": cpu["logical_cores"],
           "cpu_generation": cpu.get("generation"), "cpu_tier": cpu.get("tier"), "cpu_family": cpu.get("family"),
           "cpu_model": cpu.get("model"), "cpu_stepping": cpu.get("stepping"),
           "memory_gib_band": str(d["memory_gib_band"]), "gpu_count": len(d["gpus"]),
           "renderer": d["renderer"], "renderer_fallback": d.get("renderer_fallback")}
    for i in range(2):
        g = d["gpus"][i] if i < len(d["gpus"]) else None
        for k in ("vendor", "backend", "type", "unified_memory", "vendor_id", "device_id", "generation", "tier"):
            row[f"gpu{i}_{k}"] = g.get(k) if g else None
        band = g.get("memory_gib_band") if g else None
        row[f"gpu{i}_memory_gib_band"] = None if band is None else str(band)
        row[f"gpu{i}_driver_version"] = (g.get("driver_version") if g else None) or []
    return row


# ------------------------------------------------------------------------------------------------
# Sections
# ------------------------------------------------------------------------------------------------

def storage_checks(requests):
    accepted = [r["batch"] for r in requests if r["ingress"].get("status") == 202]
    refused = [r["batch"] for r in requests if r["ingress"].get("status") != 202]
    want = {}
    for b in accepted:
        want.update(event_rows(b))
    installations = sorted({r["batch"]["installation_id"] for r in requests})
    where = "installation_id IN (" + ", ".join(f"'{i}'" for i in installations) + ")"
    wait_for(lambda: int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE {where}")) >= len(want), 60)
    time.sleep(3)  # a duplicate or stray row would show by now
    raw = int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE {where}"))
    rows = ch_rows(f"SELECT * FROM luxforge.events_unique WHERE {where} ORDER BY session_id, seq")
    got = {(r["session_id"], r["seq"]): r for r in rows}
    check(f"the {len(accepted)} batches the ingress accepted are stored as exactly their {len(want)} events, "
          "each once (the batch whose reply was withheld included, and never resent)",
          set(got) == set(want) and raw == len(want), f"stored {len(got)} unique / {raw} rows, expected {len(want)}")
    bad = []
    for key, w in want.items():
        r = got.get(key)
        if r is None:
            continue
        for c, v in w.items():
            if r[c] != v:
                bad.append(f"{key[1]}.{c}: {r[c]!r} != {v!r}")
        if abs(calendar.timegm(time.strptime(r["received_at"], "%Y-%m-%d %H:%M:%S")) - time.time()) > 3600:
            bad.append(f"received_at not recent: {r['received_at']}")
    check("every stored event row equals the client's event column by column (IDs, session start, batch, seq, "
          "time, name, release, image IDs and every field)", not bad, "; ".join(bad[:6]))
    leaked = [(b["session_id"], e["seq"]) for b in refused for e in b["events"]
              if (b["session_id"], e["seq"]) in got and (b["session_id"], e["seq"]) not in want]
    stray_installation = refused[-1]["installation_id"] if refused else None
    stray = int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE installation_id = '{stray_installation}'")) if stray_installation else 0
    check("no event of a batch the ingress refused (the 429 and the misrouted 404) is stored", not leaked and stray == 0,
          f"{leaked[:4]} / misrouted installation rows {stray}")

    last = {}
    for b in accepted:
        if b["session_id"] not in last or b["batch_seq"] > last[b["session_id"]]["batch_seq"]:
            last[b["session_id"]] = b
    profiles = {r["session_id"]: r for r in ch_rows(f"SELECT * FROM luxforge.profiles FINAL WHERE {where}")}
    pbad = []
    for s, b in last.items():
        r = profiles.get(s)
        if r is None:
            pbad.append(f"no profile for {s}")
            continue
        for c, v in profile_row(b).items():
            if r[c] != v:
                pbad.append(f"{s[:8]}.{c}: {r[c]!r} != {v!r}")
    check(f"one stored profile per session ({len(last)}), every column equal to the client's device context "
          "of its latest accepted batch", not pbad and set(profiles) == set(last), "; ".join(pbad[:6]))
    batches = {(r["session_id"], r["batch_seq"]): r["dropped"]
               for r in ch_rows(f"SELECT * FROM luxforge.batches_unique WHERE {where}")}
    want_b = {(b["session_id"], b["batch_seq"]): b["dropped"] for b in accepted}
    check("one loss counter per accepted batch, as the client reported it", batches == want_b, f"{batches} != {want_b}")
    return accepted


def dashboard_checks(requests):
    """Requests, in order: work's accepted and withheld batches; again's accepted, refused (429),
    accepted and rotated batches; rejected's misrouted one."""
    first = requests[2]["batch"]
    session = first["session_id"]
    evs = [e for i in (2, 4) for e in requests[i]["batch"]["events"]]
    tl = panel("luxforge-session", "Timeline", {"session": session})
    missing, prev = [], -1
    for e in evs:
        missing.append(e["seq"] - prev - 1)
        prev = e["seq"]
    refused = len(requests[3]["batch"]["events"])
    start_ms = time_ms(first["session_start"])
    check("session timeline of the client's second launch: order, labels, wall-clock times, and the refused "
          f"batch's {refused} events as the gap before the next",
          [r["seq"] for r in tl] == [e["seq"] for e in evs] and [r["label"] for r in tl] == [scenario.label(e) for e in evs]
          and [r["missing before"] for r in tl] == missing and [r["wall time"] for r in tl] == [start_ms + e["t"] for e in evs]
          and sum(missing) == refused and refused >= 1,
          f"{[(r['seq'], r['label'], r['missing before']) for r in tl]}")
    summary = panel("luxforge-session", "Session summary", {"session": session})[0]
    check("session summary: events received and missing, two batches received and the refused one missing",
          summary["events received"] == len(evs) and summary["events missing"] == refused
          and summary["batches received"] == 2 and summary["batches missing"] == 1, str(summary))
    batches = panel("luxforge-session", "Batches", {"session": session})
    check("session batches: 0 and 2 arrived, 1 (refused) did not", [r["batch"] for r in batches] == [0, 2], str(batches))
    work = requests[0]["batch"]
    installation_a = work["installation_id"]
    installation_b = requests[5]["batch"]["installation_id"]
    image_a = next(e["image"] for e in work["events"] if "image" in e)
    image_b = next(e["image"] for e in requests[5]["batch"]["events"] if "image" in e)
    installs = {r["__value"] for r in variable_values("luxforge-history", "installation")}
    check("the installation variable lists both of the client's installations",
          {installation_a, installation_b} <= installs, str(installs))
    imgs = sorted(r["__value"] for r in variable_values("luxforge-history", "image", {"installation": installation_a}))
    check("the first installation has exactly one image, the photograph's", imgs == [image_a], str(imgs))
    v = {"installation": installation_a, "image": image_a}
    sess = panel("luxforge-history", "Sessions of this installation", v)
    want_sessions = [work["session_id"], first["session_id"]]
    check("installation history: the first installation's two sessions (two launches) in order",
          [r["session"] for r in sess] == want_sessions, str([r["session"] for r in sess]))
    hist = panel("luxforge-history", "History of the selected image across sessions", v)
    stored = [r["batch"] for r in requests if r["ingress"].get("status") == 202
              and r["batch"]["installation_id"] == installation_a]
    want_h = [(b["session_id"][:8], e["seq"], scenario.label(e)) for b in stored for e in b["events"]
              if e.get("image") == image_a]
    got_h = [(r["session"], r["seq"], r["label"]) for r in hist]
    check("image history: the photograph's events across both launches, in order", got_h == want_h, f"{got_h} vs {want_h}")
    rotated = panel("luxforge-history", "History of the selected image across sessions",
                    {"installation": installation_b, "image": image_a})
    check("after off and on, the new installation has no history under the old image ID", rotated == [], str(rotated))
    new = panel("luxforge-history", "History of the selected image across sessions",
                {"installation": installation_b, "image": image_b})
    check("the new installation's image ID shows only its own events",
          [r["seq"] for r in new] == [e["seq"] for e in requests[5]["batch"]["events"] if e.get("image") == image_b], str(new))


def time_ms(text):
    return calendar.timegm(time.strptime(text, "%Y-%m-%dT%H:%M:%SZ")) * 1000


def negative_checks(base):
    """Fixtures built from the client's own first batch and device context."""
    def fresh(batch=None):
        b = copy.deepcopy(batch or base)
        b["installation_id"], b["session_id"] = new_id(), new_id()
        return b

    def mutate(change):
        b = fresh()
        change(b)
        return b

    host, user, home = socket.gethostname(), getpass.getuser(), os.path.expanduser("~")
    image = next(e["image"] for e in base["events"] if "image" in e)
    sessions = []

    def send(b, **kw):
        sessions.append(b["session_id"] if isinstance(b, dict) else None)
        return post_batch(b if isinstance(b, dict) else None, **kw)

    control = fresh()
    check("control: the client's first batch under fresh IDs is accepted (202)", post_batch(control) == 202)
    gpu0 = lambda b: b["device"]["gpus"][0]  # noqa: E731
    cases = [
        ("the machine's host name in the device", lambda b: b["device"].update({"hostname": host})),
        ("the user name in the envelope", lambda b: b.update({"user": user})),
        ("a client IP address in the envelope", lambda b: b.update({"ip": "203.0.113.9"})),
        ("a second identifier (machine UUID)", lambda b: b.update({"machine_id": new_id()})),
        ("a raw OS version string", lambda b: b["device"]["os"].update({"version": f"{'.'.join(map(str, b['device']['os']['version']))} (Darwin)"})),
        ("a free-form macOS build", lambda b: b["device"]["os"].update({"build": "Darwin Kernel Version 25.5.0"})),
        ("a raw CPU brand string", lambda b: b["device"]["cpu"].update({"brand": "Apple M4 Pro"})),
        ("exact installed memory instead of a band", lambda b: b["device"].update({"memory_gib_band": 48})),
        ("a raw GPU adapter name", lambda b: gpu0(b).update({"name": "Apple M4 Pro"}) if b["device"]["gpus"] else b["device"].update({"gpu": "x"})),
        ("a GPU adapter UUID", lambda b: gpu0(b).update({"uuid": new_id()}) if b["device"]["gpus"] else b["device"].update({"gpu": "x"})),
        ("three GPUs", lambda b: b["device"].update({"gpus": [{"vendor": "apple", "backend": "metal", "type": "integrated", "unified_memory": True, "generation": n} for n in (1, 2, 3)]})),
        ("a locale", lambda b: b["device"].update({"locale": "en_GB"})),
        ("an installation ID that is not a random v4 UUID", lambda b: b.update({"installation_id": str(uuid.uuid1())})),
        ("a dirty release", lambda b: b.update({"release": TEST_RELEASE + "-dirty"})),
        ("a session start with a time zone offset", lambda b: b.update({"session_start": b["session_start"].replace("Z", "+01:00")})),
        ("the photograph's file name on an event", lambda b: b["events"][-1].update({"file_name": "orientation-1.jpg"})),
        ("a local path on an event", lambda b: b["events"][-1].update({"path": os.path.join(home, "Pictures", "a.jpg")})),
        ("an editing value on a tool commit", lambda b: next(e for e in b["events"] if e["name"] == "tool.commit").update({"value": 0.4})),
        ("an unkeyed SHA-256 of the photograph as its image ID", lambda b: next(e for e in b["events"] if "image" in e).update({"image": "ab" * 32})),
        ("an upper-case image ID", lambda b: next(e for e in b["events"] if "image" in e).update({"image": image.upper()})),
        ("an error message on an operation", lambda b: next(e for e in b["events"] if e["name"] == "operation.end").update({"error": "No such file"})),
        ("an unknown event name", lambda b: b["events"].append({"seq": b["events"][-1]["seq"] + 1, "t": b["events"][-1]["t"], "name": "photo.metadata"})),
        ("a feature outside the registry", lambda b: next(e for e in b["events"] if e["name"] == "interaction").update({"feature": "preset:My Holiday"})),
        ("a tool outside the registry", lambda b: next(e for e in b["events"] if e["name"] == "tool.commit").update({"tool": "proof"})),
        ("events out of order", lambda b: b["events"].reverse()),
        ("a repeated sequence number", lambda b: b["events"][1].update({"seq": b["events"][0]["seq"]})),
        ("a count on images.added over a million", lambda b: b["events"].append({"seq": b["events"][-1]["seq"] + 1, "t": b["events"][-1]["t"], "name": "images.added", "count": 1000001})),
        ("schema 2", lambda b: b.update({"schema": 2})),
        ("disclosure 2", lambda b: b.update({"disclosure_version": 2})),
        ("513 events", lambda b: b.update({"events": [{"seq": i, "t": i, "name": "app.idle", "state": "idle"} for i in range(513)]})),
    ]
    statuses = {}
    for what, change in cases:
        statuses[what] = send(mutate(change))
    wrong = {k: v for k, v in statuses.items() if v != 400}
    check(f"{len(cases)} negative fixtures built from the client's own batch and device fields are refused with 400",
          not wrong, str(wrong))
    dup_session = new_id()
    text = json.dumps(fresh() | {"session_id": dup_session})
    duplicate = text.replace('"schema": 1', '"schema": 1, "installation_id": "%s"' % new_id(), 1).encode()
    sessions.append(dup_session)
    check("a repeated envelope key is refused (400)", post_batch(None, raw=duplicate) == 400)
    big = fresh()
    sessions.append(big["session_id"])
    raw = json.dumps(big).encode()
    padded = raw[:-1] + b"," + b'"pad":"' + b"x" * 65536 + b'"}'
    check("the client's batch padded past 65,536 bytes is refused (413)", post_batch(None, raw=padded) == 413)
    plain = fresh()
    sessions.append(plain["session_id"])
    check("the client's batch as text/plain is refused (415)",
          post_batch(None, raw=json.dumps(plain).encode(), headers={"Content-Type": "text/plain"}) == 415)
    time.sleep(4)
    ids = ", ".join(f"'{s}'" for s in sessions if s)
    stored = {t: int(ch_scalar(f"SELECT count() FROM luxforge.{t} WHERE session_id IN ({ids})")) for t in TABLES}
    check("nothing of any refused fixture reached Vector or ClickHouse", stored == {t: 0 for t in TABLES}, str(stored))
    check("the control batch is stored",
          wait_for(lambda: int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE session_id = '{control['session_id']}'")) == len(control["events"]), 30))
    return control


def registry_checks(base):
    core = open(os.path.join(REPO, "crates/luxforge-core/src/usage/registry.json"), "rb").read()
    ingress = open(os.path.join(REPO, "services/usage-ingest/registry.json"), "rb").read()
    check("the ingress's registry.json is the desktop's byte for byte", core == ingress)
    registry = json.loads(core)
    image = next(e["image"] for e in base["events"] if "image" in e)
    events = [{"seq": 0, "t": 0, "name": "session.start", "trigger": "launch"}]
    for f in registry["feature"]:
        events.append({"seq": len(events), "t": len(events), "name": "interaction", "feature": f, "image": image})
    for t in registry["tool"]:
        events.append({"seq": len(events), "t": len(events), "name": "tool.commit", "tool": t, "image": image})
    b = copy.deepcopy(base)
    b.update({"installation_id": new_id(), "session_id": new_id(), "batch_seq": 0, "dropped": 0, "events": events})
    check(f"a batch with every registry feature ({len(registry['feature'])}) and tool ({len(registry['tool'])}) "
          "under the client's device context is accepted", post_batch(b) == 202)
    ok = wait_for(lambda: int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE session_id = '{b['session_id']}'")) == len(events), 30)
    rows = ch_rows(f"SELECT feature, tool FROM luxforge.events_unique WHERE session_id = '{b['session_id']}' ORDER BY seq")
    check("every registry feature and tool is stored as sent",
          ok and [r["feature"] for r in rows if r["feature"]] == registry["feature"]
          and [r["tool"] for r in rows if r["tool"]] == registry["tool"], f"{len(rows)} rows")
    return b["session_id"]


def retention_checks(session):
    ch_ok("DROP DATABASE IF EXISTS luxforge_clientttl")
    ch_ok("CREATE DATABASE luxforge_clientttl")
    for t in TABLES:
        ddl = ch_scalar(f"SHOW CREATE TABLE luxforge.{t}").replace("\\n", "\n").replace("\\'", "'")
        ddl = ddl.replace(f"CREATE TABLE luxforge.{t}", f"CREATE TABLE luxforge_clientttl.{t}")
        ch_ok(ddl.replace("TTL received_at + toIntervalDay(90)", "TTL received_at + toIntervalSecond(10)"))
        ch_ok(f"INSERT INTO luxforge_clientttl.{t} SELECT * REPLACE (now() AS received_at) FROM luxforge.{t} WHERE session_id = '{session}'")
    before = {t: int(ch_scalar(f"SELECT count() FROM luxforge_clientttl.{t}")) for t in TABLES}
    time.sleep(12)
    for t in TABLES:
        ch_ok(f"OPTIMIZE TABLE luxforge_clientttl.{t} FINAL")
    after = {t: int(ch_scalar(f"SELECT count() FROM luxforge_clientttl.{t}")) for t in TABLES}
    check("accelerated expiry: copies of the client's event, profile and batch rows on the stores' own DDL with a "
          "10-second TTL are removed by a merge", all(v > 0 for v in before.values()) and all(v == 0 for v in after.values()),
          f"{before} -> {after}")
    ch_ok("DROP DATABASE luxforge_clientttl")
    old = new_id()
    ch_ok(f"INSERT INTO luxforge.events SELECT * REPLACE (now() - INTERVAL 91 DAY AS received_at, toUUID('{old}') AS session_id) "
          f"FROM luxforge.events WHERE session_id = '{session}'")
    check("a copy of the client's rows received 91 days ago is not kept",
          int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE session_id = '{old}'")) == 0)


def scan(name, text, needles, allow=()):
    """Run scan-logs.py over `text`, keeping the capture in hops/."""
    os.makedirs(HOPS, exist_ok=True)
    path = os.path.join(HOPS, name)
    with open(path, "w") as f:
        f.write(text)
    args = [sys.executable, "-I", SCAN]
    for label, value in needles:
        args += ["--needle", f"{label}={value}"]
    for a in allow:
        args += ["--allow", a]
    r = subprocess.run(args + [path], capture_output=True, text=True)
    return r.returncode, (r.stdout + r.stderr).strip().splitlines()


def privacy_checks(requests):
    batches = [r["batch"] for r in requests]
    installations = sorted({b["installation_id"] for b in batches})
    sessions = sorted({b["session_id"] for b in batches})
    images = sorted({e["image"] for b in batches for e in b["events"] if "image" in e})
    keys = []
    for config in ("work/host/config", "shared/host/config", "rejected/host/config"):
        key = prefs(config).get("image_key")
        if key:
            keys.append(key)
    check("the client's configurations hold an image key for each installation (3)", len(set(keys)) == 3, str(len(keys)))
    agent = next(v for k, v in requests[0]["headers"] if k == "user-agent")
    # The bare user name is searched for only in the request bodies, which are all enums, numbers and
    # IDs: in a log it can be an ordinary word ("will"). The home path holds it everywhere else.
    machine = [("hostname", socket.gethostname()), ("home", os.path.expanduser("~")),
               ("fixture", "orientation-1"), ("export", "usage-backend-export"), ("repository", REPO)]
    machine = [(k, v) for k, v in machine if len(v) >= 4]
    user = [("user", getpass.getuser())] if len(getpass.getuser()) >= 4 else []
    secret = [(f"image-key-{i}", k) for i, k in enumerate(keys)]
    ids = ([(f"installation-{i}", v) for i, v in enumerate(installations)] + [(f"session-{i}", v) for i, v in enumerate(sessions)]
           + [(f"image-{i}", v) for i, v in enumerate(images)])

    # The request bodies: no key, name, path or User-Agent.
    bodies = "\n".join(r["body"] for r in requests)
    rc, lines = scan("request-bodies.txt", bodies, machine + user + secret + [("user-agent", agent)])
    check("no request body holds an image key, the machine's host or user name, a home or repository path or a "
          "file name", rc == 0, "\n".join(lines[-4:]))
    check("the client's User-Agent is a fixed product string with no host, user or path",
          not any(v in agent for _, v in machine + user), agent)

    # Every container's log: nothing of the client, and no address.
    nets = subprocess.run(["docker", "network", "ls", "--filter", f"label=com.docker.compose.project={st.PROJECT}", "-q"],
                          capture_output=True, text=True).stdout.split()
    networks = json.loads(subprocess.run(["docker", "network", "inspect", *nets], capture_output=True, text=True).stdout)
    addresses = set()
    for n in networks:
        for c in n.get("IPAM", {}).get("Config") or []:
            if c.get("Gateway"):
                addresses.add(c["Gateway"])
        for c in n.get("Containers", {}).values():
            addresses.add(c["IPv4Address"].split("/")[0])
    passwords = [(f"password-{k.lower()}", st.ENV[f"LUXFORGE_CLICKHOUSE_{k}_PASSWORD"]) for k in ("ADMIN", "WRITER", "READER")]
    passwords.append(("password-grafana", st.ENV["LUXFORGE_GRAFANA_ADMIN_PASSWORD"]))
    needles = ids + secret + machine + [("user-agent", agent)] + passwords
    for service in ("ingress", "vector", "clickhouse", "grafana"):
        r = dc("logs", "--no-color", "--no-log-prefix", service)
        text = r.stdout + r.stderr
        rc, lines = scan(f"log-{service}.txt", text or "(empty)\n", needles)
        check(f"{service} log: none of the client's {len(installations)} installation, {len(sessions)} session and "
              f"{len(images)} image IDs, its image keys, the machine's names, its User-Agent, a password or any IP "
              f"address (Docker assigned {len(addresses)})",
              rc == 0 and not any(a in text for a in addresses), "\n".join(lines[-4:]))
    ingress_lines = [l for l in dc("logs", "--no-color", "--no-log-prefix", "ingress").stdout.splitlines() if l.strip()]
    check("the ingress logged only its fixed messages and aggregate counters",
          ingress_lines and all(l.startswith("usage-ingest ") for l in ingress_lines), "\n".join(ingress_lines[:3]))
    stats = st.stats_line()
    check("the ingress's counters saw the journey: accepted, a per-installation 429, a 404 and the refusals",
          stats.get("accepted", 0) >= 5 and stats.get("limited_id", 0) >= 1 and stats.get("not_found", 0) >= 1
          and stats.get("bad_request", 0) >= 30, str(stats))

    # Stored rows: IDs and image IDs by design, nothing else of the machine or the request.
    for t in TABLES:
        rc, lines = scan(f"table-{t}.tsv", ch_ok(f"SELECT * FROM luxforge.{t} FORMAT TSV") or "(empty)\n",
                         secret + machine + [("user-agent", agent)])
        check(f"stored {t}: no image key, machine name, path, file name, User-Agent or IP address", rc == 0,
              "\n".join(lines[-4:]))

    # The client's own hop: its logs and frames hold no identifier, image ID or key (its
    # preferences file holds the installation ID and key by design; receiver.json is the capture).
    leaks = []
    files = 0
    for launch in ("work", "again", "rejected"):
        root = os.path.join(SMOKE, launch)
        for dirpath, _, names in os.walk(root):
            for name in names:
                path = os.path.join(dirpath, name)
                if name == "preferences.json" or not name.endswith((".json", ".jsonl", ".log", ".txt", ".md")):
                    continue
                files += 1
                text = open(path, encoding="utf-8", errors="replace").read()
                leaks += [f"{os.path.relpath(path, SMOKE)}: {label}" for label, v in ids + secret if v in text]
    check(f"the desktop's own logs and frames ({files} files) hold none of its installation, session or image IDs "
          "or image keys", files > 0 and not leaks, "; ".join(leaks[:5]))
    # The infrastructure under the containers: Docker Desktop's own host and VM logs, written during
    # the run. Searched for the client's identifiers, keys and User-Agent (they describe the VM's
    # own networking, so addresses are not searched for here).
    logs = os.path.expanduser("~/Library/Containers/com.docker.docker/Data/log")
    if os.path.isdir(logs):
        recent = [os.path.join(d, n) for d, _, ns in os.walk(logs) for n in ns
                  if os.path.getmtime(os.path.join(d, n)) >= STARTED]
        text = "".join(open(f, encoding="utf-8", errors="replace").read() for f in recent)
        found = [label for label, v in ids + secret + [("user-agent", agent)] if v in text]
        check(f"Docker Desktop's host and VM logs written during the run ({len(recent)} files) hold none of the "
              "client's identifiers, image keys or User-Agent", recent and not found, str(found))
    else:
        print("SKIP  Docker Desktop's host and VM logs: not on this host (not a pass)")
    rows = ch_scalar("SELECT name FROM system.tables WHERE database = 'system' AND (name LIKE '%\\_log' OR name LIKE '%\\_log\\_%')")
    check("ClickHouse kept no query or other system log of the inserts", rows == "", rows)


def main():
    print(f"== client test, project {st.PROJECT}, ingress on 127.0.0.1:{st.PORT}, output {OUT}")
    os.makedirs(OUT)
    st.static_checks()
    # The editor is built before the stack starts, so the containers run only while needed.
    cargo = os.path.join(REPO, "tools", "cargo-cached")
    built = sh([cargo, "build", "--release", "-p", "luxforge-app", "-p", "luxforge-cli"])
    if not check("the release editor and luxforge-ctl build", built.returncode == 0):
        return finish()
    dc("up", "-d", "--build", "--wait", "--wait-timeout", "300")
    check("ingress accepts connections", wait_for(lambda: post_batch(None, raw=b"{}") == 400, 60))
    wait_for(lambda: st.grafana_json("/api/health")[0] == 200, 60)
    env = dict(os.environ, LUXFORGE_USAGE_INGRESS=f"http://127.0.0.1:{st.PORT}")
    ran = sh([cargo, "xtask", "smoke", "--scenario", "usage-backend", "--output", SMOKE], env=env)
    check(f"the desktop's usage-backend scenario passes its client-side checks against the stack ({SMOKE})",
          ran.returncode == 0)
    if not os.path.exists(os.path.join(SMOKE, "receiver.json")):
        return finish()
    requests = load_requests()
    check("the relay recorded the client's seven requests", len(requests) == 7, str(len(requests)))
    if len(requests) != 7:
        return finish()
    for section in (lambda: storage_checks(requests), lambda: dashboard_checks(requests)):
        try:
            section()
        except Exception as e:  # a section that cannot run is a failure, and the rest still run
            check(f"section ran: {e}", False)
    try:
        negative_checks(requests[0]["batch"])
        registry_checks(requests[0]["batch"])
        retention_checks(requests[0]["batch"]["session_id"])
    except Exception as e:
        check(f"section ran: {e}", False)
    try:
        privacy_checks(requests)
    except Exception as e:
        check(f"privacy section ran: {e}", False)
    return finish()


def finish():
    results = st.results
    failed = [r for r in results if not r[1]]
    print(f"\n== {len(results) - len(failed)} passed, {len(failed)} failed, {len(results)} checks")
    for name, _, detail in failed:
        print(f"FAILED: {name}\n    {detail}")
    with open(os.path.join(OUT, "client-test.json"), "w") as f:
        json.dump({"status": "passed" if not failed and results else "failed",
                   "checks": [{"name": n, "passed": ok, "detail": d if not ok else ""} for n, ok, d in results]}, f, indent=1)
    return 1 if failed or not results else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print(f"\n== aborted: {e}")
        try:
            finish()
        except Exception:
            pass
        sys.exit(2)
