"""WASI SDK extraction works with Debian 12's older tarfile API."""
import importlib.util
import io
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('install_wasi_sdk', Path(__file__).with_name('install_wasi_sdk.py'))
INSTALLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INSTALLER)
NATIVE_EXTRACTION_FILTER = INSTALLER.HAS_EXTRACTION_FILTER


class LegacyArchive:
    def __init__(self, stream):
        self.stream = stream

    def __iter__(self):
        return iter(self.stream)

    def extract(self, member, path):
        # Exercise the installer's checks without the modern default filter.
        if NATIVE_EXTRACTION_FILTER:
            self.stream.extract(member, path, filter='fully_trusted')
        else:
            self.stream.extract(member, path)

    def extractall(self, path):
        raise AssertionError('Legacy extraction must check individual members')


class WasiArchiveTests(unittest.TestCase):
    def extract(self, entries, destination, legacy=True):
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode='w') as archive:
            for name, kind, content in entries:
                entry = tarfile.TarInfo(name)
                entry.type = kind
                entry.mode = 0o7777
                if kind == tarfile.REGTYPE:
                    content = content.encode()
                    entry.size = len(content)
                    archive.addfile(entry, io.BytesIO(content))
                else:
                    entry.linkname = content
                    archive.addfile(entry)
        data.seek(0)
        with tarfile.open(fileobj=data) as archive:
            with patch.object(INSTALLER, 'HAS_EXTRACTION_FILTER', not legacy):
                INSTALLER.extract_sdk_archive(LegacyArchive(archive) if legacy else archive, destination, 'sdk')

    def test_legacy_regular_files_symlinks_forward_hardlinks_and_permissions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.extract([
                ('sdk/bin/cc', tarfile.LNKTYPE, 'sdk/bin/clang'),
                ('sdk/bin/clang', tarfile.REGTYPE, 'compiler'),
                ('sdk/bin/clang++', tarfile.SYMTYPE, 'clang'),
            ], root)
            self.assertEqual((root / 'sdk/bin/cc').read_text(), 'compiler')
            self.assertEqual((root / 'sdk/bin/clang++').read_text(), 'compiler')
            self.assertEqual(os.stat(root / 'sdk/bin/clang').st_mode & 0o7777, 0o755)

    def test_legacy_rejects_traversal_absolute_paths_links_devices_and_cycles(self):
        for entry in [
            ('../escape', tarfile.REGTYPE, 'bad'),
            ('/tmp/escape', tarfile.REGTYPE, 'bad'),
            ('sdk/link', tarfile.SYMTYPE, '../../escape'),
            ('sdk/link', tarfile.LNKTYPE, '../escape'),
            ('sdk/device', tarfile.CHRTYPE, ''),
            ('sdk/missing', tarfile.LNKTYPE, 'sdk/missing'),
        ]:
            with self.subTest(entry=entry), tempfile.TemporaryDirectory() as directory:
                with self.assertRaises(ValueError):
                    self.extract([entry], Path(directory))

    def test_legacy_checks_targets_through_previously_extracted_symlinks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'sdk').mkdir()
            (root / 'sdk/redirect').symlink_to(root.parent)
            with self.assertRaises(ValueError):
                self.extract([('sdk/redirect/escape', tarfile.REGTYPE, 'bad')], root)

    @unittest.skipUnless(NATIVE_EXTRACTION_FILTER, 'This Python predates extraction filters')
    def test_modern_filtered_extraction(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.extract([('sdk/bin/clang', tarfile.REGTYPE, 'compiler')], root, legacy=False)
            self.assertEqual((root / 'sdk/bin/clang').read_text(), 'compiler')

    def test_legacy_replaced_symlinks_keep_their_original_parent(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(ValueError):
                self.extract([
                    ('sdk/deep/sub/clang', tarfile.REGTYPE, 'compiler'),
                    ('sdk/bin/link', tarfile.SYMTYPE, '../deep/sub/clang'),
                    ('sdk/bin/link', tarfile.SYMTYPE, '../../escape'),
                ], Path(directory))


if __name__ == '__main__':
    unittest.main()
