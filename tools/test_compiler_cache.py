"""Real compiler-cache correctness probes; requires the pinned local sccache installation."""
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
WRAPPER = ROOT / "tools/cargo-cached"


class CompilerCacheTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory(prefix="lux-cache-", dir="/tmp")
        cls.work = Path(cls.tmp.name).resolve()
        fake_bin = cls.work / "bin"
        fake_bin.mkdir()
        fake_cargo = fake_bin / "cargo"
        keys = ["SCCACHE_CONF", "SCCACHE_DIR", "SCCACHE_SERVER_UDS", "SCCACHE_DIRECT",
                "SCCACHE_CACHE_SIZE", "SCCACHE_C_CUSTOM_CACHE_BUSTER", "RUSTC_WRAPPER",
                "LUXFORGE_SCCACHE_BIN", "PATH", "CC", "CXX"]
        fake_cargo.write_text("#!/usr/bin/env python3\nimport os,json\n"
                              f"keys={keys!r}\n"
                              "unexpected=[k for k in os.environ if k.startswith('SCCACHE_') and k not in keys]\n"
                              "assert not unexpected,unexpected\n"
                              "print(json.dumps({k:os.environ[k] for k in keys}))\n")
        fake_cargo.chmod(0o755)
        env = dict(os.environ, PATH=f"{fake_bin}:{os.environ['PATH']}",
                   LUXFORGE_SCCACHE_CACHE=str(cls.work / "cache"),
                   SCCACHE_BASEDIRS=str(cls.work), SCCACHE_DIRECT="true",
                   SCCACHE_BUCKET="must-not-use-remote-cache")
        env.pop("RUSTC_WRAPPER", None)
        captured = subprocess.check_output([str(WRAPPER), "--version"], env=env, text=True)
        cls.env = dict(os.environ, **json.loads(captured))
        for key in list(cls.env):
            if key.startswith("SCCACHE_") and key not in keys:
                del cls.env[key]
        cls.tool = cls.env["LUXFORGE_SCCACHE_BIN"]
        cls.cc = shlex.split(cls.env["CC"])
        cls.rustc = shutil.which("rustc")
        cls.probes = 0

    @classmethod
    def tearDownClass(cls):
        subprocess.run([cls.tool, "--stop-server"], env=cls.env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
        cls.tmp.cleanup()

    def directory(self):
        type(self).probes += 1
        directory = self.work / f"probe-{self.probes}"
        directory.mkdir()
        return directory

    def run_command(self, args, **kwargs):
        return subprocess.check_output(args, env=kwargs.pop("env", self.env), text=True, **kwargs)

    def stats(self):
        return json.loads(self.run_command([self.tool, "--show-stats", "--stats-format", "json"]))

    def native(self, source, flags=()):
        obj = source.with_suffix(".o")
        exe = source.with_suffix(".exe")
        self.run_command([*self.cc, *flags, "-c", str(source), "-o", str(obj)])
        self.run_command([*self.cc[1:], str(obj), "-o", str(exe)])
        return self.run_command([str(exe)])

    def test_native_hit_then_header_optional_include_and_flags_invalidate(self):
        directory = self.directory()
        header = directory / "value.h"
        header.write_text("#define VALUE 1\n")
        source = directory / "test.c"
        source.write_text('#include <stdio.h>\n#include "value.h"\n'
                          '#if __has_include("optional.h")\n#include "optional.h"\n'
                          '#else\n#define EXTRA 0\n#endif\n'
                          '#ifndef FLAG\n#define FLAG 0\n#endif\n'
                          'int main(void) { printf("%d:%d:%d", VALUE, EXTRA, FLAG); }\n')
        self.assertEqual(self.native(source), "1:0:0")
        before = self.stats()["stats"]["cache_hits"]
        self.assertEqual(self.native(source), "1:0:0")
        self.assertNotEqual(self.stats()["stats"]["cache_hits"], before)
        stamp = header.stat()
        header.write_text("#define VALUE 2\n")
        os.utime(header, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
        self.assertEqual(self.native(source), "2:0:0")
        (directory / "optional.h").write_text("#define EXTRA 7\n")
        self.assertEqual(self.native(source), "2:7:0")
        self.assertEqual(self.native(source, ["-DFLAG=9"]), "2:7:9")
        stats = self.stats()
        self.assertFalse(stats["use_preprocessor_cache_mode"])
        self.assertEqual(stats["basedirs"], [])

    def test_identical_checkout_sources_preserve_their_own_file_paths(self):
        sources = []
        for _ in range(2):
            source = self.directory() / "test.c"
            source.write_text('#include <stdio.h>\nint main(void) { puts(__FILE__); }\n')
            sources.append(source)
        for source in sources:
            self.assertEqual(self.native(source).strip(), str(source))
            # A checkout path can be program data, not just a diagnostic filename.
            literal = str(source.parent / "literal-data")
            source.write_text(f'#include <stdio.h>\nint main(void) {{ puts("{literal}"); }}\n')
            self.assertEqual(self.native(source).strip(), literal)

    def rust_library(self, source, env, extra=()):
        self.run_command([str(ROOT / "tools/rustc-cache"), self.rustc, "--crate-name", "probe",
                          "--crate-type", "rlib", "--emit", "link,dep-info", "--out-dir",
                          str(source.parent), str(source), *extra], env=env)
        main = source.parent / "main.rs"
        main.write_text('fn main() { println!("{}", probe::value()); }')
        exe = source.parent / "run"
        self.run_command([self.rustc, str(main), "--extern",
                          f"probe={source.parent / 'libprobe.rlib'}", "-L",
                          f"dependency={source.parent}", "-o", str(exe)], env=env)
        return self.run_command([str(exe)], env=env).strip()

    def test_registry_leaf_hit_then_source_and_env_invalidate(self):
        directory = self.directory()
        source = directory / "lib.rs"
        source.write_text('pub fn value() -> &\'static str { env!("PROBE_VALUE") }')
        env = dict(self.env, CARGO_HOME=str(self.work / "cargo"),
                   CARGO_MANIFEST_DIR=str(self.work / "cargo/registry/src/probe"), PROBE_VALUE="one")
        self.assertEqual(self.rust_library(source, env), "one")
        before = self.stats()["stats"]["cache_hits"]
        self.assertEqual(self.rust_library(source, env), "one")
        self.assertNotEqual(self.stats()["stats"]["cache_hits"], before)
        env["PROBE_VALUE"] = "two"
        self.assertEqual(self.rust_library(source, env), "two")
        source.write_text('pub fn value() -> &\'static str { "changed" }')
        self.assertEqual(self.rust_library(source, env), "changed")

    def test_workspace_rust_bypasses_the_cache(self):
        directory = self.directory()
        source = directory / "lib.rs"
        source.write_text('pub fn value() -> &\'static str { "workspace" }')
        env = dict(self.env, CARGO_MANIFEST_DIR=str(directory))
        before = self.stats()["stats"]["compile_requests"]
        self.assertEqual(self.rust_library(source, env), "workspace")
        self.assertEqual(self.stats()["stats"]["compile_requests"], before)

    def test_reexported_macro_reads_changed_untracked_file_without_a_cached_result(self):
        directory = self.directory()
        payload = directory / "payload.txt"
        payload.write_text("one")
        macro = directory / "disk_macro.rs"
        macro.write_text('extern crate proc_macro;\n'
                         '#[proc_macro] pub fn value(_: proc_macro::TokenStream) -> proc_macro::TokenStream {\n'
                         'let text = std::fs::read_to_string(std::env::var("PROBE_INPUT").unwrap()).unwrap();\n'
                         'format!("{:?}", text).parse().unwrap() }')
        self.run_command([self.rustc, "--crate-name", "disk_macro", "--crate-type", "proc-macro",
                          "--out-dir", str(directory), str(macro)])
        library = next(directory.glob("libdisk_macro.*"))
        bridge = directory / "bridge.rs"
        bridge.write_text('extern crate disk_macro; pub use disk_macro::value;')
        self.run_command([self.rustc, "--crate-name", "bridge", "--crate-type", "rlib",
                          "--out-dir", str(directory), "--extern", f"disk_macro={library}", str(bridge)])
        source = directory / "lib.rs"
        source.write_text('extern crate bridge; pub fn value() -> &\'static str { bridge::value!() }')
        env = dict(self.env, CARGO_HOME=str(self.work / "cargo"), PROBE_INPUT=str(payload),
                   CARGO_MANIFEST_DIR=str(self.work / "cargo/registry/src/probe"))
        extra = ["--extern", f"bridge={directory / 'libbridge.rlib'}", "-L", f"dependency={directory}"]
        before = self.stats()["stats"]["compile_requests"]
        self.assertEqual(self.rust_library(source, env, extra), "one")
        payload.write_text("two")
        self.assertEqual(self.rust_library(source, env, extra), "two")
        self.assertEqual(self.stats()["stats"]["compile_requests"], before)


if __name__ == "__main__":
    unittest.main()
