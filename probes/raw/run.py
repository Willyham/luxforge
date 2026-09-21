"""Run pinned local decoder probes. Source files are opened read-only and rehashed."""
from pathlib import Path
import hashlib
import subprocess
import sys

if len(sys.argv) != 3:
    raise SystemExit('usage: python3 run.py OWNER_RAW_DIRECTORY NEW_OUTPUT_DIRECTORY')
root = Path(__file__).resolve().parents[2]
owner = Path(sys.argv[1])
base = Path(sys.argv[2])
base.mkdir(parents=True, exist_ok=False)
files = [(name, owner / filename) for name, filename in (
    ('owner-z6', 'nikon_z6.NEF'),
    ('owner-x100vi', 'fujifilm_x100vi.RAF'),
    ('owner-dji', 'mavic_air_2s.DNG'),
)]
files += [(p.stem, p) for p in sorted((root / 'private/raw').glob('*')) if p.suffix.upper() in ('.NEF', '.RAF', '.DNG')]
for name, src in files:
    print(name, flush=True)
    with src.open('rb') as f:
        original = hashlib.file_digest(f, 'sha256').hexdigest()
    for backend, binary in (
        ('rawler', root / 'probes/raw/target/release/lightwell-raw-probe'),
        ('libraw', root / 'private/raw-backends/libraw_probe'),
    ):
        out = base / (name + '-' + backend)
        proc = subprocess.run(['/usr/bin/time', '-l', str(binary), str(src), str(out)], text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        (base / (name + '-' + backend + '.time.txt')).write_text(proc.stderr)
        print(backend, proc.returncode, proc.stdout.strip(), flush=True)
        if proc.returncode:
            (base / (name + '-' + backend + '.error.txt')).write_text(proc.stderr)
    with src.open('rb') as f:
        after = hashlib.file_digest(f, 'sha256').hexdigest()
    if original != after:
        raise RuntimeError(name + ': source changed')
    (base / (name + '.source.sha256')).write_text(original + '  ' + str(src) + '\n')
