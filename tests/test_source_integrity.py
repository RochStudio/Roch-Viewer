"""Small structural checks for mistakes Python otherwise accepts silently."""

import ast
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]


class LiteralDictionaryKeyTest(unittest.TestCase):
    def test_source_dictionaries_do_not_repeat_literal_keys(self):
        duplicates = []
        for path in (ROOT / "rochviewer").rglob("*.py"):
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            for node in ast.walk(tree):
                if not isinstance(node, ast.Dict):
                    continue
                seen = {}
                for key in node.keys:
                    if not isinstance(key, ast.Constant):
                        continue
                    value = key.value
                    if not isinstance(value, (str, int, float, bytes)):
                        continue
                    if value in seen:
                        duplicates.append(
                            "%s:%d repeats %r (first at line %d)" % (
                                path.relative_to(ROOT), key.lineno, value,
                                seen[value],
                            )
                        )
                    else:
                        seen[value] = key.lineno
        self.assertEqual(duplicates, [])


if __name__ == "__main__":
    unittest.main()
