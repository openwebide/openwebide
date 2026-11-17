"""Portable defaults must retain their bytes and source identity in transit."""

import json
from pathlib import Path
import shutil
import tempfile
import unittest

from compiled_plugins import provenance, transfer


class CompiledPluginsTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.bundle = self.root / "bundled-plugins.json"
        self.bundle.write_bytes(b'{"artifacts": ["portable component bytes"]}')
        self.commit = "a" * 40
        transfer(self.bundle, self.commit, seal=True)

    def test_verified_bundle_can_move_between_hosts(self):
        destination = self.root / "other-host"
        destination.mkdir()
        restored = destination / self.bundle.name
        shutil.copy2(self.bundle, restored)
        shutil.copy2(provenance(self.bundle), provenance(restored))
        transfer(restored, self.commit)

    def test_changed_bundle_is_rejected(self):
        self.bundle.write_bytes(b'{"artifacts": ["changed bytes"]}')
        with self.assertRaisesRegex(ValueError, "checksum"):
            transfer(self.bundle, self.commit)

    def test_another_source_commit_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "commit"):
            transfer(self.bundle, "b" * 40)

    def test_missing_provenance_is_rejected(self):
        provenance(self.bundle).unlink()
        with self.assertRaises(FileNotFoundError):
            transfer(self.bundle, self.commit)

    def test_incomplete_provenance_is_rejected(self):
        provenance(self.bundle).write_text(json.dumps({"commit": self.commit}))
        with self.assertRaisesRegex(ValueError, "checksum"):
            transfer(self.bundle, self.commit)

    def test_source_identity_cannot_be_omitted(self):
        with self.assertRaisesRegex(ValueError, "full source commit"):
            transfer(self.bundle, "main", seal=True)


if __name__ == "__main__":
    unittest.main()
