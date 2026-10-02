#!/usr/bin/env python3
"""Build pinned CC0 provenance for the reviewed camera/phone target list.

Uses the supplied raw.pixls.us index and HTTPS HEAD to pin exact encoded sizes.
Existing expectations are preserved by content hash; new files are candidates.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import html
import json
from pathlib import Path
import re
import urllib.parse
import urllib.request

from sample_corpus import ROOT, MAX_SOURCE_BYTES, atomic_json, load_manifest


def index_entries(path):
    records = []
    for row in json.loads(path.read_text())["data"]:
        if "creativecommons.org/publicdomain/zero/1.0/" not in row[5]:
            continue
        link = re.search(r"href='([^']+)'", row[7])
        sha = re.search(r"sha256 Checksum'>([a-f0-9]{64})", row[7])
        sid = re.search(r"/getfile.php/(\d+)/", row[7])
        if not all((link, sha, sid)):
            continue
        url = html.unescape(link[1])
        records.append({"upstream_id": sid[1], "make": row[0], "model": row[1], "description": row[2],
                        "source_url": url, "sha256": sha[1], "extension": Path(urllib.parse.urlsplit(url).path).suffix})
    return records


def normalized(value):
    return re.sub(r"[^a-z0-9]", "", value.lower().replace("+", "plus"))


def build(targets, entries, previous=None):
    existing = {s["sha256"]: s for s in (previous or {}).get("samples", [])}
    known_sizes = {}
    frozen = {}
    evidence = {e["source_sha256"]: e for e in json.loads((ROOT / "fixtures/modern-camera-evidence.json").read_text())["entries"]}
    for model in json.loads((ROOT / "fixtures/modern-camera-selection.json").read_text())["models"]:
        for sample in model["samples"]:
            if sample["license"] != "CC0-1.0":
                continue
            known_sizes[sample["sha256"]] = sample["encoded_bytes"]
            e = evidence[sample["sha256"]]
            frozen[sample["sha256"]] = {"kind": "qualified", "mode": e["mode"], "mosaic_sha256": e["mosaic_sha256"],
                                         "as_shot": e["as_shot"], "perturbed_wb": e["perturbed_wb"]}
            metadata = {"sensor_width": e["sensor"]["width"], "sensor_height": e["sensor"]["height"],
                        "active_area": e["active_area"], "default_crop": e["default_crop"], "warnings": e.get("warnings", [])}
            if "dng_corrections" in e:
                metadata["dng_corrections"] = e["dng_corrections"]
            if "color_calibration" in e:
                metadata.update(cam_xyz=e["color_calibration"]["xyz_to_camera"], rgb_cam=e["color_calibration"]["camera_to_linear_srgb"])
            frozen[sample["sha256"]]["metadata"] = metadata
    pins = {"3582": ("NikonZ6Lossless14", "9896187fd3e3e29922b5b051a62f24afedbbbb75ddf63b5879a2b28de896116c"),
            "3585": ("NikonZ6Lossless12", "f25f0aafd76a99f5f20cbe2a001f4620397be43de829010d5dbaf9255ce64424"),
            "7301": ("FujifilmX100ViLossless14", "f10be69db8c3731fdcacf3741fd188fcef2557efd5de79f84a22a34adf443283")}
    devices, samples, used = [], [], set()
    for target in targets["devices"]:
        aliases = {normalized(v) for v in target.get("source_models", [target["model"]])}
        matches = [e for e in entries if e["make"].lower() == target["make"].lower() and normalized(e["model"]) in aliases]
        preserved = [e for e in matches if e["sha256"] in frozen or e["upstream_id"] in pins or e["sha256"] in existing]
        if target["kind"] == "phone":
            chosen = matches
        elif preserved:
            chosen = preserved
        else:
            # Prefer full-size ordinary captures; retain at most two distinct descriptions.
            ordinary = [e for e in matches if not re.search(r"sraw|mraw|small|pixel.?shift|high.?res", e["description"], re.I)]
            choices, descriptions = [], set()
            for e in sorted(ordinary or matches, key=lambda e: int(e["upstream_id"])):
                if e["description"] not in descriptions:
                    choices.append(e)
                    descriptions.add(e["description"])
            chosen = choices[:2]
        device = dict(target, sample_ids=[])
        for entry in chosen:
            sha = entry["sha256"]
            if sha in used:
                continue
            sid = "pixls-" + entry["upstream_id"]
            expectation = {**frozen.get(sha, {}), **existing.get(sha, {}).get("expectation", {})} or {"kind": "candidate"}
            if entry["upstream_id"] in pins and expectation.get("kind") != "qualified":
                mode, mosaic = pins[entry["upstream_id"]]
                expectation = {"kind": "mosaic", "mode": mode, "mosaic_sha256": mosaic}
            sample = {"id": sid, "device": target["id"], "group": "phones" if target["kind"] == "phone" else normalized(target["make"]),
                      "source_url": entry["source_url"], "source_id": entry["upstream_id"], "source_description": entry["description"],
                      "sha256": sha, "key": f"sha256/{sha[:2]}/{sha}", "license": "CC0-1.0",
                      "extension": entry["extension"], "bytes": existing.get(sha, {}).get("bytes") or known_sizes.get(sha),
                      "expectation": expectation}
            device["sample_ids"].append(sid)
            samples.append(sample)
            used.add(sha)
        device["availability"] = "available" if device["sample_ids"] else "missing-licensed-sample"
        devices.append(device)

    def size(sample):
        if sample["bytes"] is None:
            url = urllib.parse.quote(sample["source_url"], safe=":/?=&%")
            request = urllib.request.Request(url, method="HEAD", headers={"User-Agent": "Luxforge-sample-corpus/1"})
            with urllib.request.urlopen(request, timeout=60) as response:
                sample["bytes"] = int(response.headers["Content-Length"])
        if not 0 < sample["bytes"] <= MAX_SOURCE_BYTES:
            raise ValueError(f"{sample['id']}: source exceeds encoded input bound")
        return sample
    with ThreadPoolExecutor(max_workers=4) as executor:
        samples = list(executor.map(size, samples))
    return {"format": "luxforge-sample-corpus-v1", "selection_date": targets["selection_date"],
            "scope": "Curated 250-camera starter set and five flagship phone generations; not an active-use census or support claim",
            "development_reference": "Existing qualified development hashes were recorded on the owner's M4 Mac; portable tests compare exact mosaics, frozen metadata, dimensions, finite output and output ranges; --strict-development also compares development hashes.",
            "upstream": "https://raw.pixls.us/json/getrepository.php", "devices": devices, "samples": samples}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--index", type=Path, required=True)
    parser.add_argument("--targets", type=Path, default=ROOT / "fixtures/sample-corpus-targets.json")
    parser.add_argument("--output", type=Path, default=ROOT / "fixtures/sample-corpus.json")
    parser.add_argument("--previous", type=Path, help="preserve reviewed expectations by source hash")
    args = parser.parse_args()
    data = build(json.loads(args.targets.read_text()), index_entries(args.index), load_manifest(args.previous) if args.previous else None)
    atomic_json(args.output, data)
    load_manifest(args.output)
    print(f"pinned {len(data['samples'])} CC0 samples for {len(data['devices'])} targets")


if __name__ == "__main__":
    main()
