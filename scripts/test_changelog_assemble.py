#!/usr/bin/env python3
"""Tests for changelog_assemble.py. Run: python3 -m unittest scripts/test_changelog_assemble.py"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import changelog_assemble as ca  # noqa: E402

CHANGELOG = """# Changelog

## Unreleased

- Fixed: hand-written fix
  with a second line.
- Added: hand-written add
"""


class Assemble(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        ca.CHANGELOG = root / "CHANGELOG.md"
        ca.FRAGMENT_DIR = root / "changelog.d"
        ca.FRAGMENT_DIR.mkdir()
        ca.CHANGELOG.write_text(CHANGELOG)

    def tearDown(self):
        self.tmp.cleanup()

    def frag(self, name, body):
        (ca.FRAGMENT_DIR / name).write_text(body)

    def test_merges_orders_and_deletes(self):
        self.frag("2-b.fixed.md", "fix b\nmore b\n")
        self.frag("1-a.fixed.md", "fix a\n")
        self.frag("3-s.security.md", "sec\n")
        self.frag("README.md", "ignored")
        self.assertEqual(ca.assemble("1.0.0", "2026-02-02"), 0)
        text = ca.CHANGELOG.read_text()
        self.assertIn("## Unreleased\n\n## [1.0.0] - 2026-02-02\n", text)
        body = text.split("## [1.0.0] - 2026-02-02\n", 1)[1]
        self.assertLess(body.index("hand-written add"), body.index("- Security: sec"))
        self.assertLess(body.index("- Security: sec"), body.index("- Fixed: fix a"))
        self.assertLess(body.index("- Fixed: fix a"), body.index("- Fixed: fix b\n  more b"))
        self.assertEqual([p.name for p in ca.fragment_files(ca.FRAGMENT_DIR)], [])
        self.assertTrue((ca.FRAGMENT_DIR / "README.md").exists())

    def test_check_rejects_bad_fragments(self):
        self.assertEqual(ca.check(), 0)
        self.frag("x.bogus.md", "y")
        self.frag("e.added.md", "  \n")
        self.frag("noext.md", "y")
        self.frag("h.fixed.md", "y\n## Oops")
        self.frag("b.fixed.md", "- y")
        self.frag("l.fixed.md", "Fixed: y")
        self.frag("ok.fixed.md", "y\n  ## indented is fine")
        _, errors = ca.load_fragments(ca.FRAGMENT_DIR)
        self.assertEqual(len(errors), 6)
        self.assertEqual(ca.check(), 1)

    def test_refuses_empty_release(self):
        ca.CHANGELOG.write_text("# Changelog\n\n## Unreleased\n")
        self.assertEqual(ca.assemble("1.0.0", "2026-02-02"), 1)


if __name__ == "__main__":
    unittest.main()
