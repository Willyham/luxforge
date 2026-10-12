"""tools/cargo-cached's build-volume target link and host-wide build slots, against a fake Cargo.

Each test copies the wrapper into a scratch checkout, so the link it makes is the scratch one's.
The compiler cache is off (`LUXFORGE_CACHE_OFF=1`): these need no sccache installation.
"""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
HANG = 60

FAKE_CARGO = """#!/usr/bin/env python3
import os, sys, time
from pathlib import Path
gates = Path(os.environ["FAKE_GATES"])
name = os.environ.get("FAKE_NAME", "run")
(gates / f"started-{name}").write_text(" ".join(sys.argv[1:]) + "\\n" + os.environ.get("LUXFORGE_BUILD_SLOT", ""))
if os.environ.get("FAKE_HOLD"):
    deadline = time.monotonic() + 60
    while not (gates / f"release-{name}").exists():
        assert time.monotonic() < deadline, "never released"
        time.sleep(0.05)
sys.exit(int(os.environ.get("FAKE_EXIT", "0")))
"""


def wait_until(what, condition):
    deadline = time.monotonic() + HANG
    while not condition():
        if time.monotonic() > deadline:
            raise AssertionError(f"{what} never happened")
        time.sleep(0.05)


class BuildSlotTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="lux-slots-", dir="/tmp")
        work = Path(self.tmp.name).resolve()
        self.repo = work / "checkout"
        (self.repo / "tools").mkdir(parents=True)
        shutil.copy2(ROOT / "tools/cargo-cached", self.repo / "tools/cargo-cached")
        self.volume = work / "volume"
        self.volume.mkdir()
        self.gates = work / "gates"
        self.gates.mkdir()
        self.slots = work / "slots"
        fake_bin = work / "bin"
        fake_bin.mkdir()
        (fake_bin / "cargo").write_text(FAKE_CARGO)
        (fake_bin / "cargo").chmod(0o755)
        self.env = dict(os.environ, PATH=f"{fake_bin}:{os.environ['PATH']}", LUXFORGE_CACHE_OFF="1",
                        LUXFORGE_BUILD_ROOT=str(self.volume), LUXFORGE_BUILD_SLOT_DIR=str(self.slots),
                        LUXFORGE_BUILD_SLOTS="1", FAKE_GATES=str(self.gates))
        for key in ("CARGO_TARGET_DIR", "LUXFORGE_BUILD_SLOT"):
            self.env.pop(key, None)
        self.processes = []

    def tearDown(self):
        for process in self.processes:
            if process.poll() is None:
                process.kill()
            process.wait()
        self.tmp.cleanup()

    def wrapper(self, *args, **env):
        return subprocess.run([str(self.repo / "tools/cargo-cached"), *args], env=dict(self.env, **env),
                              cwd=self.repo, capture_output=True, text=True, timeout=HANG)

    def start(self, name, *args, hold=True):
        env = dict(self.env, FAKE_NAME=name, FAKE_HOLD="1" if hold else "")
        with open(self.gates / f"stderr-{name}", "w") as stderr:
            process = subprocess.Popen([str(self.repo / "tools/cargo-cached"), *args], env=env, cwd=self.repo,
                                       stdout=subprocess.DEVNULL, stderr=stderr)
        self.processes.append(process)
        return process

    def stderr(self, name):
        return (self.gates / f"stderr-{name}").read_text()

    def started(self, name):
        return (self.gates / f"started-{name}").exists()

    def release(self, name, process):
        (self.gates / f"release-{name}").write_text("")
        process.wait(timeout=HANG)
        return process

    def test_target_links_to_the_worktrees_own_build_volume_directory(self):
        self.assertEqual(self.wrapper("build").returncode, 0)
        link = self.repo / "target"
        self.assertTrue(link.is_symlink())
        directory = Path(os.readlink(link))
        self.assertEqual(directory.parent, self.volume / "targets")
        self.assertTrue(directory.name.startswith("checkout-"))
        self.assertEqual((directory / ".worktree").read_text().strip(), str(self.repo))
        self.assertEqual(self.wrapper("build").returncode, 0)
        self.assertEqual(Path(os.readlink(link)), directory)

    def test_an_existing_target_an_explicit_target_dir_or_no_volume_is_left_alone(self):
        (self.repo / "target").mkdir()
        self.assertEqual(self.wrapper("build").returncode, 0)
        self.assertFalse((self.repo / "target").is_symlink())
        (self.repo / "target").rmdir()
        self.assertEqual(self.wrapper("build", CARGO_TARGET_DIR=str(self.repo / "elsewhere")).returncode, 0)
        self.assertEqual(self.wrapper("build", LUXFORGE_BUILD_ROOT="").returncode, 0)
        self.assertEqual(self.wrapper("build", LUXFORGE_BUILD_ROOT=str(self.volume / "absent")).returncode, 0)
        self.assertFalse((self.repo / "target").exists() or (self.repo / "target").is_symlink())
        self.assertFalse((self.volume / "targets").exists())

    def test_prune_lists_then_deletes_only_directories_no_worktree_links_to(self):
        self.assertEqual(self.wrapper("build").returncode, 0)
        kept = Path(os.readlink(self.repo / "target"))
        orphan = self.volume / "targets" / "gone-0123456789ab"
        orphan.mkdir()
        (orphan / ".worktree").write_text(str(self.volume / "gone") + "\n")
        listed = self.wrapper("--prune-targets")
        self.assertIn(f"orphaned {orphan}", listed.stdout)
        self.assertNotIn(str(kept) + " ", listed.stdout)
        self.assertTrue(orphan.exists())
        self.assertEqual(self.wrapper("--prune-targets", "--delete").returncode, 0)
        self.assertFalse(orphan.exists())
        self.assertTrue(kept.exists())

    def test_a_build_waits_for_a_slot_and_takes_it_when_released(self):
        first = self.start("first", "test", "-p", "core")
        wait_until("the first build to start", lambda: self.started("first"))
        second = self.start("second", "build", hold=False)
        wait_until("the second build to report it is waiting",
                   lambda: "all 1 build slots are busy" in self.stderr("second"))
        self.assertIn("cargo test -p core", self.stderr("second"))
        self.assertFalse(self.started("second"))
        self.release("first", first)
        self.assertEqual(first.returncode, 0)
        self.assertEqual(second.wait(timeout=HANG), 0)
        self.assertTrue(self.started("second"))
        self.assertIn("build slot 1 taken after", self.stderr("second"))
        self.assertEqual(list(self.slots.iterdir()), [])

    def test_commands_that_build_nothing_and_nested_cargo_take_no_slot(self):
        first = self.start("first", "build")
        wait_until("the first build to start", lambda: self.started("first"))
        for args in (("fmt", "--all"), ("metadata",), ("xtask", "develop", "--background"), ("--slots",)):
            self.assertEqual(self.wrapper(*args, FAKE_NAME="light").returncode, 0, args)
        held = (self.gates / "started-first").read_text().splitlines()[1]
        self.assertEqual(self.wrapper("build", FAKE_NAME="nested", LUXFORGE_BUILD_SLOT=held).returncode, 0)
        self.assertIn("pid", self.wrapper("--slots").stdout)
        self.release("first", first)

    def test_a_slot_whose_owner_has_gone_is_reclaimed(self):
        gone = subprocess.Popen(["true"])
        gone.wait()
        slot = self.slots / "1"
        slot.mkdir(parents=True)
        (slot / "owner").write_text(f"{gone.pid}\nMon Jan  1 00:00:00 2024\n/elsewhere\nbuild\n")
        self.assertEqual(self.wrapper("build", FAKE_NAME="after").returncode, 0)
        self.assertTrue(self.started("after"))
        self.assertEqual(list(self.slots.iterdir()), [])

    def test_cargos_exit_status_is_returned_and_the_slot_released(self):
        failed = self.wrapper("build", FAKE_EXIT="101")
        self.assertEqual(failed.returncode, 101)
        self.assertEqual(list(self.slots.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
