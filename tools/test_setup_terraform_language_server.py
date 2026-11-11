"""Installer boundaries; real vendor binaries are checked separately by the probe."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

SPEC = importlib.util.spec_from_file_location('terraform_setup', Path(__file__).with_name('setup-terraform-language-server.py'))
SETUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SETUP)


class Distributions(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='terraform-installer-contract-')
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.cache = self.root / 'cache'
        self.cache.mkdir()
        self.destination = self.root / 'installed'
        self.pins = {}
        for name in ['terraform', 'terraform-ls']:
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w') as archive:
                archive.writestr(name, b'fixture binary')
                archive.writestr('LICENSE.txt', b'upstream license')
            self.archive(name, stream.getvalue())
        self.save()
        self.addCleanup(patch.stopall)
        patch.object(SETUP, 'TOOLS', self.tools).start()
        patch.object(SETUP.platform, 'system', return_value='Linux').start()
        patch.object(SETUP.platform, 'machine', return_value='x86_64').start()
        self.execute = patch.object(SETUP.subprocess, 'check_output', side_effect=[json.dumps({'terraform_version':'1.0.0'}), '1.0.0\n']).start()
        self.download = patch.object(SETUP, 'download', side_effect=AssertionError('Cached fixtures must not use network')).start()

    def archive(self, name, content):
        filename = f'{name}.zip'
        (self.cache / filename).write_bytes(content)
        self.pins[name] = {'version':'1.0.0','platforms':{'linux-amd64':{'filename':filename,'url':'https://example.test/'+filename,'sha256':hashlib.sha256(content).hexdigest()}}}

    def save(self):
        (self.tools / 'versions.json').write_text(json.dumps(self.pins))

    def test_verified_binaries_and_licenses_publish_together(self):
        SETUP.install(self.destination, self.cache)
        self.assertEqual((self.destination / 'bin/terraform').read_bytes(), b'fixture binary')
        self.assertEqual((self.destination / 'licenses/terraform-ls.txt').read_bytes(), b'upstream license')
        self.assertTrue((self.destination / '.openwebide-release.json').is_file())
        self.assertEqual(self.execute.call_count, 2)
        self.download.assert_not_called()

    def test_corrupt_cache_never_executes_or_publishes(self):
        (self.cache / 'terraform.zip').write_bytes(b'corrupted')
        with self.assertRaisesRegex(RuntimeError, 'checksum'):
            SETUP.install(self.destination, self.cache)
        self.assertFalse(self.destination.exists())
        self.execute.assert_not_called()

    def test_duplicate_binary_archive_is_rejected_before_execution(self):
        stream = io.BytesIO()
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(stream, 'w') as archive:
                archive.writestr('terraform', b'first')
                archive.writestr('terraform', b'second')
        self.archive('terraform', stream.getvalue())
        self.save()
        with self.assertRaisesRegex(RuntimeError, 'Expected one'):
            SETUP.install(self.destination, self.cache)
        self.assertFalse(self.destination.exists())
        self.execute.assert_not_called()

    def test_version_mismatch_preserves_existing_data_and_leaves_no_install(self):
        sibling = self.root / 'user-data'
        sibling.write_text('keep')
        self.execute.side_effect = [json.dumps({'terraform_version':'different'})]
        with self.assertRaisesRegex(RuntimeError, 'version differs'):
            SETUP.install(self.destination, self.cache)
        self.assertFalse(self.destination.exists())
        self.assertEqual(sibling.read_text(), 'keep')
        self.destination.mkdir()
        (self.destination / 'user-data').write_text('existing')
        with self.assertRaisesRegex(RuntimeError, 'Existing Terraform tool directory differs'):
            SETUP.install(self.destination, self.cache)
        self.assertEqual((self.destination / 'user-data').read_text(), 'existing')


if __name__ == '__main__':
    unittest.main()
