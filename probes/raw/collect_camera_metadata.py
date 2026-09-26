#!/usr/bin/env python3
"""Fetch a selected CC0 RAW subset and inspect it with the pinned native probe.

Never mirrors the corpus. Every requested ID must have a declared CC0 license,
SHA-256 and size. Originals and per-run metadata stay in a new ignored directory.
"""
import argparse
import concurrent.futures
import hashlib
import html
import json
from pathlib import Path
import re
import subprocess
import urllib.parse
import urllib.request


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def entries(index, ids):
    found = {}
    for row in index['data']:
        match = re.search(r"href='([^']+)'", row[7])
        if not match:
            continue
        url = html.unescape(match[1])
        number = re.search(r'/getfile.php/(\d+)/', url)
        if not number or number[1] not in ids:
            continue
        if 'creativecommons.org/publicdomain/zero/1.0/' not in row[5]:
            raise ValueError(f"sample {number[1]} is not declared CC0")
        digest = re.search(r"sha256 Checksum'>([0-9a-f]{64})", row[7])
        if not digest:
            raise ValueError(f"sample {number[1]} has no SHA-256")
        found[number[1]] = dict(id=number[1], make=row[0], model=row[1], mode=row[2],
                               source_url=url, sha256=digest[1], license='CC0-1.0')
    missing = ids - found.keys()
    if missing:
        raise ValueError(f'missing sample IDs: {sorted(missing)}')
    return [found[key] for key in sorted(found, key=int)]


def fetch(sample, root, limit):
    source_url = urllib.parse.urlsplit(sample['source_url'])
    url = urllib.parse.urlunsplit(source_url._replace(path=urllib.parse.quote(source_url.path, safe='/')))
    suffix = Path(source_url.path).suffix
    path = root / f"{sample['id']}{suffix}"
    result = dict(sample, path=str(path.resolve()))
    try:
        request = urllib.request.Request(url, headers={'User-Agent': 'Luxforge-camera-qualification/1'})
        with urllib.request.urlopen(request, timeout=60) as response, path.open('xb') as target:
            total = 0
            while chunk := response.read(1024 * 1024):
                total += len(chunk)
                if total > limit:
                    raise ValueError(f'source exceeds {limit} bytes')
                target.write(chunk)
        actual = sha256(path)
        if actual != sample['sha256']:
            raise ValueError(f'SHA-256 mismatch: {actual}')
        result.update(bytes=total, download='passed')
    except Exception as error:
        result.update(download='failed', error=str(error))
    print(sample['id'], sample['make'], sample['model'], result['download'], flush=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--index', type=Path, required=True)
    parser.add_argument('--ids', nargs='+', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--probe', type=Path, required=True)
    parser.add_argument('--max-source-mib', type=int, default=128)
    args = parser.parse_args()
    ids = set(args.ids)
    if not 1 <= len(ids) <= 128 or not 1 <= args.max_source_mib <= 512:
        parser.error('select 1..128 samples, bounded to at most 512 MiB each')
    samples = entries(json.loads(args.index.read_text()), ids)
    args.output.mkdir(parents=True, exist_ok=False)
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        results = list(pool.map(lambda sample: fetch(sample, args.output, args.max_source_mib * 1024 * 1024), samples))
    for result in results:
        if result['download'] != 'passed':
            continue
        output = args.output / f"probe-{result['id']}"
        try:
            run = subprocess.run([str(args.probe.resolve()), result['path'], str(output), '--metadata-only'],
                                 capture_output=True, text=True, timeout=120)
            result.update(probe='passed' if run.returncode == 0 else 'failed', probe_log=run.stdout + run.stderr)
            if run.returncode == 0:
                result['metadata'] = json.loads((output / 'result.json').read_text())
            result['source_preserved'] = sha256(Path(result['path'])) == result['sha256']
        except Exception as error:
            result.update(probe='failed', probe_log=str(error))
        print(result['id'], 'probe', result['probe'], flush=True)
        (args.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    (args.output / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    return 0 if all(r.get('probe') == 'passed' and r.get('source_preserved') for r in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
