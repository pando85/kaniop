"""Regression tests for the docs navigation validator."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "check-docs-nav.py"
spec = importlib.util.spec_from_file_location("check_docs_nav", SCRIPT)
assert spec is not None and spec.loader is not None
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class DocumentationNavigationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workspace = tempfile.TemporaryDirectory()
        self.addCleanup(self.workspace.cleanup)
        self.source = Path(self.workspace.name)
        self.write(
            "SUMMARY.md",
            "# Summary\n\n[Introduction](index.md)\n- [Usage](usage.md)\n"
            "  - [Groups](usage/group.md)\n",
        )
        self.write("index.md", "---\ntitle: Introduction\nweight: 0\n---\n# Introduction\n")
        self.write("usage.md", "---\ntitle: Usage\nweight: 100\n---\n# Usage\n")
        self.write(
            "usage/group.md",
            "---\ntitle: Groups\nweight: 110\nindent: true\n---\n# Groups\n",
        )

    def write(self, name: str, content: str) -> None:
        path = self.source / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def check(self) -> str:
        return "\n".join(checker.validate(self.source))

    def test_current_navigation_is_valid(self) -> None:
        self.assertEqual(self.check(), "")

    def test_display_title_may_differ_from_summary_label(self) -> None:
        self.write(
            "usage/group.md",
            "---\ntitle: Managing Groups\nweight: 110\nindent: true\n---\n",
        )
        self.assertEqual(self.check(), "")

    def test_missing_title(self) -> None:
        self.write("usage.md", "---\nweight: 100\n---\n# Usage\n")
        self.assertIn("missing or empty frontmatter title", self.check())

    def test_missing_frontmatter(self) -> None:
        self.write("usage.md", "# Usage\n")
        self.assertIn("missing YAML frontmatter", self.check())

    def test_invalid_weight(self) -> None:
        self.write("usage.md", "---\ntitle: Usage\nweight: banana\n---\n")
        self.assertIn("weight must be a non-negative integer", self.check())

    def test_duplicate_or_out_of_order_weight(self) -> None:
        self.write("usage.md", "---\ntitle: Usage\nweight: 110\n---\n")
        self.assertIn("must be greater than", self.check())

    def test_mismatched_indentation(self) -> None:
        self.write("usage/group.md", "---\ntitle: Groups\nweight: 110\n---\n")
        self.assertIn("indent must be true", self.check())

    def test_missing_link_target(self) -> None:
        self.write("SUMMARY.md", "[Introduction](index.md)\n- [Missing](not-found.md)\n")
        self.assertIn("missing page 'not-found.md'", self.check())

    def test_unlisted_page(self) -> None:
        self.write("new.md", "---\ntitle: New page\nweight: 120\n---\n")
        self.assertIn("new.md: documentation page missing from SUMMARY.md", self.check())

    def test_duplicate_summary_link(self) -> None:
        self.write(
            "SUMMARY.md",
            "[Introduction](index.md)\n- [Usage](usage.md)\n"
            "- [Usage again](usage.md)\n  - [Groups](usage/group.md)\n",
        )
        self.assertIn("duplicate page 'usage.md'", self.check())

    def test_nested_pages_are_flattened_for_jekyll(self) -> None:
        self.write(
            "SUMMARY.md",
            "[Introduction](index.md)\n- [Usage](usage.md)\n"
            "  - [Groups](usage/group.md)\n    - [Detail](usage/detail.md)\n",
        )
        self.write(
            "usage/detail.md",
            "---\ntitle: Detail\nweight: 120\nindent: true\n---\n",
        )
        self.assertEqual(self.check(), "")


if __name__ == "__main__":
    unittest.main()
