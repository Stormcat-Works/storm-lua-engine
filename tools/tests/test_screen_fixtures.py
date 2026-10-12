"""画面契約の破損・誤更新を拒否する検査自体のテストです。"""
import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("screen_contract", ROOT / "tools/check_screen_fixtures.py")
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)

class ScreenContractTests(unittest.TestCase):
    def setUp(self):
        self.corpus = {"format": 1, "contract": checker.CONTRACT, "cases": [{
            "id": "one-pixel", "width": 1, "height": 1,
            "ops": [{"type": "rectF", "x": 0, "y": 0, "w": 1, "h": 1}],
            "expectedRgbaRle": [[1, 255, 255, 255, 255]],
        }]}
    def test_all_accepted_cases_and_font_hash(self):
        self.assertEqual(checker.verify(), 743)
    def test_complete_rgba_is_valid(self):
        self.assertEqual(checker.validate_corpus(self.corpus), 1)
    def test_duplicate_case_is_not_a_second_test(self):
        self.corpus["cases"] *= 2
        with self.assertRaises(ValueError): checker.validate_corpus(self.corpus)
    def test_missing_or_partial_expected_frame_rejects(self):
        for runs in ([], [[2, 0, 0, 0, 0]], [[0, 0, 0, 0, 0]], [[1, 256, 0, 0, 0]], [[True, 0, 0, 0, 0]]):
            case = copy.deepcopy(self.corpus)
            case["cases"][0]["expectedRgbaRle"] = runs
            with self.assertRaises(ValueError): checker.validate_corpus(case)
    def test_nonfinite_or_boolean_coordinate_rejects(self):
        for value in (float("nan"), float("inf"), True, "1"):
            self.corpus["cases"][0]["ops"][0]["x"] = value
            with self.assertRaises(ValueError): checker.validate_corpus(self.corpus)
    def test_unknown_command_and_extra_field_rejects(self):
        for op in ({"type": "unknown"}, {"type": "clear", "typo": 1}):
            self.corpus["cases"][0]["ops"] = [op]
            with self.assertRaises(ValueError): checker.validate_corpus(self.corpus)
    def test_version_and_empty_corpus_reject(self):
        self.corpus["format"] = 2
        with self.assertRaises(ValueError): checker.validate_corpus(self.corpus)
        self.corpus["format"] = 1
        self.corpus["cases"] = []
        with self.assertRaises(ValueError): checker.validate_corpus(self.corpus)
    def test_content_change_without_manifest_review_rejects(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in (checker.CORPUS, checker.FONT, "fixtures/screen/manifest.json"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes((ROOT / name).read_bytes())
            path = root / checker.CORPUS
            data = json.loads(path.read_text())
            data["cases"][0]["id"] += "-modified"
            path.write_text(json.dumps(data))
            with self.assertRaises(ValueError): checker.verify(root)

if __name__ == "__main__": unittest.main()
