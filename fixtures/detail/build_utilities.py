#!/usr/bin/env python3
"""Link Detail evidence utilities to existing built crates, without another Cargo build."""
import os
from pathlib import Path
import subprocess
import tempfile

root = Path(__file__).resolve().parents[2]
deps = root / "target/debug/deps"


def latest(crate):
    paths = list(deps.glob(f"lib{crate}-*.rlib"))
    if not paths:
        raise SystemExit(f"Build {crate} first; no existing rlib in {deps}")
    return max(paths, key=lambda path: path.stat().st_mtime)


for name, crates in [
    ("render_corpus", ["luxforge_core", "image", "sha2"]),
    ("production_probes", ["luxforge_core", "luxforge_reference", "serde"]),
]:
    destination = Path(tempfile.gettempdir()) / f"detail-{name.replace('_', '-')}"
    args = [
        "rustc", "--edition=2024", str(root / f"fixtures/detail/{name}.rs"),
        "-L", f"dependency={deps}", "-o", str(destination),
    ]
    for crate in crates:
        args += ["--extern", f"{crate}={latest(crate)}"]
    env = dict(os.environ, CARGO_MANIFEST_DIR=str(root / "crates/luxforge-reference"))
    # Different feature/profile builds can leave several serde_json crate identities. Pick the
    # one the selected core actually uses, rather than assuming its newest timestamp matches.
    for json_crate in deps.glob("libserde_json-*.rlib"):
        result = subprocess.run(
            args + ["--extern", f"serde_json={json_crate}"],
            cwd=root, env=env, capture_output=True, text=True,
        )
        if result.returncode == 0:
            print(destination)
            break
    else:
        raise SystemExit(result.stderr if "result" in locals() else "No serde_json rlib")
