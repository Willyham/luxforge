#!/usr/bin/env python3
"""The generative fill's native evidence and blinded comparison kit (README, "The editor's
generative fill" and "Blinded comparison").

It drives the editor's own `luxforge-json` (never the GUI) and the probe, and never downloads:

  prepare  write each case's upright input JPEG, its brush stroke and the stroke's footprint mask
  stage    stage klein's pinned files from a local models directory into a data root
           (`--root`) or a resources directory (`--resources`, such as the desktop's own), so the
           ordinary install job hashes them and fetches nothing (the conditioning is a release
           asset that may not be published yet); each file is checked against its pinned length
           and SHA-256 first
  editor   per case, one `luxforge-json` process under `/usr/bin/time -l`: install if needed,
           Generate, Regenerate, accept and export each candidate; record times, progress, peak
           memory and `uptime` (`--once`: Generate alone; `--prompt`: a Replace, then a
           prompt-free removal in the same process)
  lama     the Fast fill (LaMa, the probe's `fill`) over the same footprint
  sheets   blinded contact sheets: the input with the region outlined, then each method's result
           under a letter in a per-case random order; the key is written separately

Needs Python 3 with Pillow and NumPy (`stage` needs Python 3 alone). Every path is an argument;
nothing is written outside --out, --root and --resources.
"""
import argparse
import hashlib
import json
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import numpy as np
    from PIL import Image, ImageDraw, ImageFont, ImageOps
except ImportError:
    # `stage` needs neither: it runs on Python 3 alone.
    np = Image = ImageDraw = ImageFont = ImageOps = None

AI = "luxforge.ai"
MODEL = "generative-klein-4b"
TASK = "task.fill-generative"

# The local file each declared download is, by the declared URL's file name: the publishers'
# names as `private/models/generative/` holds them (as `crates/luxforge-diffusion`'s loopback test
# maps them).
LOCAL_NAMES = {
    "flux-2-klein-4b-Q4_0.gguf": "flux-2-klein-4b-Q4_0.gguf",
    "Qwen3-4B-Q4_K_M.gguf": "Qwen3-4B-Q4_K_M.gguf",
    "diffusion_pytorch_model.safetensors": "flux2-klein-4b-vae.safetensors",
    "tokenizer.json": "qwen3-tokenizer.json",
}


def local_name(url):
    name = url.rsplit("/", 1)[-1]
    if name in LOCAL_NAMES:
        return LOCAL_NAMES[name]
    if name.endswith("_comfy_converted.safetensors"):
        return "klein-object-remove-lora-comfy.safetensors"
    if name.endswith(".lfsdcond"):
        return name
    raise SystemExit(f"no local file for {url}")


# ---------------------------------------------------------------------------------------------
# prepare


def footprint(width, height, points, radius):
    """The stroke's footprint: every pixel within `radius` content heights of its polyline."""
    r = radius * height
    ys, xs = np.mgrid[0:height, 0:width].astype(np.float32)
    xs += 0.5
    ys += 0.5
    pts = [(x * width, y * height) for x, y in points]
    if len(pts) == 1:
        pts = pts * 2
    best = np.full((height, width), np.inf, dtype=np.float32)
    for (ax, ay), (bx, by) in zip(pts, pts[1:]):
        dx, dy = bx - ax, by - ay
        length = dx * dx + dy * dy
        if length == 0:
            t = np.zeros_like(xs)
        else:
            t = np.clip(((xs - ax) * dx + (ys - ay) * dy) / length, 0, 1)
        d = np.hypot(xs - (ax + t * dx), ys - (ay + t * dy))
        best = np.minimum(best, d)
    return best <= r


def stroke_from_region(region):
    """A horizontal stroke whose footprint covers the region's bounding box."""
    ys, xs = np.nonzero(region)
    x0, x1, y0, y1 = xs.min(), xs.max() + 1, ys.min(), ys.max() + 1
    height, width = region.shape
    half = (y1 - y0) / 2
    r = half + 12
    cy = (y0 + y1) / 2
    # Each end inset as far as the radius still reaches the box's corners.
    inset = min(0.9 * (r * r - half * half) ** 0.5, (x1 - x0) / 2)
    points = [[(x0 + inset) / width, cy / height], [(x1 - inset) / width, cy / height]]
    return points, r / height


def prepare(args):
    cases = json.loads(Path(args.cases).read_text())["cases"]
    out = Path(args.out) / "inputs"
    out.mkdir(parents=True, exist_ok=True)
    for case in cases:
        cid = case["id"]
        if "paste" in case:
            pasted = Path(args.out) / "paste" / case["paste"]
            subprocess.run(
                [args.probe, "paste", "--case", case["paste"], "--out", str(pasted)]
                + (["--corpus", args.corpus] if args.corpus else [])
                + (["--public", args.public] if args.public else []),
                check=True,
            )
            image = Image.open(pasted / "pasted.png").convert("RGB")
            region = np.array(Image.open(pasted / "region.png").convert("L")) > 0
            points, radius = stroke_from_region(region)
        else:
            source = Path(case["image"].replace("private:", f"{args.corpus}/"))
            image = ImageOps.exif_transpose(Image.open(source)).convert("RGB")
            points, radius = case["points"], case["radius"]
        image.save(out / f"{cid}.jpg", quality=95, subsampling=0)
        mask = footprint(image.width, image.height, points, radius)
        Image.fromarray((mask * 255).astype(np.uint8)).save(out / f"{cid}-mask.png")
        (out / f"{cid}-stroke.json").write_text(
            json.dumps(
                {"id": cid, "points": points, "radius": radius, "note": case.get("note", "")},
                indent=2,
            )
        )
        print(f"{cid}: {image.width}x{image.height}, {int(mask.sum())} px in the footprint")


# ---------------------------------------------------------------------------------------------
# luxforge-json


class Json:
    """One `luxforge-json` process, optionally under `/usr/bin/time -l`."""

    def __init__(self, binary, root, timed=False):
        root = Path(root)
        command = [
            binary,
            "--catalog",
            str(root / "catalog.sqlite"),
            "--data-root",
            str(root),
            "--secret-store",
            "memory",
            "--permission-authority",
        ]
        if timed:
            command = ["/usr/bin/time", "-l"] + command
        self.stderr_path = root / "last-stderr.txt"
        self.stderr = open(self.stderr_path, "w")
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            text=True,
            bufsize=1,
        )
        self.next = 0

    def raw(self, method, params):
        self.next += 1
        request = {"id": f"r{self.next}", "method": method, "params": params}
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        while True:
            line = self.process.stdout.readline()
            if not line:
                raise SystemExit(f"luxforge-json ended; see {self.stderr_path}")
            answer = json.loads(line)
            if answer.get("id") == request["id"]:
                return answer

    def call(self, method, params):
        answer = self.raw(method, params)
        if "error" in answer and answer["error"]:
            raise SystemExit(f"{method}: {json.dumps(answer['error'])}")
        return answer.get("result")

    def settle(self, job, on_progress=None, poll=0.25):
        while True:
            record = self.call("job.read", {"job_id": job})
            if on_progress:
                on_progress(record)
            if record["status"] not in ("queued", "running"):
                return record
            time.sleep(poll)

    def finish(self):
        self.process.stdin.close()
        self.process.wait()
        self.stderr.close()
        return self.stderr_path.read_text()


def mutation(tag):
    return {"request_id": f"{tag}-{time.time_ns()}", "actor": "generative-kit"}


def resources(client):
    listed = client.call("module.list", {})
    module = next(m for m in listed["modules"] if m["id"] == AI)
    return module["resources"]


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        for block in iter(lambda: file.read(1 << 22), b""):
            digest.update(block)
    return digest.hexdigest()


def stage(args):
    """Stage every luxforge.ai resource's pinned files from `--models` where an install looks for
    a transfer it can resume: `<resources>/.staging/luxforge.ai/<id>/<version>/` with its
    `.partial.json`. The install then hashes them and fetches nothing it already holds.

    `--root` is a `luxforge-json --data-root`, whose resources directory is
    `<root>/data/modules/resources`. `--resources` names a resources directory itself, such as the
    desktop's own (`~/Library/Application Support/Luxforge/modules/resources` on macOS); the
    descriptors are then listed from a temporary root, so nothing but the staged files is written
    there."""
    listing = Path(args.root) if args.root else Path(tempfile.mkdtemp(prefix="generative-kit-"))
    listing.mkdir(parents=True, exist_ok=True)
    client = Json(args.json, listing)
    declared = resources(client)
    client.finish()
    if args.resources:
        shutil.rmtree(listing)
        resources_dir = Path(args.resources).expanduser()
    else:
        resources_dir = listing / "data/modules/resources"
    models = Path(args.models).expanduser()
    for resource in declared:
        installed = resources_dir / AI / resource["id"] / resource["version"] / "installed.json"
        if installed.exists():
            print(f"{resource['id']} {resource['version']} is already installed")
            continue
        version_dir = resources_dir / ".staging" / AI / resource["id"] / resource["version"]
        version_dir.mkdir(parents=True, exist_ok=True)
        pins = [
            {
                "name": resource["id"],
                "url": resource["url"],
                "sha256": resource["sha256"],
                "bytes": resource["bytes"],
            }
        ] + [
            {"name": f["name"], "url": f["url"], "sha256": f["sha256"], "bytes": f["bytes"]}
            for f in resource.get("files", [])
        ]
        for pin in pins:
            source = models / local_name(pin["url"])
            target = version_dir / pin["name"]
            if source.stat().st_size != pin["bytes"]:
                raise SystemExit(f"{source} is not {pin['bytes']} bytes")
            # A file that is not its pin would be fetched again from the start, or, for the
            # conditioning while its release asset is unpublished, fail the install.
            if sha256_of(source) != pin["sha256"]:
                raise SystemExit(f"{source} is not the pinned file (SHA-256 {pin['sha256']})")
            if target.exists():
                target.unlink()
            # A copy-on-write clone where the file system offers one (APFS), else a copy.
            if sys.platform == "darwin":
                subprocess.run(["cp", "-c", str(source), str(target)], check=True)
            else:
                shutil.copyfile(source, target)
        marker = {
            "format": 1,
            "module_id": AI,
            "resource_id": resource["id"],
            "version": resource["version"],
            "files": pins,
        }
        (version_dir / ".partial.json").write_text(json.dumps(marker))
        print(f"staged {resource['id']} {resource['version']}: {len(pins)} files")


def install(client):
    """Install every luxforge.ai resource under its grant, from what `stage` put in place."""
    timings = {}
    for resource in resources(client):
        answer = client.raw(
            "module.resource.install",
            {"module_id": AI, "resource_id": resource["id"], "mutation": mutation("install")},
        )
        error = answer.get("error")
        if error and error["code"] == "consent-required":
            consent = error["data"]["consent"]
            client.call(
                "module.permission.grant",
                {
                    "module_id": AI,
                    "capability": consent["capability"],
                    "scope": consent["scope"],
                    "mutation": mutation("grant"),
                },
            )
            answer = client.raw(
                "module.resource.install",
                {"module_id": AI, "resource_id": resource["id"], "mutation": mutation("install")},
            )
            error = answer.get("error")
        if error:
            raise SystemExit(f"install {resource['id']}: {json.dumps(error)}")
        if "job_id" not in answer["result"]:
            # Already installed: the answer names the installed version and starts no job.
            continue
        started = time.monotonic()
        record = client.settle(answer["result"]["job_id"], poll=1.0)
        if record["status"] != "ready":
            raise SystemExit(f"install {resource['id']}: {json.dumps(record)}")
        timings[resource["id"]] = round(time.monotonic() - started, 1)
    return timings


def uptime():
    return subprocess.run(["uptime"], capture_output=True, text=True).stdout.strip()


def time_report(stderr):
    """`/usr/bin/time -l`'s wall time, maximum resident set and peak footprint."""
    found = {}
    for key, pattern in [
        ("real_s", r"([\d.]+) real"),
        ("max_rss_bytes", r"(\d+)\s+maximum resident set size"),
        ("peak_footprint_bytes", r"(\d+)\s+peak memory footprint"),
    ]:
        match = re.search(pattern, stderr)
        if match:
            found[key] = float(match.group(1)) if "." in match.group(1) else int(match.group(1))
    return found


def editor(args):
    out = Path(args.out)
    (out / "klein").mkdir(parents=True, exist_ok=True)
    root = Path(args.root)
    report = {"cases": [], "uptime_start": uptime()}
    cases = json.loads(Path(args.cases).read_text())["cases"]
    only = set(args.only.split(",")) if args.only else None
    for case in cases:
        cid = case["id"]
        if only and cid not in only:
            continue
        stroke = json.loads((out / "inputs" / f"{cid}-stroke.json").read_text())
        before = uptime()
        client = Json(args.json, root, timed=True)
        installed = install(client)
        status = client.call("module.status", {"module_id": AI})
        tier = status["fill_tiers"][0]
        if not tier["offered"]:
            raise SystemExit(f"the generative tier is not offered: {json.dumps(tier)}")
        opened = time.monotonic()
        asset = open_photo(client, out / "inputs" / f"{cid}.jpg")
        open_s = time.monotonic() - opened
        runs = []
        seed = args.seed
        # Generate then Regenerate; with --once Generate alone; with --prompt a Replace and then a
        # prompt-free removal from the same seed, so one process holds each runtime slot in turn.
        if args.prompt:
            plan = [("replace", args.prompt), ("generate", None)]
        elif args.once:
            plan = [("generate", None)]
        else:
            plan = [("generate", None), ("regenerate", None)]
        for label, prompt in plan:
            params = {
                "asset_id": asset,
                "selection-points": stroke["points"],
                "selection-radius": stroke["radius"],
                "model": MODEL,
                "seed": seed,
                "mutation": mutation(label),
            }
            if prompt:
                params["prompt"] = prompt
            steps = []
            started = time.monotonic()

            def progress(record, steps=steps, started=started):
                text = (record.get("progress") or {}).get("message") or ""
                if text and (not steps or steps[-1][1] != text):
                    steps.append((round(time.monotonic() - started, 2), text))

            queued = client.call(TASK, params)
            record = client.settle(queued["job_id"], progress)
            elapsed = time.monotonic() - started
            if record["status"] != "ready":
                raise SystemExit(f"{cid} {label}: {json.dumps(record)}")
            result = record["result"]["result"]
            runs.append(
                {
                    "label": label,
                    "prompt": prompt,
                    "job_id": queued["job_id"],
                    "seed": seed,
                    "seconds": round(elapsed, 2),
                    "progress": steps,
                    "result": result,
                    "candidates": record["candidates"],
                    # Which runtime slots the process holds once the candidate is published.
                    "runtime_after": client.call("module.status", {"module_id": AI}).get("runtime"),
                }
            )
            print(f"{cid} {label} seed {seed}: {elapsed:.1f} s", flush=True)
            if label != "replace":
                seed = result["next_seed"]
        # Accept and export each candidate in turn, undoing between them.
        for run in runs:
            revision = client.call("asset.state", {"asset_id": asset})["revision"]
            accepted = client.call(
                "edit.accept-correction-patch",
                {
                    "asset_id": asset,
                    "job_id": run["job_id"],
                    "candidate": 0,
                    "mutation": {**mutation("accept"), "expected_revision": revision},
                },
            )
            kind = "replace-" if run["prompt"] else ""
            destination = out / "klein" / f"{cid}-{kind}seed{run['seed']}.jpg"
            if destination.exists():
                destination.unlink()
            exported = client.call(
                "export.write",
                {
                    "asset_id": asset,
                    "destination": str(destination.resolve()),
                    "settings": {
                        "format": "jpeg",
                        "size": {"fit": "full"},
                        "quality": 90,
                        "optimise": False,
                        "chroma": "444",
                        "limit_bytes": None,
                        "sharpen": None,
                        "metadata": "none",
                        "pixels_per_inch": None,
                        "folder": {"path": str(destination.parent.resolve())},
                        "suffix": "-edited",
                    },
                    "mutation": mutation("export"),
                },
            )
            done = client.settle(exported["job_id"])
            if done["status"] != "ready":
                raise SystemExit(f"export: {json.dumps(done)}")
            run["accepted"] = accepted.get("outcome")
            run["export"] = str(destination)
            revision = client.call("asset.state", {"asset_id": asset})["revision"]
            client.call(
                "history.undo",
                {"asset_id": asset, "mutation": {**mutation("undo"), "expected_revision": revision}},
            )
        module = client.call("module.status", {"module_id": AI})
        stderr = client.finish()
        report["cases"].append(
            {
                "id": cid,
                "installed_s": installed,
                "open_s": round(open_s, 2),
                "runs": runs,
                "runtime": module.get("runtime"),
                "process": time_report(stderr),
                "uptime_before": before,
                "uptime_after": uptime(),
            }
        )
        (out / "editor.json").write_text(json.dumps(report, indent=2))
    report["uptime_end"] = uptime()
    (out / "editor.json").write_text(json.dumps(report, indent=2))


def open_photo(client, path):
    started = client.call(
        "pick.develop",
        {
            "targets": {"kind": "paths", "paths": [str(path.resolve())]},
            "into": [],
            "confirm_removable": True,
            "mutation": mutation("develop"),
        },
    )
    developed = client.settle(started["job_id"])
    asset = developed["result"]["developed"][0]["asset_id"]
    prepared = client.call("source.prepare", {"asset_id": asset})
    client.settle(prepared["job_id"])
    client.call("job.adopt", {"job_id": prepared["job_id"]})
    return asset


# ---------------------------------------------------------------------------------------------
# lama


def lama(args):
    out = Path(args.out)
    for stroke_path in sorted((out / "inputs").glob("*-stroke.json")):
        cid = stroke_path.name[: -len("-stroke.json")]
        target = out / "lama" / cid
        target.mkdir(parents=True, exist_ok=True)
        command = [
            args.probe,
            "fill",
            "--image",
            str(out / "inputs" / f"{cid}.jpg"),
            "--mask",
            str(out / "inputs" / f"{cid}-mask.png"),
            "--out",
            str(target),
        ]
        if args.models:
            command += ["--models", args.models]
        subprocess.run(command, check=True)


# ---------------------------------------------------------------------------------------------
# sheets


def window(mask):
    ys, xs = np.nonzero(mask)
    x0, x1, y0, y1 = xs.min(), xs.max() + 1, ys.min(), ys.max() + 1
    side = max(x1 - x0, y1 - y0) * 2
    cx, cy = (x0 + x1) // 2, (y0 + y1) // 2
    height, width = mask.shape
    side = min(side, width, height)
    left = int(min(max(cx - side // 2, 0), width - side))
    top = int(min(max(cy - side // 2, 0), height - side))
    return left, top, left + side, top + side


def font(size):
    for name in ("/System/Library/Fonts/Helvetica.ttc", "DejaVuSans.ttf"):
        try:
            return ImageFont.truetype(name, size)
        except OSError:
            continue
    return ImageFont.load_default()


def sheets(args):
    out = Path(args.out)
    (out / "sheets").mkdir(parents=True, exist_ok=True)
    rng = random.Random(args.seed)
    key = {"seed": args.seed, "cases": {}}
    tile = args.tile
    big = font(28)
    for stroke_path in sorted((out / "inputs").glob("*-stroke.json")):
        cid = stroke_path.name[: -len("-stroke.json")]
        mask = np.array(Image.open(out / "inputs" / f"{cid}-mask.png")) > 0
        box = window(mask)
        methods = {}
        klein = sorted((out / "klein").glob(f"{cid}-seed*.jpg"))
        if klein:
            methods["klein"] = klein[0]
        lama_result = out / "lama" / cid / "result.png"
        if lama_result.exists():
            methods["lama"] = lama_result
        if args.lightroom:
            for ext in ("jpg", "jpeg", "png", "tif"):
                path = Path(args.lightroom) / f"{cid}.{ext}"
                if path.exists():
                    methods["lightroom"] = path
                    break
        if len(methods) < 2:
            print(f"{cid}: fewer than two results, no sheet")
            continue
        order = list(methods)
        rng.shuffle(order)
        letters = "ABCD"
        source = Image.open(out / "inputs" / f"{cid}.jpg").convert("RGB")
        outlined = source.crop(box).resize((tile, tile), Image.LANCZOS)
        edge = Image.fromarray(
            (mask[box[1] : box[3], box[0] : box[2]] * 255).astype(np.uint8)
        ).resize((tile, tile), Image.NEAREST)
        e = np.array(edge) > 0
        border = e ^ np.roll(e, 1, 0) | e ^ np.roll(e, 1, 1)
        o = np.array(outlined)
        o[border] = [255, 0, 0]
        outlined = Image.fromarray(o)
        columns = 1 + len(order)
        sheet = Image.new("RGB", (columns * (tile + 16) + 16, tile + 76), (24, 24, 24))
        draw = ImageDraw.Draw(sheet)
        sheet.paste(outlined, (16, 60))
        draw.text((16, 16), f"{cid}: region", fill=(230, 230, 230), font=big)
        for index, method in enumerate(order):
            image = Image.open(methods[method]).convert("RGB")
            if image.size != source.size:
                raise SystemExit(f"{methods[method]} is {image.size}, not {source.size}")
            x = 16 + (index + 1) * (tile + 16)
            sheet.paste(image.crop(box).resize((tile, tile), Image.LANCZOS), (x, 60))
            draw.text((x, 16), letters[index], fill=(230, 230, 230), font=big)
        sheet.save(out / "sheets" / f"{cid}.png")
        key["cases"][cid] = {letters[i]: m for i, m in enumerate(order)}
        print(f"{cid}: {', '.join(letters[: len(order)])}")
    (out / "key.json").write_text(json.dumps(key, indent=2))
    ballot = ["case,best,worst,notes"] + [f"{cid},,," for cid in key["cases"]]
    (out / "sheets" / "ballot.csv").write_text("\n".join(ballot) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("prepare")
    p.add_argument("--cases", required=True)
    p.add_argument("--probe", required=True)
    p.add_argument("--corpus")
    p.add_argument("--public")
    p.add_argument("--out", required=True)
    p.set_defaults(run=prepare)
    p = sub.add_parser("stage")
    p.add_argument("--json", required=True)
    p.add_argument("--models", required=True, help="the directory holding the local files")
    group = p.add_mutually_exclusive_group(required=True)
    group.add_argument("--root", help="a luxforge-json --data-root")
    group.add_argument(
        "--resources",
        help="a resources directory itself, such as the desktop's "
        "~/Library/Application Support/Luxforge/modules/resources",
    )
    p.set_defaults(run=stage)
    p = sub.add_parser("editor")
    p.add_argument("--json", required=True)
    p.add_argument("--root", required=True)
    p.add_argument("--cases", required=True)
    p.add_argument("--out", required=True)
    p.add_argument("--seed", type=int, default=0)
    p.add_argument("--only")
    p.add_argument("--once", action="store_true", help="Generate one candidate, no Regenerate")
    p.add_argument("--prompt", help="a Replace with this prompt, then a prompt-free removal")
    p.set_defaults(run=editor)
    p = sub.add_parser("lama")
    p.add_argument("--probe", required=True)
    p.add_argument("--models")
    p.add_argument("--out", required=True)
    p.set_defaults(run=lama)
    p = sub.add_parser("sheets")
    p.add_argument("--out", required=True)
    p.add_argument("--lightroom")
    p.add_argument("--seed", type=int, default=2026)
    p.add_argument("--tile", type=int, default=640)
    p.set_defaults(run=sheets)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
