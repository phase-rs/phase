#!/usr/bin/env python3
"""Tests for the integration card fixture shard format and its tools.

The committed-tree test needs no card-data export: the committed shards must
equal the writer's output for the entries they hold (same files, same bytes,
sorted, one record per line, each record in its first-letter shard).
"""
import gzip
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fixture_shards  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURE_DIR = REPO_ROOT / "crates/engine/tests/fixtures/integration_cards"
OLD_GZ = "crates/engine/tests/fixtures/integration_cards.json.gz"


class ShardFormatTests(unittest.TestCase):
    def test_shard_name_by_first_character(self) -> None:
        self.assertEqual(fixture_shards.shard_name("abandon hope"), "a.jsonl")
        self.assertEqual(fixture_shards.shard_name("zurgo"), "z.jsonl")
        self.assertEqual(fixture_shards.shard_name("1996 world champion"), "_digits.jsonl")
        self.assertEqual(fixture_shards.shard_name("_____ goblin"), "_digits.jsonl")
        self.assertEqual(fixture_shards.shard_name("éomer"), "_other.jsonl")

    def test_render_is_sorted_compact_one_record_per_line(self) -> None:
        shards = fixture_shards.render_shards({"b": {"n": [1, 2]}, "a": {"n": "é"}, "ab": None})
        self.assertEqual(sorted(shards), ["a.jsonl", "b.jsonl"])
        self.assertEqual(
            shards["a.jsonl"].decode(),
            '{"key":"a","card":{"n":"é"}}\n{"key":"ab","card":null}\n',
        )
        self.assertEqual(shards["b.jsonl"].decode(), '{"key":"b","card":{"n":[1,2]}}\n')

    def test_round_trip_and_duplicate_keys(self) -> None:
        entries = {"x": {"k": 1}, "y": {"k": 2}, "é": {"k": 3}}
        self.assertEqual(
            fixture_shards.parse_shards(fixture_shards.render_shards(entries)),
            dict(sorted(entries.items())),
        )
        record = b'{"key":"a","card":1}\n'
        with self.assertRaises(ValueError):
            fixture_shards.parse_shards({"a.jsonl": record, "b.jsonl": record})

    def test_read_shards_ignores_non_shard_files(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            directory = Path(raw)
            (directory / "a.jsonl").write_bytes(b"x")
            (directory / ".DS_Store").write_bytes(b"y")
            self.assertEqual(sorted(fixture_shards.read_shards(directory)), ["a.jsonl"])


class CommittedShardTests(unittest.TestCase):
    @unittest.skipUnless(FIXTURE_DIR.is_dir(), "fixture directory is not in this tree")
    def test_committed_shards_are_the_writer_output(self) -> None:
        stray = sorted(
            path.name
            for path in FIXTURE_DIR.iterdir()
            if not path.name.endswith(fixture_shards.SHARD_SUFFIX)
        )
        self.assertEqual(stray, [], "only *.jsonl shards belong in the fixture directory")
        committed = fixture_shards.read_shards(FIXTURE_DIR)
        self.assertGreater(len(committed), 20)
        entries = fixture_shards.parse_shards(committed)
        self.assertGreater(len(entries), 1000)
        rendered = fixture_shards.render_shards(entries)
        self.assertEqual(sorted(rendered), sorted(committed), "shard file set")
        for name, data in rendered.items():
            self.assertEqual(committed[name], data, f"{name} is not canonical writer output")


class ReapplyTests(unittest.TestCase):
    """End to end in a throwaway repository: the script only reads git and the shards."""

    def git(self, root: Path, *args: str) -> str:
        return subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@example.com", *args],
            cwd=root, check=True, stdout=subprocess.PIPE, text=True,
        ).stdout.strip()

    def commit_gz(self, root: Path, entries: dict[str, object]) -> str:
        path = root / OLD_GZ
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(gzip.compress(json.dumps(entries).encode()))
        self.git(root, "add", "-A")
        self.git(root, "commit", "-q", "-m", "gz")
        return self.git(root, "rev-parse", "HEAD")

    def run_script(self, root: Path, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [sys.executable, str(root / "scripts/reapply-fixture-entries.py"), *args],
            cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
        )

    def test_applies_noops_and_conflicts(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "scripts").mkdir()
            shutil.copytree(REPO_ROOT / "scripts/lib", root / "scripts/lib", ignore=shutil.ignore_patterns("__pycache__"))
            shutil.copy(REPO_ROOT / "scripts/reapply-fixture-entries.py", root / "scripts")
            self.git(root, "init", "-q", "-b", "main")
            base = {"keep": {"v": 1}, "edit": {"v": 1}, "drop": {"v": 1}, "both": {"v": 1}, "clash": {"v": 1}}
            fork = self.commit_gz(root, base)
            theirs = {"keep": {"v": 1}, "edit": {"v": 2}, "both": {"v": 2}, "clash": {"v": 2}, "new": {"v": 9}}
            self.git(root, "checkout", "-q", "-b", "pr")
            pr = self.commit_gz(root, theirs)
            self.git(root, "checkout", "-q", "main")
            # main sharded the fixture: it holds `both` already at the branch's value and
            # changed `clash` its own way
            main_now = {"keep": {"v": 1}, "edit": {"v": 1}, "drop": {"v": 1}, "both": {"v": 2}, "clash": {"v": 3}}
            (root / OLD_GZ).unlink()
            fixture_shards.write_shards(
                root / "crates/engine/tests/fixtures/integration_cards",
                fixture_shards.render_shards(main_now),
            )
            conflicted = self.run_script(root, pr, "--base", fork, "--write")
            self.assertEqual(conflicted.returncode, 1, conflicted.stdout)
            self.assertIn("CONFLICT 'clash'", conflicted.stdout)
            shards = fixture_shards.parse_shards(
                fixture_shards.read_shards(root / "crates/engine/tests/fixtures/integration_cards")
            )
            self.assertEqual(shards, dict(sorted(main_now.items())), "nothing written on a conflict")

            # Without the clash: the branch's other edits apply.
            theirs.pop("clash")
            base_no_clash = dict(base)
            base_no_clash.pop("clash")
            main_now.pop("clash")
            fixture_shards.write_shards(
                root / "crates/engine/tests/fixtures/integration_cards",
                fixture_shards.render_shards(main_now),
            )
            self.git(root, "checkout", "-q", "pr")
            fork2 = self.commit_gz(root, base_no_clash)
            pr2 = self.commit_gz(root, theirs)
            result = self.run_script(root, pr2, "--base", fork2, "--write")
            self.assertEqual(result.returncode, 0, result.stdout)
            shards = fixture_shards.parse_shards(
                fixture_shards.read_shards(root / "crates/engine/tests/fixtures/integration_cards")
            )
            self.assertEqual(
                shards,
                {"both": {"v": 2}, "edit": {"v": 2}, "keep": {"v": 1}, "new": {"v": 9}},
            )


if __name__ == "__main__":
    unittest.main()
