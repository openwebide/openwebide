"""Check source-image cleanup and the shared cache's publication boundary."""

from pathlib import Path
import shutil
from subprocess import CalledProcessError, CompletedProcess
import tempfile
import unittest
from unittest.mock import patch

import build_container
import setup_language_tools


class SourceBuildTests(unittest.TestCase):
    def test_cleanup_follows_successful_compose_replacement(self):
        for should_fail in (False, True):
            with self.subTest(compose_fails=should_fail), tempfile.TemporaryDirectory() as directory:
                calls = []

                def docker(*args, **_):
                    calls.append(args)
                    if args[:2] == ('compose', 'config'):
                        return CompletedProcess(args, 0, '{"services":{"openwebide":{"image":"openwebide:local"}}}')
                    if should_fail and args[:2] == ('compose', 'up'):
                        raise CalledProcessError(1, args)
                    return CompletedProcess(args, 0)

                with patch.object(build_container, 'docker', docker), \
                     patch.object(build_container, 'builder_name', return_value='fixture-builder'), \
                     patch.object(build_container.tempfile, 'gettempdir', return_value=directory), \
                     patch.object(build_container.subprocess, 'check_output', return_value='revision'):
                    if should_fail:
                        with self.assertRaises(CalledProcessError):
                            build_container.app_build([])
                        self.assertEqual(calls[-1][:2], ('compose', 'up'))
                        self.assertFalse(any(call[:2] == ('image', 'prune') for call in calls))
                    else:
                        build_container.app_build([])
                        self.assertEqual(calls[-2][:2], ('compose', 'up'))
                        self.assertEqual(calls[-1], ('image', 'prune', '--force', '--filter',
                                                     'label=io.openwebide.local-build=true'))

    def test_measurement_failure_cleans_only_its_own_image(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for path in ('target/wasm32-wasip2/release/openwebide_backend.wasm',
                         'frontend/dist/index.html', 'tools/editor-view.Dockerfile'):
                file = root / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.touch()

            calls = []

            def docker(*args, **_):
                calls.append(args)
                return CompletedProcess(args, 7 if args[0] == 'run' else 0, 'sha256:abc')

            with patch.object(build_container, 'docker', docker), \
                 patch.object(build_container, 'REPO', root), \
                 patch.object(build_container, 'builder_name', return_value='fixture-builder'), \
                 patch.object(build_container.tempfile, 'gettempdir', return_value=directory):
                self.assertEqual(build_container.measure(['--repeat', '3'], row_update=True), 7)
            build = next(call for call in calls if call[:2] == ('buildx', 'build'))
            tag = build[build.index('--tag') + 1]
            run = next(call for call in calls if call[0] == 'run')
            self.assertEqual(run[-6:], ('--entrypoint', 'python3', tag,
                                        'tools/measure-editor-row-update.py', '--repeat', '3'))
            self.assertEqual(calls[-1], ('image', 'rm', '--no-prune', tag))


class LanguageToolCacheTests(unittest.TestCase):
    def test_install_is_reused_by_another_worktree_until_pins_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            checkout = root / 'one'
            pins = checkout / 'go-language-server'
            (pins / 'dependencies').mkdir(parents=True)
            (pins / 'versions.json').write_text('{"go":"1"}')
            (pins / 'dependencies/go.mod').write_text('go 1.27.3')
            (pins / 'dependencies/go.sum').write_text('gopls checksum')
            second_checkout = root / 'two'
            shutil.copytree(checkout, second_checkout)
            installs = []

            def install(destination):
                installs.append(destination)
                (destination / 'bin').mkdir(parents=True)
                (destination / 'go/bin').mkdir(parents=True)

            with patch.dict('os.environ', OPENWEBIDE_TOOL_CACHE=str(root / 'cache')):
                with patch.object(setup_language_tools, 'TOOLS', checkout):
                    first = setup_language_tools.setup_tool('go', install)
                with patch.object(setup_language_tools, 'TOOLS', second_checkout):
                    self.assertEqual(first, setup_language_tools.setup_tool('go', install))
                    (second_checkout / 'go-language-server/dependencies/go.sum').write_text('new checksum')
                    self.assertNotEqual(first, setup_language_tools.setup_tool('go', install))
            self.assertEqual(len(installs), 2)

    def test_failed_install_is_retryable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'clangd-language-server').mkdir()
            (root / 'clangd-language-server/versions.json').write_text('{}')

            def fail(destination):
                (destination / 'bin').mkdir(parents=True)
                raise RuntimeError('download failed')

            with patch.dict('os.environ', OPENWEBIDE_TOOL_CACHE=str(root / 'cache')), \
                 patch.object(setup_language_tools, 'TOOLS', root):
                with self.assertRaisesRegex(RuntimeError, 'download failed'):
                    setup_language_tools.setup_tool('clangd', fail)
                self.assertEqual(list((root / 'cache/clangd').glob('.install-*')), [])
                paths = setup_language_tools.setup_tool(
                    'clangd', lambda target: (target / 'bin').mkdir(parents=True))
                self.assertTrue(paths[0].is_dir())
