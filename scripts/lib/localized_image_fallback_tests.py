#!/usr/bin/env python3
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "localized_image_fallback",
    Path(__file__).with_name("localized-image-fallback.py"),
)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def face(url: str) -> dict[str, str]:
    return {size: f"{url}-{size}" for size in MODULE.IMAGE_SIZES}


def card(card_id: str, language: str, oracle: str, name: str, **fields: object) -> dict[str, object]:
    value: dict[str, object] = {
        "object": "card",
        "id": card_id,
        "lang": language,
        "oracle_id": oracle,
        "layout": "normal",
        "name": name,
        "image_status": "highres_scan",
    }
    value.update(fields)
    return value


class LocalizedImageFallbackTests(unittest.TestCase):
    def test_exact_wins_and_matching_candidates_preserve_faces(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output"
            output.mkdir()
            exact = {
                "en-exact": {"id": "ja-exact", "faces": [face("exact")]},
            }
            (output / "scryfall-images.v2.ja.json").write_text(json.dumps(exact), encoding="utf-8")
            (output / "scryfall-images.v2.de.json").write_text("{}", encoding="utf-8")
            cards = [
                card("en-exact", "en", "forest", "Forest"),
                card("en-missing", "en", "forest", "Forest"),
                card("ja-forest", "ja", "forest", "Forest", games=["paper"], image_uris=face("forest")),
                card(
                    "en-dfc", "en", "dfc", "Front", layout="transform",
                    card_faces=[{"name": "Front"}, {"name": "Back"}],
                ),
                card(
                    "ja-dfc", "ja", "dfc", "Front", layout="transform",
                    card_faces=[{"name": "Front", "image_uris": face("front")},
                                {"name": "Back", "image_uris": face("back")}],
                ),
                card(
                    "en-root", "en", "root", "Spell", layout="adventure",
                    card_faces=[{"name": "Spell"}, {"name": "Creature"}],
                ),
                card(
                    "ja-root", "ja", "root", "Spell", layout="adventure",
                    card_faces=[{"name": "Spell"}, {"name": "Creature"}],
                    image_uris=face("root"),
                ),
                card(
                    "en-missing-art", "en", "missing-art", "Missing",
                ),
                card(
                    "ja-missing-art", "ja", "missing-art", "Missing",
                    image_status="missing", image_uris=face(MODULE.PLACEHOLDER_URL),
                ),
                card("en-placeholder", "en", "placeholder", "Placeholder"),
                card("ja-placeholder", "ja", "placeholder", "Placeholder",
                     image_status="placeholder", image_uris=face("not-soon.jpg")),
            ]
            bulk = root / "all-cards.json"
            bulk.write_text(json.dumps(cards), encoding="utf-8")

            self.assertEqual(MODULE.augment_maps(bulk, output), {"de": 0, "ja": 3})
            result = json.loads((output / "scryfall-images.v2.ja.json").read_text(encoding="utf-8"))
            self.assertEqual(result["en-exact"], exact["en-exact"])
            self.assertEqual(result["en-missing"]["id"], "ja-forest")
            self.assertEqual(result["en-dfc"]["faces"][1]["normal"], "back-normal")
            self.assertEqual(result["en-root"]["faces"], [face("root")])
            self.assertNotIn("en-missing-art", result)
            self.assertNotIn("en-placeholder", result)
            self.assertNotIn("en-missing", json.loads((output / "scryfall-images.v2.de.json").read_text()))

    def test_reversible_cards_match_ordered_face_oracle_ids(self) -> None:
        faces = [{"name": "Front", "oracle_id": "front", "image_uris": face("front")},
                 {"name": "Back", "oracle_id": "back", "image_uris": face("back")}]
        english = card("en-reversible", "en", "unused", "Front // Back",
                       layout="reversible_card", card_faces=faces)
        del english["oracle_id"]
        japanese = dict(english, id="ja-reversible", lang="ja")
        wrong_back = dict(japanese, id="ja-wrong", released_at="2099-01-01",
                          card_faces=[faces[0], dict(faces[1], oracle_id="other")])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            map_path = root / "scryfall-images.v2.ja.json"
            map_path.write_text("{}", encoding="utf-8")
            bulk = root / "all-cards.json"
            bulk.write_text(json.dumps([english, japanese, wrong_back]), encoding="utf-8")
            self.assertEqual(MODULE.augment_maps(bulk, root), {"ja": 1})
            result = json.loads(map_path.read_text(encoding="utf-8"))["en-reversible"]
            self.assertEqual(result["id"], "ja-reversible")
            self.assertEqual(result["faces"], [face("front"), face("back")])
        self.assertIsNone(MODULE.card_identity(dict(english, card_faces=[])))
        self.assertIsNone(MODULE.card_identity(dict(english, card_faces=[{"name": "Front"}])))

    def test_stream_boundaries_and_incomplete_input_do_not_publish(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            valid = root / "valid.json"
            valid.write_text('[{"object":"card"}, {"object":"card"}]', encoding="utf-8")
            self.assertEqual(list(MODULE.stream_json_array(valid, chunk_size=2)), [{"object": "card"}, {"object": "card"}])
            empty = root / "empty.json"
            empty.write_text("[ ]", encoding="utf-8")
            self.assertEqual(list(MODULE.stream_json_array(empty, chunk_size=1)), [])
            malformed = root / "malformed.json"
            malformed.write_text('[{"object":"card"}', encoding="utf-8")
            with self.assertRaises(MODULE.JsonArrayError):
                list(MODULE.stream_json_array(malformed, chunk_size=2))

            output = root / "output"
            output.mkdir()
            map_path = output / "scryfall-images.v2.ja.json"
            map_path.write_text("{}", encoding="utf-8")
            with self.assertRaises(MODULE.JsonArrayError):
                MODULE.augment_maps(malformed, output)
            self.assertEqual(map_path.read_text(encoding="utf-8"), "{}")
            self.assertFalse((output / "scryfall-images.v2.fallback.json").exists())


if __name__ == "__main__":
    unittest.main()
