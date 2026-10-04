#!/usr/bin/env python3
"""Explicit, bounded R2 sample sync and authentic adapter regression tooling.

Python 3.10+ and AWS CLI v2; no cloud access during ordinary repository checks.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "fixtures/sample-corpus.json"
MAX_SOURCE_BYTES = 512 * 1024 * 1024
MARKER = ".luxforge-sample-cache-v1"
HASH = re.compile(r"[0-9a-f]{64}")


class CorpusError(Exception):
    pass


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def atomic_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as out:
        tmp = Path(out.name)
        json.dump(value, out, indent=2)
        out.write("\n")
    os.replace(tmp, path)


def load_manifest(path: Path) -> dict:
    data = json.loads(path.read_text())
    if data.get("format") != "luxforge-sample-corpus-v1":
        raise CorpusError("unsupported sample manifest format")
    ids, hashes = set(), set()
    for sample in data["samples"]:
        sid, sha = sample["id"], sample["sha256"]
        if not isinstance(sid, str) or not re.fullmatch(r"[a-zA-Z0-9_-]+", sid) or sid in ids:
            raise CorpusError("invalid or duplicate sample id")
        if not HASH.fullmatch(sha) or sha in hashes:
            raise CorpusError(f"{sid}: invalid or duplicate source hash")
        if sample["license"] != "CC0-1.0":
            raise CorpusError(f"{sid}: mirror only accepts explicitly CC0 samples")
        if not 0 < sample["bytes"] <= MAX_SOURCE_BYTES:
            raise CorpusError(f"{sid}: source size outside bounds")
        if sample["key"] != f"sha256/{sha[:2]}/{sha}":
            raise CorpusError(f"{sid}: object key must address the source hash")
        if not re.fullmatch(r"\.[a-zA-Z0-9]{1,8}", sample["extension"]):
            raise CorpusError(f"{sid}: unsafe extension")
        url = urllib.parse.urlsplit(sample["source_url"])
        if url.scheme != "https" or url.netloc != "raw.pixls.us":
            raise CorpusError(f"{sid}: unexpected upstream source")
        if sample["expectation"]["kind"] not in ("qualified", "mosaic", "candidate", "refusal"):
            raise CorpusError(f"{sid}: unknown expectation")
        ids.add(sid)
        hashes.add(sha)
    return data


def selected(data: dict, args: argparse.Namespace) -> list[dict]:
    groups = set(args.group or [])
    unknown = groups - {s["group"] for s in data["samples"]}
    if unknown:
        raise CorpusError(f"unknown groups: {sorted(unknown)}")
    samples = [s for s in data["samples"] if not groups or s["group"] in groups]
    if args.qualified_only:
        samples = [s for s in samples if s["expectation"]["kind"] in ("qualified", "mosaic")]
    samples.sort(key=lambda s: s["id"])
    if args.shard:
        match = re.fullmatch(r"(\d+)/(\d+)", args.shard)
        if not match or not 0 <= int(match[1]) < int(match[2]) <= 64:
            raise CorpusError("shard must be zero-based INDEX/COUNT, COUNT <= 64")
        index, count = map(int, match.groups())
        samples = [s for s in samples if int(s["sha256"][:16], 16) % count == index]
    if args.limit is not None:
        if args.limit < 1:
            raise CorpusError("limit must be positive")
        samples = samples[:args.limit]
    if not samples:
        raise CorpusError("selection contains no samples")
    return samples


def credentials(env_file: Path, write: bool = False) -> dict[str, str]:
    values = {}
    if env_file.exists():
        for line in env_file.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#"):
                key, separator, value = line.partition("=")
                if not separator or not re.fullmatch(r"[A-Z0-9_]+", key):
                    raise CorpusError("invalid dotenv entry (shell expansion is not supported)")
                values[key] = value.strip().strip("\"'")
    # Explicit process environment wins over a local dotenv file.
    values.update({k: v for k, v in os.environ.items() if k.startswith("R2_")})
    prefix = "R2_" if write or not values.get("R2_READ_ACCESS_KEY_ID") else "R2_READ_"
    key, secret = values.get(prefix + "ACCESS_KEY_ID"), values.get(prefix + "SECRET_ACCESS_KEY")
    endpoint = values.get("R2_ENDPOINT_URL") or f"https://{values.get('R2_ACCOUNT_ID', '')}.r2.cloudflarestorage.com"
    if not key or not secret or not values.get("R2_BUCKET"):
        raise CorpusError("missing R2 credentials; configure ignored .env or R2_* environment variables")
    if not re.fullmatch(r"https://[a-f0-9]{32}\.r2\.cloudflarestorage\.com", endpoint):
        raise CorpusError("R2 endpoint must be the account's HTTPS Cloudflare S3 endpoint")
    if not re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", values["R2_BUCKET"]):
        raise CorpusError("invalid R2 bucket name")
    return {"key": key, "secret": secret, "endpoint": endpoint, "bucket": values["R2_BUCKET"]}


class R2:
    def __init__(self, config: dict):
        self.config = config
        if not shutil.which("aws"):
            raise CorpusError("AWS CLI v2 is required for authenticated R2 operations")

    def call(self, *args: str, missing_ok: bool = False) -> dict | None:
        env = os.environ.copy()
        env.update(AWS_ACCESS_KEY_ID=self.config["key"], AWS_SECRET_ACCESS_KEY=self.config["secret"],
                   AWS_DEFAULT_REGION="auto", AWS_EC2_METADATA_DISABLED="true", AWS_PAGER="",
                   AWS_RETRY_MODE="standard", AWS_MAX_ATTEMPTS="3",
                   AWS_REQUEST_CHECKSUM_CALCULATION="when_required", AWS_RESPONSE_CHECKSUM_VALIDATION="when_required")
        # Ignore unrelated local AWS session credentials and profiles.
        for key in ("AWS_SESSION_TOKEN", "AWS_PROFILE", "AWS_SECURITY_TOKEN"):
            env.pop(key, None)
        process = subprocess.run(["aws", "--endpoint-url", self.config["endpoint"], "--region", "auto",
                                  "--cli-connect-timeout", "30", "--cli-read-timeout", "120",
                                  "s3api", *args, "--output", "json"], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=600)
        if process.returncode:
            if missing_ok and ("(404)" in process.stderr or "(NoSuchKey)" in process.stderr or "(NotFound)" in process.stderr):
                return None
            # Never echo SDK diagnostics, environment or credentials.
            raise CorpusError(f"R2 {args[0]} failed (exit {process.returncode}); check token scope/expiry and connectivity")
        return json.loads(process.stdout or "{}")

    def head(self, sample: dict) -> dict | None:
        return self.call("head-object", "--bucket", self.config["bucket"], "--key", sample["key"], missing_ok=True)

    def check_head(self, sample: dict, head: dict | None) -> None:
        if not head or head.get("ContentLength") != sample["bytes"] or head.get("Metadata", {}).get("sha256") != sample["sha256"]:
            raise CorpusError(f"{sample['id']}: R2 object size/hash metadata mismatch or missing object")

    def download(self, sample: dict, target: Path) -> None:
        head = self.head(sample)
        self.check_head(sample, head)
        # Bind the bounded size check to the same object version during the streamed read.
        self.call("get-object", "--bucket", self.config["bucket"], "--key", sample["key"],
                  "--if-match", head["ETag"], str(target))

    def upload(self, sample: dict, source: Path) -> bool:
        verify(source, sample)
        head = self.head(sample)
        if head is not None:
            self.check_head(sample, head)
            return False
        self.call("put-object", "--bucket", self.config["bucket"], "--key", sample["key"],
                  "--body", str(source), "--metadata", f"sha256={sample['sha256']},license={sample['license']}",
                  "--content-type", "application/json" if sample["key"].startswith("manifests/") else "application/octet-stream", "--if-none-match", "*")
        self.check_head(sample, self.head(sample))
        return True


def cache_root(path: Path) -> Path:
    path = path.absolute()
    if path.is_symlink() or any(p.is_symlink() for p in path.parents):
        raise CorpusError("cache must not contain symlink components")
    if (path / MARKER).is_symlink():
        raise CorpusError("cache marker must not be a symlink")
    if path.exists() and not (path / MARKER).is_file():
        if any(path.iterdir()):
            raise CorpusError("refusing nonempty directory without the tool's cache marker")
    path.mkdir(parents=True, exist_ok=True)
    (path / MARKER).touch(exist_ok=True)
    return path


def cached_path(root: Path, sample: dict) -> Path:
    return root / (sample["sha256"] + sample["extension"].lower())


def verify(path: Path, sample: dict) -> None:
    if path.is_symlink() or not path.is_file() or path.stat().st_size != sample["bytes"] or digest(path) != sample["sha256"]:
        raise CorpusError(f"{sample['id']}: missing or changed source {path.name}")


def upstream(sample: dict, target: Path) -> None:
    url = urllib.parse.quote(sample["source_url"], safe=":/?=&%")
    request = urllib.request.Request(url, headers={"User-Agent": "Luxforge-sample-corpus/1"})
    with urllib.request.urlopen(request, timeout=90) as response, target.open("wb") as out:
        if urllib.parse.urlsplit(response.url).scheme != "https":
            raise CorpusError("upstream redirected outside HTTPS")
        count = 0
        while block := response.read(1024 * 1024):
            count += len(block)
            if count > sample["bytes"]:
                raise CorpusError(f"{sample['id']}: upstream exceeds pinned size")
            out.write(block)


def obtain(root: Path, sample: dict, transfer, created: list[Path]) -> Path:
    path = cached_path(root, sample)
    if path.exists() or path.is_symlink():
        verify(path, sample)
        return path
    with tempfile.NamedTemporaryFile(dir=root, suffix=".partial", delete=False) as out:
        partial = Path(out.name)
    try:
        transfer(sample, partial)
        verify(partial, sample)
        # Never replace a pre-existing original/cache entry, including a concurrent writer.
        os.link(partial, path)
        created.append(path)
    finally:
        partial.unlink(missing_ok=True)
    return path


def cleanup(created: list[Path], root: Path) -> None:
    for path in created:
        if path.parent != root or path.is_symlink():
            raise CorpusError("unsafe cleanup target")
        if path.exists() and digest(path) != path.stem:
            print(f"retained changed downloaded source {path.name}", file=sys.stderr)
            continue
        path.unlink(missing_ok=True)
    created.clear()


def chunks(samples: list[dict], count: int, byte_limit: int):
    if not 1 <= count <= 128 or not MAX_SOURCE_BYTES <= byte_limit <= 16 * 1024**3:
        raise CorpusError("chunk size must be 1..128; chunk byte budget must be 512 MiB..16 GiB")
    chunk, size = [], 0
    for sample in samples:
        if chunk and (len(chunk) >= count or size + sample["bytes"] > byte_limit):
            yield chunk
            chunk, size = [], 0
        chunk.append(sample)
        size += sample["bytes"]
    if chunk:
        yield chunk


def compare(sample: dict, result: dict, strict_development: bool) -> tuple[str, list[str]]:
    expected = sample["expectation"]
    problems = []
    if result.get("source_sha256") != sample["sha256"] or result.get("source_sha256_after") != sample["sha256"]:
        problems.append("source preservation mismatch")
    if expected["kind"] == "candidate":
        return "unqualified-decoded" if result["status"] == "passed" else "unqualified-refused", problems
    if expected["kind"] == "refusal":
        if result["status"] != "failed" or expected["error_contains"] not in result.get("error", ""):
            problems.append("expected explicit refusal changed")
        return "refusal", problems
    if result["status"] != "passed":
        problems.append(result.get("error", "decode/development failed"))
        return "qualified", problems
    if result.get("mosaic_sha256") != expected["mosaic_sha256"]:
        problems.append("frozen mosaic hash changed")
    if result.get("metadata", {}).get("mode") != expected["mode"]:
        problems.append("recording mode changed")
    for key, reference in expected.get("metadata", {}).items():
        actual = result.get("metadata", {}).get(key)
        if not metadata_equal(actual, reference):
            problems.append(f"frozen metadata changed: {key}")
    for name in ("as_shot", "perturbed_wb"):
        actual = result.get(name, {})
        if not actual.get("finite"):
            problems.append(f"{name}: non-finite output")
        if expected["kind"] == "qualified":
            reference = expected[name]
            if (actual.get("width"), actual.get("height")) != (reference["width"], reference["height"]):
                problems.append(f"{name}: output dimensions changed")
            for key in ("min", "max"):
                if key in reference and (type(actual.get(key)) not in (float, int) or
                                         not math.isclose(actual[key], reference[key], rel_tol=1e-5, abs_tol=1e-5)):
                    problems.append(f"{name}: frozen output range changed")
            if strict_development and actual.get("sha256") != reference["sha256"]:
                problems.append(f"{name}: frozen development hash changed")
    return "qualified", problems


def metadata_equal(actual, reference) -> bool:
    if isinstance(reference, float):
        return type(actual) in (float, int) and math.isclose(actual, reference, rel_tol=1e-6, abs_tol=1e-6)
    if isinstance(reference, list):
        return isinstance(actual, list) and len(actual) == len(reference) and all(metadata_equal(a, b) for a, b in zip(actual, reference))
    if isinstance(reference, dict):
        return isinstance(actual, dict) and actual.keys() == reference.keys() and all(metadata_equal(actual[k], reference[k]) for k in reference)
    return actual == reference


def run_tests(args, data, samples, root, remote) -> int:
    if args.require_complete:
        missing = [d["name"] for d in data["devices"] if not d["sample_ids"]]
        candidates = [s["id"] for s in samples if s["expectation"]["kind"] == "candidate"]
        if missing or candidates:
            raise CorpusError(f"coverage incomplete: {len(missing)} devices without samples, {len(candidates)} unqualified selected samples")
    output = args.output.absolute()
    if output.exists():
        raise CorpusError("test output directory must be new")
    output.mkdir(parents=True)
    binary = args.qualifier
    if not binary:
        subprocess.run(["cargo", "build", "--release", "--locked", "-p", "luxforge-raw", "--example", "qualify_profiles"], cwd=ROOT, check=True)
        binary = ROOT / "target/release/examples/qualify_profiles"
    binary = binary.absolute()
    started = time.monotonic()
    report = {"format": "luxforge-corpus-run-v1", "manifest_sha256": digest(args.manifest),
              "scope": "adapter/source preservation; not controlled colour or native editor qualification",
              "development_hashes": args.strict_development, "results": [], "failed": 0}
    created = []
    try:
        for number, group in enumerate(chunks(samples, args.chunk_size, args.chunk_mib * 1024**2)):
            try:
                def prepare(sample):
                    path = cached_path(root, sample) if args.offline else obtain(root, sample, remote.download, created)
                    verify(path, sample)
                    return {"id": sample["id"], "path": str(path), "sha256": sample["sha256"]}
                with ThreadPoolExecutor(max_workers=args.transfer_jobs) as executor:
                    inputs = list(executor.map(prepare, group))
                manifest_path = output / f"chunk-{number:03}.json"
                result_path = output / f"chunk-{number:03}-results.json"
                atomic_json(manifest_path, {"samples": inputs})
                with (output / f"chunk-{number:03}.log").open("w") as log:
                    process = subprocess.run([str(binary), str(manifest_path), str(result_path)], cwd=ROOT,
                                             stdout=log, stderr=subprocess.STDOUT, timeout=args.chunk_timeout)
                if process.returncode not in (0, 2) or not result_path.exists():
                    raise CorpusError(f"qualifier failed without a usable report in chunk {number}")
                outcomes = json.loads(result_path.read_text())["results"]
                if [r["id"] for r in outcomes] != [s["id"] for s in group]:
                    raise CorpusError("qualifier did not report the exact selected inputs")
                for sample, result in zip(group, outcomes):
                    status, problems = compare(sample, result, args.strict_development)
                    verify(cached_path(root, sample), sample)
                    report["results"].append({"id": sample["id"], "device": sample["device"],
                                              "classification": status, "problems": problems})
                    report["failed"] += bool(problems)
                print(f"chunk {number + 1}: {len(group)} sources, {report['failed']} failures so far", flush=True)
                atomic_json(output / "summary.json", report)
            finally:
                if args.cleanup:
                    cleanup(created, root)
    except Exception as error:
        report["error"] = str(error)
        report["failed"] += 1
    finally:
        if args.cleanup:
            cleanup(created, root)
        report["elapsed_seconds"] = round(time.monotonic() - started, 3)
        report["selected"] = len(samples)
        report["processed"] = len(report["results"])
        report["unqualified"] = sum(r["classification"].startswith("unqualified") for r in report["results"])
        atomic_json(output / "summary.json", report)
    print(json.dumps({k: v for k, v in report.items() if k != "results"}, indent=2))
    return 1 if report["failed"] else 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("inventory", "mirror", "publish", "publish-index", "sync", "test", "verify-remote", "clean"))
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    parser.add_argument("--env-file", type=Path, default=ROOT / ".env")
    parser.add_argument("--cache", type=Path, default=ROOT / "private/sample-corpus/cache")
    parser.add_argument("--group", action="append", help="manufacturer group; repeat to combine")
    parser.add_argument("--qualified-only", action="store_true")
    parser.add_argument("--shard", help="zero-based INDEX/COUNT, deterministic by source hash")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--chunk-size", type=int, default=8)
    parser.add_argument("--chunk-mib", type=int, default=1024)
    parser.add_argument("--chunk-timeout", type=int, default=600)
    parser.add_argument("--transfer-jobs", type=int, default=1, help="1..4 bounded mirror/publish/sync workers")
    parser.add_argument("--cleanup", action="store_true", help="remove only files downloaded by this invocation")
    parser.add_argument("--offline", action="store_true", help="test only the local verified cache")
    parser.add_argument("--strict-development", action="store_true", help="also compare frozen development hashes from the M4 reference")
    parser.add_argument("--require-complete", action="store_true", help="fail on missing device samples or unqualified selected inputs")
    parser.add_argument("--qualifier", type=Path, help="already-built qualify_profiles executable")
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts/sample-corpus/run")
    parser.add_argument("--source-dir", type=Path, action="append", help="reuse hash-verified originals here when publishing")
    args = parser.parse_args(argv)
    try:
        if args.offline and args.command != "test":
            raise CorpusError("--offline is only supported by test")
        if not 1 <= args.transfer_jobs <= 4:
            raise CorpusError("transfer jobs must be 1..4")
        if not 1 <= args.chunk_timeout <= 3600:
            raise CorpusError("chunk timeout must be 1..3600 seconds")
        args.manifest = args.manifest.absolute()
        data = load_manifest(args.manifest)
        samples = selected(data, args)
        if args.command == "inventory":
            groups = {}
            for sample in samples:
                group = groups.setdefault(sample["group"], {"samples": 0, "bytes": 0, "qualified": 0})
                group["samples"] += 1
                group["bytes"] += sample["bytes"]
                group["qualified"] += sample["expectation"]["kind"] in ("qualified", "mosaic")
            print(json.dumps({"selected_samples": len(samples), "selected_bytes": sum(s["bytes"] for s in samples),
                              "devices": len(data["devices"]), "missing_devices": [d["name"] for d in data["devices"] if not d["sample_ids"]],
                              "groups": groups}, indent=2))
            return 0
        remote = None if args.command in ("mirror", "clean") or args.offline else R2(credentials(args.env_file, write=args.command in ("publish", "publish-index")))
        if args.command == "publish-index":
            if len(samples) != len(data["samples"]):
                raise CorpusError("publish-index requires the full manifest selection")
            with ThreadPoolExecutor(max_workers=args.transfer_jobs) as executor:
                list(executor.map(lambda s: remote.check_head(s, remote.head(s)), samples))
            sha = digest(args.manifest)
            snapshot = {"id": "manifest", "sha256": sha, "bytes": args.manifest.stat().st_size,
                        "key": f"manifests/{sha}.json", "license": "GPL-3.0-or-later"}
            remote.upload(snapshot, args.manifest)
            print(f"archived immutable manifest at {snapshot['key']}")
            return 0
        if args.command == "verify-remote":
            for sample in samples:
                remote.check_head(sample, remote.head(sample))
            print(f"verified R2 metadata for {len(samples)} pinned objects")
            return 0
        root = cache_root(args.cache)
        if args.command == "clean":
            paths = [cached_path(root, s) for s in samples if cached_path(root, s).exists()]
            # Validate every selected file before deleting any tool-owned cache entry.
            for sample in samples:
                path = cached_path(root, sample)
                if path.exists() or path.is_symlink():
                    verify(path, sample)
            count = len(paths)
            cleanup(paths, root)
            print(f"removed {count} verified tool-cache files; retained originals and reports")
            return 0
        if args.command == "test":
            return run_tests(args, data, samples, root, remote)
        originals = {}
        if args.source_dir:
            sizes = {s["bytes"] for s in samples}
            for directory in args.source_dir:
                for path in directory.rglob("*"):
                    if path.is_file() and not path.is_symlink() and path.stat().st_size in sizes:
                        originals.setdefault(digest(path), path)
        def transfer_one(item):
            index, sample = item
            created = []
            try:
                source = originals.get(sample["sha256"])
                if source is not None:
                    verify(source, sample)
                    if args.command != "publish":
                        original = source
                        source = obtain(root, sample, lambda _s, p: shutil.copyfile(original, p), created)
                else:
                    transfer = upstream if args.command in ("mirror", "publish") else remote.download
                    source = obtain(root, sample, transfer, created)
                if args.command == "publish":
                    # Uploads never fall back to an unlicensed or unpinned original.
                    remote.upload(sample, source)
                print(f"{index}/{len(samples)} {sample['id']} {args.command}: verified", flush=True)
            finally:
                if args.cleanup:
                    cleanup(created, root)
        with ThreadPoolExecutor(max_workers=args.transfer_jobs) as executor:
            # At most four streams/SDK processes; no unbounded network fan-out.
            for _ in executor.map(transfer_one, enumerate(samples, 1)):
                pass
        return 0
    except (CorpusError, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"sample-corpus: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
