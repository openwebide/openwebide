"""Navigation must keep every guide discoverable, including nested sections."""

import tempfile
import unittest
from pathlib import Path

from navigation import validate_navigation
from build import rewrite_readme_links


class NavigationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.content = Path(self.temporary.name)
        (self.content / "docs").mkdir()
        (self.content / "index.md").touch()
        (self.content / "docs/guide.md").touch()
        self.navigation = [{"Home": "index.md"}, {"Guides": [{"Guide": "docs/guide.md"}]}]

    def test_nested_sections_cover_all_pages(self):
        self.assertEqual(validate_navigation(self.navigation, self.content), self.navigation)

    def test_new_page_requires_placement(self):
        (self.content / "docs/new.md").touch()
        with self.assertRaisesRegex(ValueError, r"unlisted=\['docs/new.md'\]"):
            validate_navigation(self.navigation, self.content)

    def test_removed_or_misspelled_page_is_rejected(self):
        (self.content / "docs/guide.md").unlink()
        with self.assertRaisesRegex(ValueError, r"missing files=\['docs/guide.md'\]"):
            validate_navigation(self.navigation, self.content)

    def test_duplicate_page_is_rejected_across_sections(self):
        self.navigation.append({"Reference": [{"Also guide": "docs/guide.md"}]})
        with self.assertRaisesRegex(ValueError, r"duplicates=\['docs/guide.md'\]"):
            validate_navigation(self.navigation, self.content)

    def test_malformed_sections_are_rejected(self):
        for navigation in ([], [{"Empty": []}], [{"Bad": None}], [{"": "index.md"}]):
            with self.subTest(navigation=navigation), self.assertRaises(ValueError):
                validate_navigation(navigation, self.content)


class ReadmeLinksTests(unittest.TestCase):
    def test_root_readme_navigation_preserves_sdk_and_external_source_links(self):
        text = "[Start](README.md#run) [Up](../README.md) [SDK](../crates/plugin-sdk/README.md) [Source](https://github.com/openwebide/openwebide/blob/main/crates/plugin-sdk/README.md)"
        rendered = rewrite_readme_links(text)
        self.assertIn("[Start](getting-started.md#run)", rendered)
        self.assertIn("[Up](../getting-started.md)", rendered)
        self.assertIn("[SDK](../crates/plugin-sdk/README.md)", rendered)
        self.assertIn("[Source](https://github.com/openwebide/openwebide/blob/main/crates/plugin-sdk/README.md)", rendered)


if __name__ == "__main__":
    unittest.main()
