#!/usr/bin/env python3

import json
import tempfile
import unittest
from pathlib import Path

import train_eval_weights


class LoadSelfplayCorpusTests(unittest.TestCase):
    def test_rejects_entire_shard_when_a_late_metadata_line_is_incompatible(self) -> None:
        row = {
            "features": {
                name: 1.0
                for name in (
                    train_eval_weights.SELFPLAY_FEATURE_NAMES
                    + train_eval_weights.SELFPLAY_CONTROLS
                )
            },
            "turn": 1,
            "won": True,
            "seed": 17,
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            accepted = root / "accepted.jsonl"
            accepted.write_text(
                "\n".join((json.dumps({"meta": {"schema": 3}}), json.dumps(row))) + "\n"
            )
            rejected = root / "rejected.jsonl"
            rejected.write_text(
                "\n".join(
                    (
                        json.dumps({"meta": {"schema": 3}}),
                        json.dumps(row),
                        json.dumps({"meta": {"schema": 2}}),
                    )
                )
                + "\n"
            )

            features, labels, meta, files, seeds = train_eval_weights.load_selfplay_corpus(
                str(root / "*.jsonl")
            )

        self.assertEqual(features["early"].shape, (1, 11))
        self.assertEqual(labels["early"].tolist(), [1])
        self.assertEqual(meta, {"schema": 3})
        self.assertEqual(files, [str(accepted)])
        self.assertEqual(seeds, {17})


if __name__ == "__main__":
    unittest.main()
