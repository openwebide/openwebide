"""Pinned LLVM payload selection and installation failure boundaries."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('clangd_setup', Path(__file__).with_name('setup-clangd-language-server.py'))
SETUP = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SETUP)


class LicenseCheckout(unittest.TestCase):
    def test_git_checkout_preserves_upstream_bytes_and_other_whitespace_checks(self):
        source = Path(__file__).resolve().parent.parent
        relative = 'tools/clangd-language-server/LICENSE.TXT'
        payload = (source / relative).read_bytes()
        pins = json.loads((source / 'tools/clangd-language-server/versions.json').read_text())
        self.assertEqual(hashlib.sha256(payload).hexdigest(),
                         pins['platforms']['linux-arm64']['licenseSha256'])
        with tempfile.TemporaryDirectory(prefix='clangd-license-checkout-') as directory:
            root = Path(directory)

            def git(*arguments, check=True):
                return subprocess.run(['git', '-C', directory, *arguments],
                                      check=check, capture_output=True, text=True)

            git('init', '--quiet', '--initial-branch=main', '--template=')
            git('config', 'core.autocrlf', 'true')
            (root / '.gitattributes').write_bytes((source / '.gitattributes').read_bytes())
            license_file = root / relative
            license_file.parent.mkdir(parents=True)
            license_file.write_bytes(payload)
            git('add', '.gitattributes', relative)
            license_file.unlink()
            git('checkout-index', '--', relative)
            self.assertEqual(license_file.read_bytes(), payload)
            git('diff', '--cached', '--check')
            (root / 'ordinary.txt').write_bytes(b'bad trailing space \n')
            git('add', 'ordinary.txt')
            result = git('diff', '--cached', '--check', check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('ordinary.txt', result.stdout)
            git('update-index', '--force-remove', 'ordinary.txt')
            license_file.write_bytes(payload + b'bad trailing space \n')
            git('add', relative)
            result = git('diff', '--cached', '--check', check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(relative, result.stdout)


class Distributions(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory(prefix='clangd-installer-contract-')
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.archive = self.root / 'release.tar.xz'
        self.destination = self.root / 'installed'
        self.payload = [('bin/clangd', b'binary'), ('lib/clang/23/include/stddef.h', b'header'),
                        ('bin/clang', b'unneeded compiler'), ('lib/liblldb.so', b'unneeded debugger')]

    def bundle(self, extra=()):
        with tarfile.open(self.archive, 'w:xz') as bundle:
            for name, content in [*self.payload, *extra]:
                entry = tarfile.TarInfo('LLVM/' + name)
                if isinstance(content, str):
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = content
                    bundle.addfile(entry)
                else:
                    entry.size = len(content)
                    bundle.addfile(entry, io.BytesIO(content))

    def test_only_clangd_and_resource_headers_are_extracted(self):
        self.bundle()
        self.destination.mkdir()
        SETUP.extract_llvm(self.archive, self.destination, 'LLVM', 23)
        self.assertEqual((self.destination/'bin/clangd').read_bytes(), b'binary')
        self.assertEqual((self.destination/'lib/clang/23/include/stddef.h').read_bytes(), b'header')
        self.assertFalse((self.destination/'bin/clang').exists())
        self.assertFalse((self.destination/'lib/liblldb.so').exists())

    def test_traversal_and_linked_headers_never_escape_the_staging_directory(self):
        for name, content in [('lib/clang/23/include/../../../../../outside', b'escape'),
                              ('lib/clang/23/include/linked.h', '../../../../../outside')]:
            with self.subTest(name=name), tempfile.TemporaryDirectory(dir=self.root) as directory:
                self.bundle([(name, content)])
                with self.assertRaises(RuntimeError):
                    SETUP.extract_llvm(self.archive, Path(directory), 'LLVM', 23)
                self.assertFalse((self.root/'outside').exists())

    def test_duplicate_or_oversized_payload_is_rejected(self):
        self.bundle([('bin/clangd', b'replacement')])
        self.destination.mkdir()
        with self.assertRaisesRegex(RuntimeError, 'duplicate'):
            SETUP.extract_llvm(self.archive, self.destination, 'LLVM', 23)
        self.bundle()
        with patch.object(SETUP, 'MAX_INSTALLED_BYTES', 1), self.assertRaisesRegex(RuntimeError, 'extraction limit'):
            SETUP.extract_llvm(self.archive, self.destination, 'LLVM', 23)

    def test_missing_resource_headers_is_not_an_installable_distribution(self):
        self.destination.mkdir()
        for extra in [[], [('lib/clang/23/include', b'not a header directory')]]:
            with self.subTest(extra=extra):
                self.payload = [('bin/clangd', b'binary'), *extra]
                self.bundle()
                with self.assertRaisesRegex(RuntimeError, 'resource headers'):
                    SETUP.extract_llvm(self.archive, self.destination, 'LLVM', 23)

    def install(self, version):
        tools = self.root/'tools'
        tools.mkdir(exist_ok=True)
        license_text = b'upstream license'
        (tools/'LICENSE.TXT').write_bytes(license_text)
        pins = {'major': 23, 'platforms': {'linux-arm64': {'version': '23.1.0',
            'licenseSha256': hashlib.sha256(license_text).hexdigest(), 'archives': [{
                'url': 'https://example.test/release.tar.xz', 'root': 'LLVM',
                'sha256': hashlib.sha256(self.archive.read_bytes()).hexdigest()}]}}}
        (tools/'versions.json').write_text(json.dumps(pins))
        with patch.object(SETUP, 'TOOLS', tools), patch.object(SETUP.platform, 'system', return_value='Linux'), \
                patch.object(SETUP.platform, 'machine', return_value='aarch64'), \
                patch.object(SETUP.subprocess, 'check_output', return_value=version):
            SETUP.install(self.destination, self.root)

    def test_verified_payload_and_license_publish_together(self):
        self.bundle()
        self.install('clangd version 23.1.0')
        self.assertEqual((self.destination/'LICENSE.TXT').read_bytes(), b'upstream license')
        self.assertTrue((self.destination/'.openwebide-release.json').is_file())

    def test_failed_version_probe_leaves_no_published_install(self):
        self.bundle()
        sibling = self.root/'user-data'
        sibling.write_text('keep')
        with self.assertRaisesRegex(RuntimeError, 'version differs'):
            self.install('clangd version 0.0.0')
        self.assertFalse(self.destination.exists())
        self.assertEqual(sibling.read_text(), 'keep')


if __name__ == '__main__':
    unittest.main()
