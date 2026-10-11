"""Catch accidental Windows-1252 decoding of UTF-8 source before release."""
from pathlib import Path
import unittest


class TextEncodingTests(unittest.TestCase):
    def test_rust_source_has_no_mojibake(self):
        root = Path(__file__).resolve().parents[2]
        for path in (root / "src").glob("*.rs"):
            source = path.read_text(encoding="utf-8")
            for damaged in ("\u00c2\u00b7", "\u00e2\u20ac", "\u00e2\u201d", "\ufffd"):
                self.assertNotIn(damaged, source, str(path))


if __name__ == "__main__":
    unittest.main()
