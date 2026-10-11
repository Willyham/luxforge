"""End-to-end test of the usage statistics reference stack, run by ../test.sh.

It talks to the real containers: synthetic batches go to the published ingress; every other check
goes through `docker compose exec` into the private networks. Nothing here uses a real report.

Usage: stack_test.py PROJECT ENV_FILE INGRESS_PORT
"""

import base64
import calendar
import json
import math
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
STACK = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import scenario  # noqa: E402

PROJECT, ENV_FILE, PORT = sys.argv[1], sys.argv[2], int(sys.argv[3])
ENV = dict(line.split("=", 1) for line in open(ENV_FILE).read().splitlines()
           if line and not line.startswith("#"))
INGRESS = f"http://127.0.0.1:{PORT}/v1/usage/batches"
DASHBOARDS = os.path.join(STACK, "grafana", "dashboards")

# Values that must never appear in any container log or stored row.
SENTINEL_XFF = "203.0.113.77"
SENTINEL_REAL_IP = "198.51.100.23"
SENTINEL_UA = "SentinelAgent/9.9 (do-not-log)"
SENTINEL_COOKIE = "luxforge_sentinel_cookie=c00k1e-5ent1nel"
SENTINEL_HEADER = "hdr-5ent1nel-7788"
SENTINEL_PAYLOAD = "RAW-PAYLOAD-SENTINEL-4242"

results = []


def check(name, ok, detail=""):
    results.append((name, bool(ok), detail))
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f"\n        {detail}"), flush=True)
    return bool(ok)


def close(a, b, tol=1e-6):
    return a is not None and b is not None and abs(float(a) - float(b)) <= tol * max(1.0, abs(float(b)))


# ------------------------------------------------------------------------------------------------
# Plumbing
# ------------------------------------------------------------------------------------------------

def dc(*args, check_rc=True, input=None, timeout=600):
    cmd = ["docker", "compose", "--env-file", ENV_FILE, "-p", PROJECT, *args]
    r = subprocess.run(cmd, cwd=STACK, capture_output=True, text=True, input=input, timeout=timeout)
    if check_rc and r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd)} failed: {r.stderr[-2000:]}")
    return r


def ch(sql, fmt="TSV", user="admin"):
    """Run SQL as a ClickHouse user inside the clickhouse container (their passwords never leave it)."""
    var = {"admin": ("luxforge_admin", "LUXFORGE_CLICKHOUSE_ADMIN_PASSWORD"),
           "writer": ("luxforge_writer", "LUXFORGE_CLICKHOUSE_WRITER_PASSWORD"),
           "reader": ("luxforge_reader", "LUXFORGE_CLICKHOUSE_READER_PASSWORD")}[user]
    script = f'clickhouse-client -u {var[0]} --password "${var[1]}" --format {fmt} -q "$0"'
    return dc("exec", "-T", "clickhouse", "sh", "-c", script, sql, check_rc=False)


def ch_ok(sql, **kw):
    r = ch(sql, **kw)
    if r.returncode != 0:
        raise RuntimeError(f"clickhouse: {r.stderr[-1500:]}")
    return r.stdout


def ch_rows(sql, user="admin"):
    out = ch_ok(sql, fmt="JSONEachRow", user=user)
    return [json.loads(line) for line in out.splitlines() if line.strip()]


def ch_scalar(sql):
    return ch_ok(sql).strip()


def http(url, method="GET", headers=None, body=None, via="clickhouse"):
    """An HTTP request made from inside the private network (default: from the clickhouse container)."""
    cmd = ["exec", "-T", via, "wget", "-q", "-S", "-O-", "--content-on-error", "--tries=1", "-T", "30", "--max-redirect=0"]
    for k, v in (headers or {}).items():
        cmd += ["--header", f"{k}: {v}"]
    if body is not None:
        cmd += ["--header", "Content-Type: application/json", "--post-data", body]
    elif method != "GET":
        cmd += ["--method", method]
    cmd.append(url)
    r = dc(*cmd, check_rc=False)
    status = re.findall(r"HTTP/1\.[01] (\d{3})", r.stderr)
    return (int(status[-1]) if status else None), r.stdout


GRAFANA = "http://grafana:3000"


def auth_header(user=None, password=None):
    user = user or ENV["LUXFORGE_GRAFANA_ADMIN_USER"]
    password = password or ENV["LUXFORGE_GRAFANA_ADMIN_PASSWORD"]
    return {"Authorization": "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()}


def grafana_json(path, body=None):
    status, text = http(GRAFANA + path, headers=auth_header(), body=json.dumps(body) if body is not None else None)
    return status, (json.loads(text) if text.strip().startswith(("{", "[")) else text)


def post_batch(batch, headers=None, raw=None, url=INGRESS):
    data = raw if raw is not None else json.dumps(batch).encode()
    h = {"Content-Type": "application/json"}
    h.update(headers or {})
    req = urllib.request.Request(url, data=data, headers=h, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code
    except Exception as e:  # connection refused, timeout
        return repr(e)


SENTINEL_HEADERS = {
    "X-Forwarded-For": SENTINEL_XFF,
    "X-Real-IP": SENTINEL_REAL_IP,
    "User-Agent": SENTINEL_UA,
    "Cookie": SENTINEL_COOKIE,
    "X-Sentinel-Header": SENTINEL_HEADER,
}


def wait_for(pred, timeout=60, interval=1.0, what=""):
    end = time.time() + timeout
    while time.time() < end:
        try:
            if pred():
                return True
        except Exception:
            pass
        time.sleep(interval)
    return False


# ------------------------------------------------------------------------------------------------
# Dashboard panels, run through Grafana's own query API
# ------------------------------------------------------------------------------------------------

def load_dashboard(uid):
    return json.load(open(os.path.join(DASHBOARDS, f"{uid}.json")))


def substitute(sql, variables):
    for name, value in variables.items():
        sql = sql.replace("${%s:raw}" % name, value).replace("${%s}" % name, value)
    left = re.findall(r"\$\{[^}]+\}", sql)
    assert not left, f"unsubstituted variables {left}"
    return sql


def frames_to_rows(result):
    rows = []
    for frame in result.get("frames", []):
        names = [f["name"] for f in frame["schema"]["fields"]]
        cols = frame["data"]["values"]
        for i in range(len(cols[0]) if cols else 0):
            rows.append({n: cols[j][i] for j, n in enumerate(names)})
    return rows


def run_query(sql, fmt, ref="A"):
    now = int(time.time() * 1000)
    body = {"queries": [{"refId": ref, "datasource": {"uid": "luxforge-clickhouse", "type": "grafana-clickhouse-datasource"},
                         "editorType": "sql", "format": fmt, "queryType": "timeseries" if fmt == 0 else "table",
                         "rawSql": sql}],
            "from": str(now - 86400000), "to": str(now + 3600000)}
    status, out = grafana_json("/api/ds/query", body)
    if status != 200:
        return status, [], str(out)[:400]
    res = out["results"][ref]
    return status, frames_to_rows(res), res.get("error")


def panel(uid, title, variables=None):
    dash = load_dashboard(uid)
    p = next(p for p in dash["panels"] if p["title"] == title)
    t = p["targets"][0]
    status, rows, err = run_query(substitute(t["rawSql"], variables or {}), t["format"])
    if err or status != 200:
        raise RuntimeError(f"{uid}/{title}: {status} {err}")
    return rows


def variable_values(uid, name, variables=None):
    dash = load_dashboard(uid)
    v = next(v for v in dash["templating"]["list"] if v["name"] == name)
    status, rows, err = run_query(substitute(v["query"]["rawSql"], variables or {}), 1)
    if err or status != 200:
        raise RuntimeError(f"{uid} variable {name}: {status} {err}")
    return rows


# ------------------------------------------------------------------------------------------------
# Test sections
# ------------------------------------------------------------------------------------------------

NOW = int(time.time())
EVENTS = scenario.all_events()
SESS = {n: s["session"] for n, s in scenario.SESSIONS.items()}


def static_checks():
    cfg = json.loads(dc("config", "--format", "json").stdout)
    published = [(name, p.get("host_ip"), p.get("published"), p.get("target"))
                 for name, svc in cfg["services"].items() for p in svc.get("ports", [])]
    check("only the ingress publishes a port, on 127.0.0.1",
          len(published) == 1 and published[0][0] == "ingress" and published[0][1] == "127.0.0.1", str(published))
    for name, net in cfg["networks"].items():
        want_internal = name != "edge"
        check(f"network {name} is {'internal' if want_internal else 'the only non-internal network'}",
              bool(net.get("internal")) == want_internal, str(net))
    unpinned = [(n, s["image"]) for n, s in cfg["services"].items()
                if "build" not in s and "@sha256:" not in s["image"]]
    check("every pulled image is pinned by digest", not unpinned, str(unpinned))
    for df in ("ingress/Dockerfile", "grafana/Dockerfile"):
        froms = re.findall(r"^FROM (\S+)", open(os.path.join(STACK, df)).read(), re.M)
        check(f"{df}: every base image is pinned by digest", froms and all("@sha256:" in f for f in froms), str(froms))
    plugin = open(os.path.join(STACK, "grafana/Dockerfile")).read()
    check("grafana/Dockerfile pins the plugin version and SHA-256",
          re.search(r"PLUGIN_VERSION=\d+\.\d+\.\d+", plugin) and re.search(r"PLUGIN_SHA256=[0-9a-f]{64}", plugin))
    vector = open(os.path.join(STACK, "vector/vector.yaml")).read()
    check("vector: memory buffers only, no disk buffer or file/archive sink",
          "type: disk" not in vector and "type: file" not in vector and "type: memory" in vector)
    check(".env is ignored by git, .env.example is not",
          subprocess.run(["git", "check-ignore", "-q", ".env"], cwd=STACK).returncode == 0
          and subprocess.run(["git", "check-ignore", "-q", ".env.example"], cwd=STACK).returncode != 0)


def send_scenario():
    batches = scenario.batches(NOW)
    statuses = [post_batch(b, SENTINEL_HEADERS) for _, b in batches]
    check("every synthetic batch is accepted with 202", all(s == 202 for s in statuses), str(statuses))
    # Refusals carry sentinels too, so the logs prove they are not echoed.
    bad = dict(batches[0][1])
    bad["unexpected"] = SENTINEL_PAYLOAD
    check("a batch with an unknown key is refused with 400", post_batch(bad, SENTINEL_HEADERS) == 400)
    check("a non-JSON body is refused with 400",
          post_batch(None, SENTINEL_HEADERS, raw=SENTINEL_PAYLOAD.encode()) == 400)
    check("another path is 404",
          post_batch(None, SENTINEL_HEADERS, raw=b"{}", url=INGRESS.replace("/v1/usage/batches", "/v1/other")) == 404)
    expected = len(EVENTS)
    ok = wait_for(lambda: int(ch_scalar("SELECT count() FROM luxforge.events_unique")) >= expected, 60)
    time.sleep(2)  # a duplicate or stray row would show up by now
    got = int(ch_scalar("SELECT count() FROM luxforge.events_unique"))
    check("every synthetic event is stored exactly once (duplicate batch collapsed)", ok and got == expected,
          f"expected {expected}, got {got}")


def stored_rows():
    rows = ch_rows("SELECT * FROM luxforge.events_unique ORDER BY session_id, seq")
    by_key = {(r["session_id"], r["seq"]): r for r in rows}
    ev_cols = ["trigger", "state", "workspace", "image", "feature", "tool", "count", "operation", "outcome",
               "duration_ms", "renderer", "cpu_percent", "memory_mib", "memory_kind", "gpu_percent",
               "gpu_memory_mib"]
    bad = []
    for e in EVENTS:
        r = by_key.get((e["session"], e["seq"]))
        if r is None:
            bad.append(f"missing {e['session_name']}/{e['seq']}")
            continue
        for c in ev_cols:
            if r[c] != e.get(c):
                bad.append(f"{e['session_name']}/{e['seq']}.{c}: {r[c]!r} != {e.get(c)!r}")
        start = scenario.iso(NOW - scenario.SESSIONS[e["session_name"]]["start_ago"]).replace("T", " ").rstrip("Z")
        for c, want in (("installation_id", e["installation"]), ("name", e["name"]), ("t", e["t"]),
                        ("batch_seq", e["batch_seq"]), ("release", e["release"]), ("session_start", start)):
            if r[c] != want:
                bad.append(f"{e['session_name']}/{e['seq']}.{c}: {r[c]!r} != {want!r}")
        received = calendar.timegm(time.strptime(r["received_at"], "%Y-%m-%d %H:%M:%S"))
        if abs(received - time.time()) > 600:
            bad.append(f"received_at not recent: {r['received_at']}")
    check("stored event rows equal the sent events field by field", not bad, "; ".join(bad[:5]))

    profiles = {r["session_id"]: r for r in ch_rows("SELECT * FROM luxforge.session_profiles")}
    pbad = []
    for name, s in scenario.SESSIONS.items():
        d = scenario.DEVICES[s["device"]]
        r = profiles.get(s["session"])
        if r is None:
            pbad.append(f"no profile for {name}")
            continue
        want = {"installation_id": s["installation"], "release": s["release"], "os_family": d["os"]["family"],
                "arch": d["arch"], "cpu_vendor": d["cpu"]["vendor"], "cpu_physical_cores": d["cpu"]["physical_cores"],
                "memory_gib_band": str(d["memory_gib_band"]), "gpu_count": len(d["gpus"]), "renderer": d["renderer"],
                "renderer_fallback": d.get("renderer_fallback")}
        for k, v in want.items():
            if r[k] != v:
                pbad.append(f"{name}.{k}: {r[k]!r} != {v!r}")
    check("one profile per session with the sent device values", not pbad and len(profiles) == len(scenario.SESSIONS),
          "; ".join(pbad[:5]))
    raw = ch_rows("SELECT * FROM luxforge.profiles FINAL ORDER BY session_id")
    full = {r["session_id"]: r for r in raw}
    g = full[SESS["S3"]]
    check("a later batch replaces the session profile (batch_seq 2 kept for S3)", g["batch_seq"] == 2 and g["dropped"] == 4,
          str(g["batch_seq"]))
    check("device fields land in typed columns",
          full[SESS["S1"]]["os_version"] == [15, 5] and full[SESS["S1"]]["gpu0_unified_memory"] is True
          and full[SESS["S3"]]["gpu0_tier"] == "base" and full[SESS["S4"]]["gpu0_driver_version"] == [560, 94]
          and full[SESS["S4"]]["gpu0_vendor_id"] == 4318 and full[SESS["S5"]]["cpu_family"] == 23,
          json.dumps({k: full[SESS["S4"]][k] for k in ("gpu0_driver_version", "gpu0_vendor_id")}))

    batches = {(r["session_id"], r["batch_seq"]): r["dropped"] for r in ch_rows("SELECT * FROM luxforge.batches_unique")}
    want = {(s["session"], b): d for s in scenario.SESSIONS.values() for b, d, _ in s["batches"]}
    check("batch loss counters stored once per (session, batch_seq), duplicate batch not double-counted",
          batches == want, f"{batches} != {want}")


def allowlist_checks():
    cols = {}
    for r in ch_rows("SELECT table, name FROM system.columns WHERE database = 'luxforge' ORDER BY table, position"):
        cols.setdefault(r["table"], []).append(r["name"])
    events = ["received_at", "installation_id", "session_id", "session_start", "batch_seq", "seq", "t", "name",
              "release", "trigger", "state", "workspace", "image", "feature", "tool", "count", "operation",
              "outcome", "duration_ms", "renderer", "cpu_percent", "memory_mib", "memory_kind", "gpu_percent",
              "gpu_memory_mib"]
    check("events table has exactly the approved columns", cols["events"] == events, str(cols["events"]))
    profile_cols = cols["profile_batches"]
    check("profiles table has the same columns as the profile row, nothing more",
          cols["profiles"] == profile_cols and profile_cols[:6] == ["received_at", "installation_id", "session_id", "release", "batch_seq", "dropped"],
          str(cols["profiles"]))
    check("batches table holds only identifiers, receipt time and the loss counter",
          cols["batches"] == ["received_at", "installation_id", "session_id", "batch_seq", "dropped"])
    forbidden = re.compile(r"(^|_)(ip|addr|address|host|agent|header|cookie|body|payload|path|url|user|ua|geo|city|country)(_|$)")
    check("no stored column is an address, header, body, path or similar",
          not [c for cs in cols.values() for c in cs if forbidden.search(c)])
    tables = ch_rows("SELECT name, engine, total_rows FROM system.tables WHERE database = 'luxforge' AND engine NOT LIKE '%View' ORDER BY name")
    check("the landing table stores nothing (Null engine) and only the three stores exist",
          {t["name"]: t["engine"] for t in tables} == {"batches": "ReplacingMergeTree", "events": "ReplacingMergeTree",
                                                       "profile_batches": "Null", "profiles": "ReplacingMergeTree"},
          str(tables))
    other = ch_rows("SELECT database, name FROM system.tables WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema', 'luxforge')")
    check("no other user database or table exists", not other, str(other))
    logs = ch_rows("SELECT name FROM system.tables WHERE database = 'system' AND (name LIKE '%\\_log' OR name LIKE '%\\_log\\_%')")
    check("no ClickHouse system log table exists (no query log can hold rows)", not logs, str(logs))
    r = ch("SELECT count() FROM system.query_log")
    check("system.query_log does not exist", r.returncode != 0 and "UNKNOWN_TABLE" in r.stderr + r.stdout, r.stderr[-300:])
    ddl = {t: ch_scalar(f"SHOW CREATE TABLE luxforge.{t}") for t in ("events", "profiles", "batches")}
    check("every store has a 90-day TTL on received_at",
          all(re.search(r"TTL received_at \+ toIntervalDay\(90\)", d) for d in ddl.values()), str({k: v[-160:] for k, v in ddl.items()}))


def expected_cohort_rows(field):
    cohorts = {}
    for e in EVENTS:
        c = cohorts.setdefault(scenario.cohort_label(e["session_name"], field),
                               {"sessions": set(), "installations": set(), "events": 0, "preview": [], "cpu": [], "mem": [],
                                "export": [], "kinds": {}})
        c["sessions"].add(e["session"])
        c["installations"].add(e["installation"])
        c["events"] += 1
        if e["name"] == "preview.settled":
            c["preview"].append(e["duration_ms"])
        if e["name"] == "process.sample":
            c["cpu"].append(e["cpu_percent"])
            c["mem"].append(e["memory_mib"])
            k = c["kinds"].setdefault(e["memory_kind"], {"cpu": [], "mem": []})
            k["cpu"].append(e["cpu_percent"])
            k["mem"].append(e["memory_mib"])
        if e["name"] == "operation.end" and e["operation"] == "export" and e["outcome"] == "completed":
            c["export"].append(e["duration_ms"])
    return cohorts


def cohort_checks():
    p = scenario.percentile
    for field in ("os", "cpu", "gpu", "memory_gib_band", "app", "renderer"):
        want = expected_cohort_rows(field)
        v = {"cohort": field, "operation": "export"}
        sizes = {r["cohort"]: r for r in panel("luxforge-performance", "Cohort sizes", v)}
        ok = set(sizes) == set(want) and all(
            sizes[k]["sessions"] == len(w["sessions"]) and sizes[k]["installations"] == len(w["installations"])
            and sizes[k]["events"] == w["events"] for k, w in want.items())
        check(f"cohort sizes by {field} equal the independent counts", ok, f"{sizes} vs {({k: (len(w['sessions']), len(w['installations']), w['events']) for k, w in want.items()})}")

        prev = {r["cohort"]: r for r in panel("luxforge-performance", "Preview settle time (ms) by cohort", v)}
        want_prev = {k: w["preview"] for k, w in want.items() if w["preview"]}
        ok = set(prev) == set(want_prev) and all(
            prev[k]["samples"] == len(vals) and close(prev[k]["p50"], round(p(vals, .5))) and close(prev[k]["p95"], round(p(vals, .95)))
            and close(prev[k]["p90"], round(p(vals, .9))) and close(prev[k]["p99"], round(p(vals, .99))) and prev[k]["max"] == max(vals)
            for k, vals in want_prev.items())
        check(f"preview settle percentiles by {field} are exact over the event rows", ok, f"{prev} vs {want_prev}")

        res = {(r["cohort"], r["memory_kind"]): r for r in panel("luxforge-performance", "CPU, memory and GPU samples by cohort", v)}
        want_res = {(k, kind): d for k, w in want.items() for kind, d in w["kinds"].items()}
        ok = set(res) == set(want_res) and all(
            res[k]["samples"] == len(d["cpu"]) and close(res[k]["cpu% p50"], round(p(d["cpu"], .5)))
            and close(res[k]["cpu% p95"], round(p(d["cpu"], .95))) and close(res[k]["mem MiB p95"], round(p(d["mem"], .95)))
            and res[k]["mem MiB max"] == max(d["mem"]) for k, d in want_res.items())
        check(f"CPU and memory percentiles by {field} (per memory kind) are exact over the event rows", ok, f"{res}")

        ops = {r["cohort"]: r for r in panel("luxforge-performance", "Completed operation duration (ms) by cohort, for the selected operation", v)}
        want_ops = {k: w["export"] for k, w in want.items() if w["export"]}
        ok = set(ops) == set(want_ops) and all(ops[k]["samples"] == len(d) and close(ops[k]["p50"], round(p(d, .5))) and ops[k]["max"] == max(d)
                                              for k, d in want_ops.items())
        check(f"completed export duration by {field} equals the independent values", ok, f"{ops} vs {want_ops}")

    # The GPU cohort carries the vendor's device id for non-Apple GPUs.
    allp = [e["duration_ms"] for e in EVENTS if e["name"] == "preview.settled"]
    daily = panel("luxforge-performance", "Preview settle time per day (all cohorts)")
    check("daily preview percentiles are computed from the day's individual events",
          len(daily) == 1 and close(daily[0]["p50"], p(allp, .5)) and close(daily[0]["p95"], p(allp, .95)),
          f"{daily} vs p50={p(allp, .5)} p95={p(allp, .95)}")
    check("per-batch percentiles are never averaged: a mean of per-session p95s would differ",
          not close(sum(p([e['duration_ms'] for e in EVENTS if e['name'] == 'preview.settled' and e['session_name'] == n], .95)
                        for n in scenario.SESSIONS if any(e['name'] == 'preview.settled' and e['session_name'] == n for e in EVENTS))
                    / 5, p(allp, .95)))
    by_cohort = panel("luxforge-performance", "Preview settle p95 per day by cohort", {"cohort": "os", "operation": "export"})
    check("per-cohort daily series returns data", len(by_cohort) >= 1, str(by_cohort))


def usage_checks():
    one = lambda t: next(iter(panel("luxforge-usage", t)[0].values()))
    facts = {n: scenario.session_facts(n) for n in scenario.SESSIONS}
    check("usage: sessions, installations and events", one("Sessions reported") == 5 and one("Installations reporting") == 4
          and one("Events received") == len(EVENTS))
    check("usage: known missing events, reported drops and missing batches",
          one("Events known missing") == sum(f["missing_events"] for f in facts.values()) == 3
          and one("Events the app reports dropping") == sum(f["dropped"] for f in facts.values()) == 6
          and one("Batches known missing") == sum(f["missing_batches"] for f in facts.values()) == 1)
    added = sum(e["count"] for e in EVENTS if e["name"] == "images.added")
    exported = sum(e["count"] for e in EVENTS if e["name"] == "operation.end" and e["operation"] == "export" and e["outcome"] == "completed")
    check("usage: images added and exported", one("Images added") == added == 65 and one("Images exported (completed exports)") == exported == 4)
    hours = sum(f["active_ms"] for f in facts.values()) / 3600000
    check("usage: active time from app.presented/app.idle", close(one("Active time, hours (reported sessions)"), hours),
          f"{one('Active time, hours (reported sessions)')} vs {hours}")
    check("usage: median active minutes per session",
          close(one("Median active minutes per session"), scenario.percentile([f["active_ms"] for f in facts.values()], .5) / 60000))
    daily = panel("luxforge-usage", "Images added and exported per day")
    check("usage: daily images added/exported", sum(r["images added"] for r in daily) == added and sum(r["images exported"] for r in daily) == exported, str(daily))
    feats = panel("luxforge-usage", "Feature use (interaction events)")
    check("usage: feature use", feats == [{"feature": "export", "events": 1, "sessions": 1, "installations": 1}], str(feats))
    tools = panel("luxforge-usage", "Tool commits")
    check("usage: tool commits", len(tools) == 1 and tools[0]["tool"] == "basic" and tools[0]["commits"] == 1 and tools[0]["images"] == 1, str(tools))
    p = scenario.percentile
    groups = {}
    for e in EVENTS:
        if e["name"] == "operation.end":
            groups.setdefault((e["operation"], e["outcome"]), []).append(e)
    rows = {(r["operation"], r["outcome"]): r for r in panel("luxforge-usage", "Operation outcomes and duration")}
    ok = set(rows) == set(groups) and all(
        rows[k]["operations"] == len(v) and rows[k]["sessions"] == len({e["session"] for e in v})
        and close(rows[k]["p50 ms"], round(p([e["duration_ms"] for e in v], .5)))
        and close(rows[k]["p95 ms"], round(p([e["duration_ms"] for e in v], .95)))
        and rows[k]["max ms"] == max(e["duration_ms"] for e in v) for k, v in groups.items())
    check("usage: operation outcomes and durations", ok, str(rows))
    rel = {(r["app"], r["os"]): (r["sessions"], r["installations"]) for r in panel("luxforge-usage", "Sessions by release and operating system")}
    want = {}
    for n, s in scenario.SESSIONS.items():
        key = (s["release"].split("+")[0], scenario.cohort_label(n, "os"))
        a, b = want.get(key, (0, set()))
        want[key] = (a + 1, b | {s["installation"]})
    check("usage: sessions by release and OS", rel == {k: (a, len(b)) for k, (a, b) in want.items()}, str(rel))


def path_checks():
    f = scenario.funnel()
    row = panel("luxforge-paths", "Add images, then Develop, then a completed export")[0]
    check("paths: funnel counts equal the independent ordered-subsequence counts",
          row["sessions"] == f["sessions"] == 5 and row["added images"] == f["added"] == 3
          and row["... then entered Develop"] == f["develop"] == 3 and row["... then completed an export"] == f["exported"] == 2
          and close(row["% of sessions that added images"], round(100 * f["exported"] / f["added"], 1))
          and close(row["% of all sessions"], round(100 * f["exported"] / f["sessions"], 1)), str(row))
    want = {}
    for op, prev, gap in scenario.failures():
        k = (op, prev)
        a, g = want.get(k, (0, 0))
        want[k] = (a + 1, g + (1 if gap > 0 else 0))
    got = {(r["operation"], r["preceded_by"]): (r["failures"], r["after a gap"]) for r in
           panel("luxforge-paths", "What immediately precedes a failed operation")}
    check("paths: what precedes a failed operation (including one after a sequence gap)", got == want, f"{got} vs {want}")
    two = panel("luxforge-paths", "The two events before a failed operation")
    check("paths: two-step predecessors list every failure", sum(r["failures"] for r in two) == len(scenario.failures()), str(two))
    nxt = {r["followed_by"]: r["times"] for r in panel("luxforge-paths", "What follows entering Develop")}
    want_next = {}
    for n in scenario.SESSIONS:
        evs = [e for e in EVENTS if e["session_name"] == n]
        for i, e in enumerate(evs):
            if scenario.label(e) == "workspace.enter: develop":
                k = scenario.label(evs[i + 1]) if i + 1 < len(evs) else "(nothing received after)"
                want_next[k] = want_next.get(k, 0) + 1
    check("paths: what follows entering Develop", nxt == want_next, f"{nxt} vs {want_next}")
    stages = {r["stage"]: r["sessions"] for r in panel("luxforge-paths", "Sessions by furthest stage reached")}
    check("paths: furthest stage per session", stages == {"completed an export after Develop": 2, "entered Develop after adding images": 1,
                                                          "none of these": 2}, str(stages))


def session_checks():
    sessions = variable_values("luxforge-session", "session")
    check("session variable lists every session as text/value pairs",
          len(sessions) == 5 and all("__value" in r and "__text" in r for r in sessions), str(sessions[:2]))
    check("session variable labels do not call sessions users or devices",
          not any(re.search(r"\b(user|device|person)\b", r["__text"], re.I) for r in sessions))
    for name in ("S1", "S3", "S4"):
        s = scenario.SESSIONS[name]
        facts = scenario.session_facts(name)
        summary = panel("luxforge-session", "Session summary", {"session": s["session"]})[0]
        check(f"session summary {name}: events, gaps, batches, drops, active time",
              summary["events received"] == facts["events"] and summary["events missing"] == facts["missing_events"]
              and summary["batches expected"] == facts["expected_batches"] and summary["batches received"] == facts["batches"]
              and summary["batches missing"] == facts["missing_batches"] and summary["dropped (reported)"] == facts["dropped"]
              and summary["active seconds"] == round(facts["active_ms"] / 1000), str(summary))
        tl = panel("luxforge-session", "Timeline", {"session": s["session"]})
        evs = [e for e in EVENTS if e["session_name"] == name]
        start_ms = (NOW - s["start_ago"]) * 1000
        prev_seq = -1
        missing = []
        for e in evs:
            missing.append(e["seq"] - prev_seq - 1)
            prev_seq = e["seq"]
        ok = ([r["seq"] for r in tl] == [e["seq"] for e in evs] and [r["label"] for r in tl] == [scenario.label(e) for e in evs]
              and [r["missing before"] for r in tl] == missing and [r["wall time"] for r in tl] == [start_ms + e["t"] for e in evs])
        check(f"session timeline {name}: order, labels, wall-clock times and gaps", ok, f"{[ (r['seq'], r['label'], r['missing before']) for r in tl]}")
    batches = panel("luxforge-session", "Batches", {"session": SESS["S3"]})
    check("session batches S3: only batches 0 and 2 arrived, batch 2 reported 4 dropped",
          [(r["batch"], r["events"], r["dropped (reported)"]) for r in batches] == [(0, 3, 0), (2, 3, 4)], str(batches))
    s4 = panel("luxforge-session", "Batches", {"session": SESS["S4"]})
    check("session batches S4: the duplicated batch appears once", [(r["batch"], r["events"]) for r in s4] == [(0, 6)], str(s4))
    samples = panel("luxforge-session", "Process samples", {"session": SESS["S1"]})
    check("session process samples", [(r["CPU %"], r["memory MiB"]) for r in samples] == [(40, 2000), (60, 2200)], str(samples))


def history_checks():
    installs = variable_values("luxforge-history", "installation")
    check("installation variable lists every installation", len(installs) == 4, str(installs))
    imgs = variable_values("luxforge-history", "image", {"installation": scenario.INSTALL_A})
    check("image variable lists only the selected installation's images",
          sorted(r["__value"] for r in imgs) == sorted([scenario.IMG1, scenario.IMG2]), str(imgs))
    v = {"installation": scenario.INSTALL_A, "image": scenario.IMG1}
    sess = panel("luxforge-history", "Sessions of this installation", v)
    want = [scenario.session_facts(n) for n in ("S1", "S2")]
    check("installation history: its two sessions in order with events and active minutes",
          [(r["events"], r["events missing"], r["session"]) for r in sess] == [(f["events"], f["missing_events"], SESS[n]) for f, n in zip(want, ("S1", "S2"))]
          and all(close(r["active minutes"], round(f["active_ms"] / 60000, 1)) for r, f in zip(sess, want)), str(sess))
    p = scenario.percentile
    per = panel("luxforge-history", "Preview settle time per session (p50 and p95)", v)
    exp = [[e["duration_ms"] for e in EVENTS if e["session_name"] == n and e["name"] == "preview.settled"] for n in ("S1", "S2")]
    check("installation history: preview percentiles per session",
          len(per) == 2 and all(close(r["p50"], p(d, .5)) and close(r["p95"], p(d, .95)) for r, d in zip(per, exp)), f"{per} vs {exp}")
    ops = {(r["operation"], r["outcome"]): r["operations"] for r in panel("luxforge-history", "Operations of this installation", v)}
    check("installation history: operations", ops == {("export", "completed"): 1, ("export", "failed"): 1, ("source-preparation", "completed"): 1}, str(ops))
    hist = panel("luxforge-history", "History of the selected image across sessions", v)
    want_h = [(SESS[n][:8], e["seq"], scenario.label(e), e.get("duration_ms")) for n in ("S1", "S2")
              for e in EVENTS if e["session_name"] == n and e.get("image") == scenario.IMG1]
    got_h = [(r["session"], r["seq"], r["label"], r["duration ms"]) for r in hist]
    check("image history: every event of the image across both sessions, in order", got_h == want_h, f"{got_h} vs {want_h}")
    timing = {(r["name"], r["operation"], r["renderer"], r["outcome"]): r for r in panel("luxforge-history", "Timing of the selected image", v)}
    pv = [120, 80, 100]
    t = timing.get(("preview.settled", "", "gpu", ""))
    check("image history: preview timing across sessions", t is not None and t["events"] == 3 and close(t["p50 ms"], round(p(pv, .5)))
          and close(t["p95 ms"], round(p(pv, .95))) and t["max ms"] == 120, str(timing))
    check("image history: export timings by outcome", timing[("operation.end", "export", "", "completed")]["max ms"] == 3000
          and timing[("operation.end", "export", "", "failed")]["max ms"] == 500)
    other = panel("luxforge-history", "History of the selected image across sessions", {"installation": scenario.INSTALL_B, "image": scenario.IMG1})
    check("image IDs are scoped to an installation: another installation shows no history for the same ID", other == [], str(other))


def dashboards_provisioned():
    status, found = grafana_json("/api/search?type=dash-db")
    check("grafana lists the five provisioned dashboards", status == 200 and sorted(d["uid"] for d in found) ==
          ["luxforge-history", "luxforge-paths", "luxforge-performance", "luxforge-session", "luxforge-usage"], str(found)[:300])
    status, health = grafana_json("/api/datasources/uid/luxforge-clickhouse/health")
    check("grafana's ClickHouse datasource is healthy", status == 200 and health.get("status") == "OK", str(health))
    status, plugins = grafana_json("/api/plugins?type=datasource&embedded=0")
    ch_plugin = [p for p in plugins if p["id"] == "grafana-clickhouse-datasource"] if isinstance(plugins, list) else []
    check("the pinned ClickHouse plugin version is loaded", ch_plugin and ch_plugin[0]["info"]["version"] == "4.22.1", str(ch_plugin)[:200])
    for uid in ("luxforge-usage", "luxforge-performance", "luxforge-paths", "luxforge-session", "luxforge-history"):
        dash = load_dashboard(uid)
        text = json.dumps(dash).lower()
        check(f"{uid}: states opt-in bias, best-effort loss and the missing session tail",
              "opt-in" in text and "best-effort" in text and "end of a session is never seen" in text)
        notes = [p["options"]["content"] for p in dash["panels"] if p["type"] == "text"]
        check(f"{uid}: says sessions and installations are not people or physical devices",
              notes and all("Neither is a person or a physical device" in n for n in notes))
        labels = [dash["title"], dash["description"]] + [p["title"] + " " + re.sub(r"\bNot [^.]*\.", "", p.get("description", ""))
                                                       for p in dash["panels"] if p["type"] != "text"]
        labels += [v["label"] for v in dash["templating"]["list"]]
        bad = [l for l in labels if re.search(r"\b(users?|devices?|people|persons?)\b", l, re.I)]
        check(f"{uid}: no title, description or variable calls a session or installation a user or device", not bad, str(bad))
    # Every panel query of every dashboard must run with representative variable values.
    variables = {"cohort": "os", "operation": "export", "session": SESS["S1"], "installation": scenario.INSTALL_A, "image": scenario.IMG1}
    failures, count = [], 0
    for uid in ("luxforge-usage", "luxforge-performance", "luxforge-paths", "luxforge-session", "luxforge-history"):
        for p in load_dashboard(uid)["panels"]:
            for t in p.get("targets", []):
                count += 1
                status, rows, err = run_query(substitute(t["rawSql"], variables), t["format"])
                if status != 200 or err:
                    failures.append(f"{uid}/{p['title']}: {status} {err}")
    check(f"all {count} dashboard panel queries run through Grafana without error", not failures, "; ".join(failures))


def auth_checks():
    for path in ("/api/search", "/api/datasources", "/api/dashboards/uid/luxforge-usage", "/api/user", "/api/org",
                 "/api/datasources/uid/luxforge-clickhouse/health", "/api/folders", "/api/admin/settings", "/api/plugins"):
        status, _ = http(GRAFANA + path)
        check(f"grafana refuses unauthenticated GET {path}", status == 401, str(status))
    status, _ = http(GRAFANA + "/api/ds/query", body="{}")
    check("grafana refuses unauthenticated queries", status == 401, str(status))
    status, _ = http(GRAFANA + "/api/search", headers=auth_header(password="not-the-password"))
    check("grafana refuses a wrong password", status == 401, str(status))
    status, _ = http(GRAFANA + "/metrics")
    check("grafana serves no /metrics without logging in", status in (302, 401, 404), str(status))
    status, _ = http(GRAFANA + "/api/search", headers=auth_header())
    check("grafana accepts the configured administrator", status == 200, str(status))
    status, _ = http(GRAFANA + "/api/dashboards/uid/luxforge-usage", headers={"X-Forwarded-User": "admin", "X-WEBAUTH-USER": "admin"})
    check("grafana ignores proxy-auth headers", status == 401, str(status))
    status, cfg = grafana_json("/api/admin/settings")
    if status == 200:
        analytics = cfg.get("analytics", {})
        check("grafana telemetry, update checks, feedback links and news are off",
              analytics.get("reporting_enabled") == "false" and analytics.get("check_for_updates") == "false"
              and analytics.get("check_for_plugin_updates") == "false" and analytics.get("feedback_links_enabled") == "false"
              and cfg.get("news", {}).get("news_feed_enabled") == "false", json.dumps(analytics))
        check("grafana has no anonymous access, sign-up or public dashboards",
              cfg["auth.anonymous"]["enabled"] == "false" and cfg["users"]["allow_sign_up"] == "false"
              and cfg.get("public_dashboards", {}).get("enabled") == "false" and cfg["snapshots"]["external_enabled"] == "false",
              json.dumps(cfg["auth.anonymous"]))
        check("grafana router (access) logging is off", cfg["server"]["router_logging"] == "false")
    else:
        check("grafana admin settings readable", False, str(status))
    # ClickHouse: authenticated, least privilege.
    status, _ = http("http://clickhouse:8123/?query=SELECT%201")
    check("clickhouse refuses an unauthenticated query", status in (401, 403, 404, 500) or status is None, str(status))
    status, _ = http("http://clickhouse:8123/?query=SELECT%201", headers={"X-ClickHouse-User": "default"})
    check("clickhouse has no default user", status != 200, str(status))
    r = ch("SELECT count() FROM luxforge.events_unique", user="reader")
    check("reader can select", r.returncode == 0, r.stderr[-200:])
    r = ch("INSERT INTO luxforge.events (received_at) VALUES (now())", user="reader")
    check("reader cannot insert", r.returncode != 0, r.stdout)
    r = ch("SELECT count() FROM luxforge.events", user="writer")
    check("writer cannot select", r.returncode != 0)
    r = ch("DROP TABLE luxforge.events", user="writer")
    check("writer cannot drop", r.returncode != 0)
    r = ch("DROP TABLE luxforge.events", user="reader")
    check("reader cannot drop", r.returncode != 0)
    # The admin user is bound to localhost inside the container: not usable from the collector network.
    status, out = http("http://clickhouse:8123/?query=SELECT%201", headers={"X-ClickHouse-User": "luxforge_admin", "X-ClickHouse-Key": ENV["LUXFORGE_CLICKHOUSE_ADMIN_PASSWORD"]}, via="grafana")
    check("the admin user cannot connect over the network", status != 200, str(status))


def container_diff(container_id):
    """Filesystem changes in a container, ignoring the bind-mount points Docker creates for config files."""
    out = subprocess.run(["docker", "diff", container_id], capture_output=True, text=True).stdout.splitlines()
    return "\n".join(l for l in out if not l.endswith(("/etc", "/etc/vector", "/etc/vector/vector.yaml"))).strip()


def container_checks():
    ids = dc("ps", "-q").stdout.split()
    info = json.loads(subprocess.run(["docker", "inspect", *ids], capture_output=True, text=True).stdout)
    svc = {i["Config"]["Labels"]["com.docker.compose.service"]: i for i in info}
    for name in ("ingress", "vector", "grafana"):
        check(f"{name} root filesystem is read-only", svc[name]["HostConfig"]["ReadonlyRootfs"] is True)
    for name, i in svc.items():
        check(f"{name}: all capabilities dropped, no-new-privileges, memory limit set",
              "ALL" in (i["HostConfig"]["CapDrop"] or []) and "no-new-privileges:true" in (i["HostConfig"]["SecurityOpt"] or [])
              and i["HostConfig"]["Memory"] > 0)
    ports = {n: i["NetworkSettings"]["Ports"] for n, i in svc.items()}
    published = {n: p for n, plist in ports.items() for p, b in plist.items() if b}
    check("only the ingress has a published host port and it is bound to 127.0.0.1",
          list(published) == ["ingress"] and all(b["HostIp"] == "127.0.0.1" for b in ports["ingress"]["8080/tcp"]), str(ports))
    for name in ("ingress", "vector"):
        diff = container_diff(svc[name]["Id"])
        check(f"{name}: nothing has been written to its filesystem (no spool, no archive)", diff == "", diff[:300])
    left = dc("exec", "-T", "vector", "sh", "-c", "find /var/lib/vector /tmp -type f | wc -l").stdout.strip()
    check("vector's scratch directories hold no files (no buffer, no checkpoint, no archive)", left == "0", left)


def retention_checks():
    old, near = scenario.uuid(0xE0), scenario.uuid(0xE1)
    for session, days in ((old, 100), (near, 89)):
        ch_ok(f"INSERT INTO luxforge.events (received_at, installation_id, session_id, session_start, batch_seq, seq, t, name, release) "
              f"VALUES (now() - INTERVAL {days} DAY, '{scenario.uuid(0xF0)}', '{session}', now() - INTERVAL {days} DAY, 0, 0, 0, 'session.start', '1.0.0+aaaaaaaaaaaa')")
        ch_ok(f"INSERT INTO luxforge.profile_batches (received_at, installation_id, session_id, release, batch_seq, dropped, os_family, arch, "
              f"cpu_vendor, cpu_physical_cores, cpu_logical_cores, memory_gib_band, gpu_count, renderer) "
              f"VALUES (now() - INTERVAL {days} DAY, '{scenario.uuid(0xF0)}', '{session}', '1.0.0+aaaaaaaaaaaa', 0, 0, 'macos', 'arm64', 'apple', 8, 8, '16', 0, 'gpu')")
    count = lambda table, s: int(ch_scalar(f"SELECT count() FROM luxforge.{table} WHERE session_id = '{s}'"))
    after = {t: (count(t, old), count(t, near)) for t in ("events", "profiles", "batches")}
    check("rows already 100 days old are never kept in events, profiles or batches; rows 89 days old are",
          all(v == (0, 1) for v in after.values()), str(after))
    seen_near = {v: int(ch_scalar(f"SELECT count() FROM luxforge.{v} WHERE session_id = '{near}'")) for v in ("events_unique", "batches_unique", "session_profiles")}
    check("rows 89 days old are visible in the read views", all(n == 1 for n in seen_near.values()), str(seen_near))
    for view in ("events_unique", "batches_unique", "session_profiles"):
        ddl = ch_scalar(f"SHOW CREATE TABLE luxforge.{view}")
        check(f"view {view} hides rows past 90 days even before a merge removes them",
              re.search(r"received_at > \(now\(\) - toIntervalDay\(90\)\)", ddl) is not None, ddl[-200:])
    # Accelerated merge-time expiry: the same DDL with the TTL shortened to 10 seconds, in a scratch database.
    # (2 seconds was shorter than a loaded machine's three `docker compose exec` calls.)
    ch_ok("DROP DATABASE IF EXISTS luxforge_ttltest")
    ch_ok("CREATE DATABASE luxforge_ttltest")
    for t in ("events", "profiles", "batches"):
        ddl = ch_scalar(f"SHOW CREATE TABLE luxforge.{t}").replace("\\n", "\n").replace("\\'", "'")
        scratch = ddl.replace(f"CREATE TABLE luxforge.{t}", f"CREATE TABLE luxforge_ttltest.{t}")
        scratch = scratch.replace("TTL received_at + toIntervalDay(90)", "TTL received_at + toIntervalSecond(10)")
        check(f"scratch copy of {t} has the shortened TTL", "toIntervalSecond(10)" in scratch and "luxforge_ttltest" in scratch, scratch[-200:])
        ch_ok(scratch)
    ch_ok(f"INSERT INTO luxforge_ttltest.events SELECT * REPLACE (now() AS received_at) FROM luxforge.events WHERE session_id = '{SESS['S1']}'")
    ch_ok(f"INSERT INTO luxforge_ttltest.profiles SELECT * REPLACE (now() AS received_at) FROM luxforge.profiles WHERE session_id = '{SESS['S1']}'")
    ch_ok(f"INSERT INTO luxforge_ttltest.batches SELECT * REPLACE (now() AS received_at) FROM luxforge.batches WHERE session_id = '{SESS['S1']}'")
    n = {t: int(ch_scalar(f"SELECT count() FROM luxforge_ttltest.{t}")) for t in ("events", "profiles", "batches")}
    check("scratch tables hold rows before their TTL passes", all(v > 0 for v in n.values()), str(n))
    time.sleep(12)
    for t in ("events", "profiles", "batches"):
        ch_ok(f"OPTIMIZE TABLE luxforge_ttltest.{t} FINAL")
    n = {t: int(ch_scalar(f"SELECT count() FROM luxforge_ttltest.{t}")) for t in ("events", "profiles", "batches")}
    check("a merge removes rows past their TTL from events, profiles and batches", all(v == 0 for v in n.values()), str(n))
    ch_ok("DROP DATABASE luxforge_ttltest")
    for t in ("events", "profiles", "batches"):
        ch_ok(f"ALTER TABLE luxforge.{t} DELETE WHERE session_id = '{near}' SETTINGS mutations_sync = 2")


def stats_line(service="ingress"):
    lines = [l for l in dc("logs", "--no-color", "--no-log-prefix", service).stdout.splitlines() if "stats accepted=" in l]
    return {k: int(v) for k, v in re.findall(r"(\w+)=(\d+)", lines[-1])} if lines else {}


def stored_sessions_like(prefix_n):
    return int(ch_scalar(f"SELECT uniqExact(session_id) FROM luxforge.events WHERE startsWith(toString(session_id), '{prefix_n}')"))


def flood_batch(n, index):
    s = scenario.SESSIONS["S5"]
    session = scenario.uuid(0x7000000 + n * 0x100000 + index)
    installation = scenario.uuid(0x8000000 + n * 0x100000 + index)
    return {"schema": 1, "disclosure_version": 1, "release": s["release"], "installation_id": installation,
            "session_id": session, "session_start": scenario.iso(NOW - 60), "batch_seq": 0, "dropped": 0,
            "device": scenario.DEVICES["D"],
            "events": [{"seq": i, "t": i, "name": "process.sample", "cpu_percent": 1, "memory_mib": 1, "memory_kind": "resident"}
                       for i in range(300)]}


def flood_sessions(n):
    return int(ch_scalar(f"SELECT uniqExact(session_id) FROM luxforge.events WHERE toString(session_id) LIKE '07{n}%'"))


def running_ok(name):
    c = json.loads(subprocess.run(["docker", "inspect", dc("ps", "-q", name).stdout.strip()], capture_output=True, text=True).stdout)[0]
    mem = subprocess.run(["docker", "stats", "--no-stream", "--format", "{{.MemUsage}}", c["Id"]], capture_output=True, text=True).stdout.strip()
    return c["State"]["Running"] and not c["State"]["OOMKilled"] and c["HostConfig"]["Memory"] > 0, mem


def clickhouse_ready():
    return dc("exec", "-T", "clickhouse", "wget", "-q", "--spider", "http://127.0.0.1:8123/ping", check_rc=False).returncode == 0


def bounded_checks():
    # 1. ClickHouse stopped: the ingress and Vector hold only bounded in-memory queues, refuse once
    #    they are full, and write nothing to disk.
    dc("stop", "clickhouse")
    statuses = [post_batch(flood_batch(1, i), SENTINEL_HEADERS) for i in range(300)]
    kinds = {s: statuses.count(s) for s in set(statuses)}
    check("ClickHouse down: the ingress keeps answering (202 or 429 only), never hanging or crashing",
          set(kinds) <= {202, 429} and 202 in kinds, str(kinds))
    check("ClickHouse down: a flood is refused with 429 once admission or the forwarding queue is full", 429 in kinds, str(kinds))
    time.sleep(20)
    for name in ("ingress", "vector"):
        ok, mem = running_ok(name)
        check(f"ClickHouse down: {name} stays running, was not OOM-killed and has a memory limit ({mem})", ok, mem)
    vlog = dc("logs", "--no-color", "--no-log-prefix", "vector").stdout
    check("ClickHouse down: Vector reports dropping events after its retries (and logs no row content)", "Events dropped" in vlog)
    stats = stats_line()
    check("ClickHouse down: the ingress counted refusals", stats.get("limited_global", 0) + stats.get("limited_queue", 0) > 0, str(stats))
    for name in ("ingress", "vector"):
        diff = container_diff(dc("ps", "-q", name).stdout.strip())
        check(f"ClickHouse down: {name} wrote nothing to disk", diff == "", diff[:200])
    # Restarting the two relays discards whatever they held in memory; with ClickHouse still down
    # nothing can have been stored, and nothing on disk can bring it back.
    dc("restart", "ingress", "vector")
    time.sleep(5)
    dc("start", "clickhouse")
    check("ClickHouse comes back", wait_for(clickhouse_ready, 90))
    time.sleep(20)
    late = flood_sessions(1)
    check("rows held in memory during the outage are gone after the relays restart: no spool replays them", late == 0, f"{late} sessions arrived late")
    check("after the outage a batch is accepted and stored",
          post_batch(scenario.build_batch("S5", NOW, 0, 0, scenario.SESSIONS["S5"]["batches"][0][2]) | {"session_id": scenario.uuid(0x9100), "installation_id": scenario.uuid(0x9101)}, SENTINEL_HEADERS) == 202
          and wait_for(lambda: int(ch_scalar(f"SELECT count() FROM luxforge.events WHERE session_id = '{scenario.uuid(0x9100)}'")) == 6, 30))

    # 2. ClickHouse down only briefly: the in-memory backlog is bounded by the ingress queue (256
    #    batches) plus Vector's buffer, and is delivered when ClickHouse returns.
    dc("stop", "clickhouse")
    accepted = sum(1 for i in range(120) if post_batch(flood_batch(2, i), SENTINEL_HEADERS) == 202)
    dc("start", "clickhouse")
    wait_for(clickhouse_ready, 90)
    wait_for(lambda: flood_sessions(2) >= 1, 60)
    time.sleep(40)
    delivered = flood_sessions(2)
    check("short outage: the backlog replayed from memory never exceeds what was accepted nor the ingress queue bound",
          delivered <= min(accepted, 256 + 8), f"accepted {accepted}, delivered {delivered}")

    # 3. Vector stopped: the ingress forwards nowhere, drops and counts; nothing is spooled.
    before = stats_line()
    dc("stop", "vector")
    stopped = [post_batch(flood_batch(3, i), SENTINEL_HEADERS) for i in range(10)]
    time.sleep(12)
    check("Vector down: the ingress still answers (202 or 429)", set(stopped) <= {202, 429}, str(stopped))
    after = stats_line()
    check("Vector down: the ingress counted failed forwards", after.get("forward_failed", 0) > before.get("forward_failed", 0), f"{before} -> {after}")
    diff = container_diff(dc("ps", "-q", "ingress").stdout.strip())
    check("Vector down: the ingress wrote nothing to disk", diff == "", diff[:200])
    dc("start", "vector")
    time.sleep(8)
    # The ingress tries each body once, so the first body after a restart can meet its stale
    # keep-alive connection and be lost. A primer absorbs that (documented in the README).
    primer = flood_batch(4, 0)
    primer["events"] = primer["events"][:2]
    post_batch(primer, SENTINEL_HEADERS)
    time.sleep(3)
    fresh = flood_batch(5, 0)
    fresh["events"] = fresh["events"][:5]
    check("Vector back: a new batch is accepted", post_batch(fresh, SENTINEL_HEADERS) == 202)
    check("Vector back: the new batch is stored", wait_for(lambda: flood_sessions(5) == 1, 30))
    check("Vector back: the batches sent while it was down were dropped, not stored", flood_sessions(3) == 0, str(flood_sessions(3)))


def probe_checks():
    """The deployment checklist's synthetic live-endpoint probe, run against the real ingress."""
    check("ingress is ready for the probe", wait_for(lambda: post_batch(None, raw=b"{}") == 400, 60))
    r = subprocess.run([sys.executable, "-I", os.path.join(STACK, "probe.py"), f"http://127.0.0.1:{PORT}"],
                       capture_output=True, text=True, timeout=300)
    summary = re.search(r"(\d+) passed, (\d+) failed, (\d+) skipped", r.stdout)
    check("the synthetic probe passes every check it can run against the stack (TLS is skipped on loopback HTTP)",
          r.returncode == 0 and summary is not None and summary.group(2) == "0", r.stdout[-1500:] + r.stderr[-500:])
    printed = re.search(r"\(installation ([0-9a-f-]{36})\)", r.stdout)
    mine = f"installation_id = '{printed.group(1) if printed else 'missing'}'"
    check("the probe's accepted batches are stored: 12 events in 3 sessions of its one installation, with the synthetic release",
          wait_for(lambda: ch_scalar(f"SELECT count() FROM luxforge.events WHERE {mine}") == "12", 30)
          and ch_scalar(f"SELECT uniqExact(session_id) FROM luxforge.events WHERE {mine}") == "3"
          and ch_scalar(f"SELECT uniqExact(release) FROM luxforge.events WHERE {mine}") == "1"
          and ch_scalar(f"SELECT any(release) FROM luxforge.events WHERE {mine}") == "0.0.0+000000000000",
          ch_scalar(f"SELECT count() FROM luxforge.events WHERE {mine}"))
    ids = printed.group(1) if printed else ""
    listed = re.findall(r"ALTER TABLE luxforge\.(\w+) DELETE WHERE installation_id IN \(([^)]*)\)", r.stdout)
    check("the probe prints the statements that remove its rows, naming the installation it used",
          {t for t, _ in listed} == {"events", "profiles", "batches"} and all(ids in g for _, g in listed), f"{listed} vs {ids}")
    for table, group in listed:
        ch_ok(f"ALTER TABLE luxforge.{table} DELETE WHERE installation_id IN ({group}) SETTINGS mutations_sync = 2")
    left = {t: int(ch_scalar(f"SELECT count() FROM luxforge.{t} WHERE {mine}")) for t in ("events", "profiles", "batches")}
    check("running the printed statements removes the probe's rows from all three tables", left == {"events": 0, "profiles": 0, "batches": 0}, str(left))


def privacy_log_checks():
    logs = {}
    for service in ("ingress", "vector", "clickhouse", "grafana"):
        r = dc("logs", "--no-color", "--no-log-prefix", service)
        logs[service] = r.stdout + r.stderr
    check("logs were captured from every container", all(len(v) > 0 for v in logs.values()), str({k: len(v) for k, v in logs.items()}))
    ids = {s["installation"] for s in scenario.SESSIONS.values()} | {s["session"] for s in scenario.SESSIONS.values()}
    ids |= {scenario.IMG1, scenario.IMG2, scenario.IMG3, scenario.IMG4, scenario.IMG5}
    ids |= {scenario.uuid(0x7000000 + a * 0x100000 + b) for a in range(1, 7) for b in range(3)}
    forbidden = {"forwarded-for address": SENTINEL_XFF, "real-ip address": SENTINEL_REAL_IP, "user agent": SENTINEL_UA,
                 "user agent product": "SentinelAgent", "cookie": SENTINEL_COOKIE, "cookie value": "c00k1e-5ent1nel",
                 "header value": SENTINEL_HEADER, "raw payload": SENTINEL_PAYLOAD}
    for k in ("ADMIN", "WRITER", "READER"):
        forbidden[f"clickhouse {k.lower()} password"] = ENV[f"LUXFORGE_CLICKHOUSE_{k}_PASSWORD"]
    forbidden["grafana password"] = ENV["LUXFORGE_GRAFANA_ADMIN_PASSWORD"]
    for i, v in enumerate(sorted(ids)):
        forbidden[f"identifier {i}"] = v
    net_ids = subprocess.run(["docker", "network", "ls", "--filter", f"label=com.docker.compose.project={PROJECT}", "-q"],
                             capture_output=True, text=True).stdout.split()
    nets = json.loads(subprocess.run(["docker", "network", "inspect", *net_ids], capture_output=True, text=True).stdout)
    addrs = set()
    for n in nets:
        for c in n.get("IPAM", {}).get("Config") or []:
            if c.get("Gateway"):
                addrs.add(c["Gateway"])
        for c in n.get("Containers", {}).values():
            addrs.add(c["IPv4Address"].split("/")[0])
    for a in sorted(addrs):
        forbidden[f"network address {a}"] = a
    hits = []
    for service, text in logs.items():
        for what, needle in forbidden.items():
            if re.search(r"(?<![\d.])" + re.escape(needle) + r"(?![\d])", text):
                hits.append(f"{service}: {what}")
    check(f"no container log contains a sentinel address, header, User-Agent, cookie, payload, password, ID, or any of the {len(addrs)} network addresses",
          not hits, "; ".join(hits))
    ipv4 = {}
    for service, text in logs.items():
        for m in re.finditer(r"(?<![\d.])(\d{1,3}(?:\.\d{1,3}){3})(?![\d.])", text):
            ip = m.group(1)
            if ip not in ("0.0.0.0", "127.0.0.1") and all(int(o) <= 255 for o in ip.split(".")):
                ctx = text[max(0, m.start() - 25): m.end() + 12].replace("\n", " ")
                # A four-part software version, as in "version 25.8.33.6 (official build)".
                if text[max(0, m.start() - 8): m.start()] == "version " or text[m.end(): m.end() + 10] == " (official":
                    continue
                ipv4.setdefault(service, set()).add(ctx)
    check("no log line contains any other IPv4 address", not ipv4, str({k: sorted(v)[:3] for k, v in ipv4.items()}))
    grafana_req = [l for l in logs["grafana"].splitlines() if "remote_addr" in l or "Request Completed" in l]
    check("grafana logs no request records with peer addresses", not grafana_req, "\n".join(grafana_req[:3]))
    ingress_lines = [l for l in logs["ingress"].splitlines() if l.strip()]
    fixed = all(l.startswith("usage-ingest ") for l in ingress_lines)
    check("ingress logs only its fixed messages and aggregate counters", fixed and ingress_lines, "\n".join(ingress_lines[:3]))
    # What is stored holds none of the request metadata either.
    dump = ""
    for table in ("events", "profiles", "batches"):
        dump += ch_ok(f"SELECT * FROM luxforge.{table} FORMAT TSV")
    leaks = [w for w, n in forbidden.items() if w.split()[0] in ("forwarded-for", "real-ip", "user", "cookie", "header", "raw") and n in dump]
    check("no stored row contains a sentinel address, header, User-Agent, cookie or payload", not leaks, str(leaks))


def main():
    print(f"== project {PROJECT}, ingress on 127.0.0.1:{PORT}")
    static_checks()
    dc("up", "-d", "--build", "--wait", "--wait-timeout", "300")
    check("ingress accepts connections", wait_for(lambda: post_batch(None, raw=b"{}") == 400, 60))
    wait_for(lambda: grafana_json("/api/health")[0] == 200, 60)
    only = set(filter(None, os.environ.get("STACK_TEST_ONLY", "").split(",")))
    sections = [send_scenario, stored_rows, allowlist_checks, dashboards_provisioned, usage_checks, cohort_checks,
                path_checks, session_checks, history_checks, auth_checks, container_checks, retention_checks,
                bounded_checks, probe_checks, privacy_log_checks]
    for section in sections:
        if not only or section.__name__ in only:
            section()
    failed = [r for r in results if not r[1]]
    print(f"\n== {len(results) - len(failed)} passed, {len(failed)} failed, {len(results)} checks")
    for name, _, detail in failed:
        print(f"FAILED: {name}\n    {detail}")
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as e:
        import traceback
        traceback.print_exc()
        print(f"\n== aborted: {e}")
        sys.exit(2)
