"""Offline tests for corpus integrity, bounded selection and source-safe cleanup."""
import argparse
from contextlib import redirect_stderr, redirect_stdout
import copy
import hashlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import sample_corpus as corpus
from build_sample_manifest import build, normalized


def sample(payload=b"authentic-test-bytes", sid="sample-1", group="nikon"):
    sha = hashlib.sha256(payload).hexdigest()
    return {"id": sid, "device": "camera-1", "group": group, "sha256": sha,
            "bytes": len(payload), "key": f"sha256/{sha[:2]}/{sha}", "extension": ".NEF",
            "license": "CC0-1.0", "source_url": "https://raw.pixls.us/getfile.php/1/nice/test.NEF",
            "expectation": {"kind": "qualified", "mode": "TestRaw14", "mosaic_sha256": "a" * 64,
                            "as_shot": {"width": 2, "height": 2, "sha256": "b" * 64},
                            "perturbed_wb": {"width": 2, "height": 2, "sha256": "c" * 64}}}


def result(s):
    return {"id": s["id"], "status": "passed", "source_sha256": s["sha256"],
            "source_sha256_after": s["sha256"], "mosaic_sha256": "a" * 64,
            "metadata": {"mode": "TestRaw14"},
            "as_shot": {"width": 2, "height": 2, "finite": True, "sha256": "b" * 64},
            "perturbed_wb": {"width": 2, "height": 2, "finite": True, "sha256": "c" * 64}}


class CorpusTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve()
        self.s = sample()
        self.data = {"format": "luxforge-sample-corpus-v1", "devices": [{"name": "Test camera", "sample_ids": [self.s["id"]]}], "samples": [self.s]}

    def tearDown(self):
        self.tmp.cleanup()

    def test_rebuilding_a_public_pin_preserves_reviewed_development(self):
        previous = copy.deepcopy(self.s)
        previous["source_id"] = "3582"
        entry = {"make": "Nikon", "model": "Z6", "upstream_id": "3582",
                 "sha256": previous["sha256"], "extension": ".NEF",
                 "source_url": previous["source_url"], "description": "lossless"}
        targets = {"selection_date": "2026-10-02", "devices": [
            {"id": "camera-1", "make": "Nikon", "model": "Z6", "kind": "camera"}]}
        rebuilt = build(targets, [entry], {"samples": [previous]})
        self.assertEqual(rebuilt["samples"][0]["expectation"], previous["expectation"])

    def test_manifest_refuses_owner_license_unsafe_key_and_oversize(self):
        for key, value in (("license", "private-permission"), ("key", "../source.NEF"),
                           ("bytes", corpus.MAX_SOURCE_BYTES + 1), ("extension", "/../NEF"),
                           ("source_url", "https://example.com/unreviewed.NEF")):
            with self.subTest(key=key):
                data = copy.deepcopy(self.data)
                data["samples"][0][key] = value
                path = self.root / "manifest.json"
                path.write_text(json.dumps(data))
                with self.assertRaises(corpus.CorpusError):
                    corpus.load_manifest(path)

    def test_shards_are_disjoint_and_complete(self):
        samples = [sample(str(i).encode(), f"sample-{i}") for i in range(50)]
        shards = []
        for i in range(4):
            args = argparse.Namespace(group=None, qualified_only=False, shard=f"{i}/4", limit=None)
            shards.append({s["id"] for s in corpus.selected({"samples": samples}, args)})
        self.assertEqual(set.union(*shards), {s["id"] for s in samples})
        self.assertEqual(sum(map(len, shards)), len(samples))

    def test_unknown_group_and_invalid_shard_fail(self):
        for group, shard in ((["missing"], None), (None, "4/4"), (None, "0/0")):
            with self.assertRaises(corpus.CorpusError):
                corpus.selected(self.data, argparse.Namespace(group=group, qualified_only=False, shard=shard, limit=None))

    def test_offline_rejects_network_commands_before_credentials(self):
        with patch.object(corpus, "credentials", side_effect=AssertionError("must not access credentials")), redirect_stderr(io.StringIO()):
            self.assertEqual(corpus.main(["publish", "--offline"]), 1)

    def test_phone_plus_variant_stays_distinct(self):
        self.assertNotEqual(normalized("Galaxy S23+"), normalized("Galaxy S23"))
        self.assertEqual(normalized("Galaxy S23+"), normalized("Galaxy S23 Plus"))

    def test_r2_download_binds_pinned_size_to_object_version(self):
        remote = object.__new__(corpus.R2)
        remote.config = {"bucket": "luxforge-samples"}
        head = {"ContentLength": self.s["bytes"], "Metadata": {"sha256": self.s["sha256"]}, "ETag": '"pinned-version"'}
        with patch.object(remote, "head", return_value=head), patch.object(remote, "call") as call:
            remote.download(self.s, self.root / "download")
            self.assertIn("--if-match", call.call_args.args)
            self.assertIn(head["ETag"], call.call_args.args)
        with patch.object(remote, "head", return_value=dict(head, ContentLength=corpus.MAX_SOURCE_BYTES + 1)), patch.object(remote, "call") as call:
            with self.assertRaises(corpus.CorpusError):
                remote.download(self.s, self.root / "download")
            call.assert_not_called()

    def test_chunk_count_and_byte_budget(self):
        samples = [dict(self.s, bytes=300 * 1024**2) for _ in range(5)]
        groups = list(corpus.chunks(samples, 8, 512 * 1024**2))
        self.assertEqual([len(g) for g in groups], [1] * 5)
        self.assertTrue(all(sum(s["bytes"] for s in g) <= 512 * 1024**2 for g in groups))

    def test_partial_corrupt_download_never_becomes_cached(self):
        root = corpus.cache_root(self.root / "cache")
        created = []
        def corrupt(s, path):
            path.write_bytes(b"wrong")
        with self.assertRaises(corpus.CorpusError):
            corpus.obtain(root, self.s, corrupt, created)
        self.assertEqual(created, [])
        self.assertEqual(list(root.iterdir()), [root / corpus.MARKER])

    def test_cleanup_preserves_preexisting_files_and_reports(self):
        root = corpus.cache_root(self.root / "cache")
        existing = sample(b"existing", "existing")
        existing_path = corpus.cached_path(root, existing)
        existing_path.write_bytes(b"existing")
        report = self.root / "report.json"
        report.write_text("evidence")
        created = []
        corpus.obtain(root, existing, lambda *_: self.fail("pre-existing source must be reused"), created)
        downloaded = corpus.obtain(root, self.s, lambda s, p: p.write_bytes(b"authentic-test-bytes"), created)
        corpus.cleanup(created, root)
        self.assertFalse(downloaded.exists())
        self.assertEqual(existing_path.read_bytes(), b"existing")
        self.assertEqual(report.read_text(), "evidence")

    def test_cleanup_retains_changed_source(self):
        root = corpus.cache_root(self.root / "cache")
        created = []
        downloaded = corpus.obtain(root, self.s, lambda s, p: p.write_bytes(b"authentic-test-bytes"), created)
        downloaded.write_bytes(b"changed during processing")
        corpus.cleanup(created, root)
        self.assertTrue(downloaded.exists())

    def test_cache_refuses_foreign_directory_and_symlink(self):
        foreign = self.root / "originals"
        foreign.mkdir()
        (foreign / "owner.NEF").write_bytes(b"sacred")
        link = self.root / "link"
        link.symlink_to(foreign, target_is_directory=True)
        for path in (foreign, link):
            with self.assertRaises(corpus.CorpusError):
                corpus.cache_root(path)
        self.assertEqual((foreign / "owner.NEF").read_bytes(), b"sacred")

    def test_frozen_evidence_and_source_mutation_fail(self):
        good = result(self.s)
        self.assertEqual(corpus.compare(self.s, good, True), ("qualified", []))
        for key in ("mosaic_sha256", "source_sha256_after"):
            bad = copy.deepcopy(good)
            bad[key] = "f" * 64
            self.assertTrue(corpus.compare(self.s, bad, True)[1])
        bad = copy.deepcopy(good)
        bad["as_shot"]["sha256"] = "f" * 64
        self.assertTrue(corpus.compare(self.s, bad, True)[1])
        self.assertFalse(corpus.compare(self.s, bad, False)[1])

    def test_candidates_never_count_as_qualified(self):
        candidate = dict(self.s, expectation={"kind": "candidate"})
        self.assertEqual(corpus.compare(candidate, result(candidate), False)[0], "unqualified-decoded")
        refused = dict(result(candidate), status="failed", error="unsupported camera")
        self.assertEqual(corpus.compare(candidate, refused, False)[0], "unqualified-refused")

    def test_portable_checks_detect_geometry_colour_and_range_changes(self):
        s = copy.deepcopy(self.s)
        s["expectation"]["metadata"] = {"sensor_width": 2, "cam_xyz": [[0.7, -0.2, 0.1]]}
        s["expectation"]["as_shot"].update(min=0.0, max=1.0)
        good = result(s)
        good["metadata"].update(sensor_width=2, cam_xyz=[[0.7, -0.2, 0.1]])
        good["as_shot"].update(min=0.0, max=1.0)
        self.assertFalse(corpus.compare(s, good, False)[1])
        for field, value in (("sensor_width", 3), ("cam_xyz", [[0.9, -0.2, 0.1]])):
            bad = copy.deepcopy(good)
            bad["metadata"][field] = value
            self.assertTrue(corpus.compare(s, bad, False)[1])
        bad = copy.deepcopy(good)
        bad["as_shot"]["max"] = 2.0
        self.assertTrue(corpus.compare(s, bad, False)[1])

    def test_clean_removes_selected_tool_files_only(self):
        manifest = self.root / "manifest.json"
        manifest.write_text(json.dumps(self.data))
        root = corpus.cache_root(self.root / "cache")
        selected = corpus.cached_path(root, self.s)
        selected.write_bytes(b"authentic-test-bytes")
        other = root / "keep.txt"
        other.write_text("unrelated data")
        with redirect_stdout(io.StringIO()):
            code = corpus.main(["clean", "--manifest", str(manifest), "--cache", str(root)])
        self.assertEqual(code, 0)
        self.assertFalse(selected.exists())
        self.assertEqual(other.read_text(), "unrelated data")

    def test_credentials_select_read_only_without_shell_expansion(self):
        path = self.root / ".env"
        path.write_text("R2_BUCKET=luxforge-samples\nR2_ACCOUNT_ID=" + "a" * 32 +
                        "\nR2_ACCESS_KEY_ID=writer\nR2_SECRET_ACCESS_KEY=write-secret\n"
                        "R2_READ_ACCESS_KEY_ID=reader\nR2_READ_SECRET_ACCESS_KEY=$(literal)\n")
        with patch.dict("os.environ", {}, clear=True):
            self.assertEqual(corpus.credentials(path)["key"], "reader")
            self.assertEqual(corpus.credentials(path)["secret"], "$(literal)")
            self.assertEqual(corpus.credentials(path, write=True)["key"], "writer")

    def test_failed_qualifier_still_reports_and_cleans_its_download(self):
        manifest = self.root / "manifest.json"
        manifest.write_text(json.dumps(self.data))
        binary = self.root / "qualifier"
        binary.write_text("#!/usr/bin/env python3\nimport sys\nsys.exit(9)\n")
        binary.chmod(0o700)
        cache = self.root / "cache"
        output = self.root / "evidence"
        class FakeR2:
            def __init__(self, _):
                pass
            def download(self, sample, path):
                path.write_bytes(b"authentic-test-bytes")
        with patch.object(corpus, "credentials", return_value={}), patch.object(corpus, "R2", FakeR2), redirect_stdout(io.StringIO()):
            code = corpus.main(["test", "--manifest", str(manifest), "--cache", str(cache),
                                "--output", str(output), "--qualifier", str(binary), "--cleanup"])
        self.assertEqual(code, 1)
        self.assertTrue((output / "summary.json").exists())
        self.assertFalse(corpus.cached_path(cache, self.s).exists())


if __name__ == "__main__":
    unittest.main()
