"""Release gates prevent mismatched tags, stale versions and incomplete notes."""

import hashlib
import json
import os
import re
import subprocess
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

import release


def workflow_script(step_name, filename='release.yml'):
    workflow = (release.ROOT / '.github/workflows' / filename).read_text()
    step = workflow.split(f'      - name: {step_name}\n', 1)[1]
    lines = step.split('        run: |\n', 1)[1].splitlines()
    script = []
    for line in lines:
        if line.strip() and not line.startswith('          '):
            break
        script.append(line)
    return textwrap.dedent('\n'.join(script))


class ReleaseTests(unittest.TestCase):
    def test_update_metadata_uses_the_storage_schema_and_rejects_missing_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'crates/storage/src/migrations.rs'
            source.parent.mkdir(parents=True)
            source.write_text('pub const SCHEMA_VERSION: i64 = 105;\n')
            self.assertEqual(release.database_schema(root), 105)
            source.write_text('// schema unavailable\n')
            with self.assertRaises(ValueError):
                release.database_schema(root)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "bridge").mkdir()
        (self.root / "docs").mkdir()
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["bridge"]\n[workspace.package]\nversion = "1.0.0"\n'
        )
        (self.root / "spin.toml").write_text('[application]\nversion = "1.0.0"\n')
        (self.root / "bridge/Cargo.toml").write_text('[package]\nname = "bridge"\nversion.workspace = true\n')
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "bridge"\nversion = "1.0.0"\n')
        (self.root / "CHANGELOG.md").write_text(
            '# Changelog\n\n## [Unreleased]\n\n- Future.\n\n## [1.0.0] - 2026-10-07\n\n- Shipped.\n\n## [0.1.0]\n\n- Old.\n'
        )

    def set_version(self, value, previous='1.0.0'):
        for file in ['Cargo.toml', 'spin.toml', 'Cargo.lock', 'CHANGELOG.md']:
            path = self.root / file
            path.write_text(path.read_text().replace(previous, value))

    def test_prerelease_versions_keep_tag_notes_and_channels_consistent(self):
        value = '1.0.0-rc.1'
        self.set_version(value)
        self.assertEqual(release.validate(self.root, f'v{value}'), (value, '- Shipped.\n'))
        self.assertEqual(release.release_channel(value), 'preview')
        self.assertEqual(release.github_outputs(value), f'version={value}\nchannel=preview\nprerelease=true\n')

    def test_nightly_stamps_workspace_and_preserves_sdk_and_history(self):
        sdk = self.root / 'sdk'
        sdk.mkdir()
        (sdk / 'Cargo.toml').write_text('[package]\nname = "openwebide-plugin-sdk"\nversion = "0.1.0"\n')
        cargo = self.root / 'Cargo.toml'
        cargo.write_text(cargo.read_text().replace('["bridge"]', '["bridge", "sdk"]'))
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text() + '\n[[package]]\nname = "openwebide-plugin-sdk"\nversion = "0.1.0"\n')
        changelog = (self.root / 'CHANGELOG.md').read_text()
        for commit in ('a' * 40, 'b' * 40):
            expected = f'1.0.0-nightly+sha.{commit[:12]}'
            self.assertEqual(release.stamp_nightly(self.root, commit), expected)
            self.assertEqual(release.validate(self.root), (expected, '- Future.\n'))
            self.assertEqual(release.release_channel(expected), 'nightly')
            self.assertIn('version = "0.1.0"', lock.read_text())
            self.assertEqual((self.root / 'CHANGELOG.md').read_text(), changelog)
            with self.assertRaises(ValueError):
                release.validate(self.root, f'v{expected}')
        with self.assertRaises(ValueError):
            release.stamp_nightly(self.root, 'unknown')

    def test_development_nightly_uses_unreleased_notes(self):
        self.set_version('1.0.0-nightly')
        self.assertEqual(release.validate(self.root), ('1.0.0-nightly', '- Future.\n'))
        self.assertEqual(release.release_channel('1.0.0-nightly'), 'nightly')

    def test_stable_versions_publish_to_stable(self):
        self.assertEqual(release.github_outputs('1.0.0'), 'version=1.0.0\nchannel=stable\nprerelease=false\n')

    def test_rejects_unsupported_or_malformed_versions(self):
        for value in ['01.0.0', '1.0', '1.0.0-alpha.1', '1.0.0-alpha', '1.0.0-alpha.01', '1.0.0-dev.1', '1.0.0-beta.1', '1.0.0-beta.1+build', '1.0.0-rc.1;echo bad']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                release.release_channel(value)

    def test_install_assets_pin_compose_and_follow_correct_quadlet_channel(self):
        sources = {
            'docker-compose.release.yml': 'image: ghcr.io/openwebide/openwebide:stable\n',
            'docker-compose.https.yml': 'image: ghcr.io/openwebide/openwebide:stable\n',
            'docker-compose.ssh.yml': 'services: {}\n',
            'deploy/openwebide.image': '[Image]\nImage=ghcr.io/openwebide/openwebide:stable\n',
            'deploy/openwebide.container': '[Container]\nImage=openwebide.image\nPull=newer\nAutoUpdate=registry\n',
            'docker/tailscale/serve-config.sh': '#!/bin/sh\necho config\n',
        }
        for name, content in sources.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        for value, channel in [('1.0.0', 'stable'), ('1.0.0-rc.2', 'preview'), ('1.0.0-nightly', 'nightly')]:
            self.set_version(value)
            output = self.root / value
            release.prepare_install_files(self.root, output)
            with self.subTest(version=value):
                pinned = 'nightly' if channel == 'nightly' else f'v{value}'
                for name in ['docker-compose.yml', 'docker-compose.https.yml']:
                    self.assertIn(f'openwebide:{pinned}', (output / name).read_text())
                    self.assertNotIn('openwebide:stable', (output / name).read_text())
                    if channel == 'nightly':
                        self.assertNotIn('openwebide:v', (output / name).read_text())
                self.assertIn(f'openwebide:{channel}', (output / 'openwebide.image').read_text())
                self.assertEqual((output / 'docker-compose.ssh.yml').read_text(), sources['docker-compose.ssh.yml'])
                self.assertIn('AutoUpdate=registry', (output / 'openwebide.container').read_text())
                with self.assertRaises(ValueError):
                    release.prepare_install_files(self.root, output)
            self.set_version('1.0.0', previous=value)
        template = self.root / 'docker-compose.release.yml'
        template.write_text('image: ghcr.io/openwebide/openwebide:latest\n')
        with self.assertRaisesRegex(ValueError, 'does not reference'):
            release.prepare_install_files(self.root, self.root / 'drifted')

    def test_nightly_bridge_archives_use_stable_names(self):
        self.set_version('1.0.0-nightly+sha.' + 'a' * 12)
        binary = self.root / 'bridge-binary'
        binary.write_bytes(b'test binary')
        (self.root / 'LICENSE').write_text('MIT')
        (self.root / 'docs/execution-bridge.md').write_text('Bridge instructions')
        output = self.root / 'assets'
        archive = release.package_bridge(self.root, 'aarch64-apple-darwin', binary, output)
        self.assertEqual(archive.name, 'openwebide-bridge-nightly-aarch64-apple-darwin.tar.gz')
        with tarfile.open(archive) as bundle:
            self.assertTrue(all(name.startswith('openwebide-bridge-nightly-aarch64-apple-darwin/') for name in bundle.getnames()))
        release.checksums(output)
        expected = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual((output / 'SHA256SUMS').read_text(), f'{expected}  {archive.name}\n')

    def test_nightly_release_assets_replace_in_place(self):
        script = workflow_script('Publish rolling nightly release assets')
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        log = self.root / 'commands.jsonl'
        for command in ['gh', 'docker']:
            path = bin_dir / command
            path.write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ['COMMAND_LOG'], 'a') as stream:
    stream.write(json.dumps([os.path.basename(sys.argv[0]), *args]) + '\\n')
if os.environ.get('GH_MISSING') == 'true' and args in [
    ['api', 'repos/openwebide/openwebide/git/ref/tags/nightly'],
    ['release', 'view', 'nightly'],
]:
    sys.exit(1)
if args[:4] == ['release', 'view', 'nightly', '--json'] and os.environ.get('GH_MISSING') == 'false':
    print('openwebide-bridge-nightly-x86_64-unknown-linux-gnu.tar.gz')
    print('openwebide-bridge-nightly-x86_64-apple-darwin.tar.gz')
''')
            path.chmod(0o755)
        assets = self.root / 'assets'
        assets.mkdir()
        for name in ['docker-compose.yml', 'openwebide.image', 'SHA256SUMS']:
            (assets / name).touch()
        commit = 'c' * 40
        for missing in ['true', 'false']:
            log.write_text('')
            env = os.environ | {'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
                                'COMMAND_LOG': str(log), 'GH_MISSING': missing, 'RUNNER_TEMP': str(self.root),
                                'GH_REPO': 'openwebide/openwebide', 'BUILD_COMMIT': commit}
            subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env, check=True)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            with self.subTest(missing=missing):
                self.assertFalse([call for call in calls if call[0] == 'docker'])
                creates = [call for call in calls if call[:3] == ['gh', 'release', 'create']]
                if missing == 'true':
                    post = next(call for call in calls if call[:4] == ['gh', 'api', '--method', 'POST'])
                    self.assertIn('repos/openwebide/openwebide/git/refs', post)
                    self.assertIn('ref=refs/tags/nightly', post)
                    self.assertIn(f'sha={commit}', post)
                    self.assertEqual(len(creates), 1)
                    for flag in ['nightly', '--verify-tag', '--prerelease', '--latest=false']:
                        self.assertIn(flag, creates[0])
                else:
                    patch = next(call for call in calls if call[:4] == ['gh', 'api', '--method', 'PATCH'])
                    self.assertIn('repos/openwebide/openwebide/git/refs/tags/nightly', patch)
                    self.assertIn('force=true', patch)
                    self.assertEqual(creates, [])
                removed = [call[3:] for call in calls if call[:3] == ['gh', 'release', 'delete-asset']]
                self.assertEqual(removed, [
                    ['nightly', 'openwebide-bridge-nightly-x86_64-apple-darwin.tar.gz', '--yes'],
                ] if missing == 'false' else [])
                uploads = [call for call in calls if call[:3] == ['gh', 'release', 'upload']]
                self.assertEqual(len(uploads), 2)
                for call in uploads:
                    self.assertIn('--clobber', call)
                self.assertEqual([arg for arg in uploads[-1] if arg.startswith('assets/')], ['assets/SHA256SUMS'])
                self.assertNotIn('assets/SHA256SUMS', uploads[0])
                self.assertEqual(calls[-1][:3], ['gh', 'release', 'edit'])
                self.assertIn('--prerelease', calls[-1])
                self.assertIn('--latest=false', calls[-1])
                self.assertNotIn('--latest', calls[-1])
                body = (self.root / 'nightly-notes.md').read_text()
                for text in [commit, f'sha-{commit}', f'blob/{commit}/CHANGELOG.md']:
                    self.assertIn(text, body)

    def test_rolling_builds_package_and_publish_assets(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        self.assertNotIn("rolling != 'true'", workflow.replace("publish == 'true' && needs.validate.outputs.rolling != 'true'", ''))
        job = workflow.split('\n  publish-nightly:', 1)[1]
        self.assertIn('needs: [validate, checks, images, assets]', job)
        self.assertIn('contents: write', job)

    def test_native_companion_builds_receive_the_bundle_and_correct_version(self):
        script = workflow_script('Build and smoke-test native bridge', 'macos-companions.yml')
        tools = self.root / 'tools'
        tools.mkdir()
        (tools / 'release.py').write_text(Path(release.__file__).read_text())
        (self.root / 'LICENSE').write_text('Fixture license')
        (self.root / 'docs/execution-bridge.md').write_text('Fixture guide')
        bundle = self.root / 'portable-defaults.json'
        bundle.write_text('{"artifacts":[]}')
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        log = self.root / 'commands.jsonl'
        cargo = bin_dir / 'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys, tomllib
from pathlib import Path
version = tomllib.loads(Path('Cargo.toml').read_text())['workspace']['package']['version']
with open(os.environ['COMMAND_LOG'], 'a') as stream:
    stream.write(json.dumps({'args': sys.argv[1:], 'version': version,
                             'bundle': os.environ['OPENWEBIDE_BUNDLED_ARTIFACTS']}) + '\\n')
if sys.argv[1] == 'build':
    binary = Path('target/release/openwebide-bridge')
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text('#!/bin/sh\\nexit 0\\n')
    binary.chmod(0o755)
''')
        cargo.chmod(0o755)
        commit = 'c' * 40
        target = 'aarch64-apple-darwin'
        for rolling in (False, True):
            with self.subTest(rolling=rolling):
                if rolling:
                    self.set_version('1.0.0-nightly')
                output = self.root / str(rolling)
                env = os.environ | {
                    'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
                    'COMMAND_LOG': str(log), 'RUNNER_TEMP': str(output),
                    'RELEASE_TARGET': target, 'BUILD_COMMIT': commit,
                    'OPENWEBIDE_BUNDLED_ARTIFACTS': str(bundle),
                    'RELEASE_TAG': 'nightly' if rolling else 'v1.0.0',
                }
                log.unlink(missing_ok=True)
                subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env,
                               capture_output=True, text=True, check=True)
                expected = '1.0.0-nightly+sha.' + commit[:12] if rolling else '1.0.0'
                calls = [json.loads(line) for line in log.read_text().splitlines()]
                self.assertTrue(calls)
                self.assertTrue(all(call['version'] == expected for call in calls))
                self.assertTrue(all(call['bundle'] == str(bundle) for call in calls))
                label = 'nightly' if rolling else 'v1.0.0'
                archive = output / 'assets' / f'openwebide-bridge-{label}-{target}.tar.gz'
                with tarfile.open(archive) as packaged:
                    binary = next(item for item in packaged if item.name.endswith('/openwebide-bridge'))
                    self.assertEqual(binary.mode, 0o755)

    def test_macos_workflow_only_publishes_for_the_matching_published_release(self):
        workflow = 'macos-companions.yml'
        plan = workflow_script('Find the published release', workflow)
        publish = workflow_script('Publish tested macOS companions', workflow)
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        log = self.root / 'commands.jsonl'
        output = self.root / 'github-output'
        assets = self.root / 'assets'
        assets.mkdir()
        (assets / 'bridge.tar.gz').touch()
        gh = bin_dir / 'gh'
        gh.write_text('''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ['COMMAND_LOG'], 'a') as stream:
    stream.write(json.dumps(args) + '\\n')
if args[:2] == ['api', '--paginate']:
    print(os.environ['PUBLISHED_JOB'])
elif args[0] == 'api' and '/commits/' in args[1]:
    print(os.environ['PUBLISHED_COMMIT'])
''')
        gh.chmod(0o755)
        commit = 'a' * 40
        base_env = os.environ | {
            'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
            'COMMAND_LOG': str(log), 'GITHUB_OUTPUT': str(output),
            'GITHUB_STEP_SUMMARY': str(self.root / 'job-summary'),
            'GH_REPO': 'openwebide/openwebide', 'RUNNER_TEMP': str(self.root),
            'RELEASE_RUN': '123', 'BUILD_COMMIT': commit, 'PUBLISHED_COMMIT': commit,
        }
        for job, event, ref, tag in [
            ('', 'schedule', 'main', ''),
            ('', 'workflow_dispatch', 'main', ''),
            ('publish-nightly', 'schedule', 'main', 'nightly'),
            ('publish', 'push', 'v1.0.0-rc.3', 'v1.0.0-rc.3'),
        ]:
            with self.subTest(event=event, job=job):
                output.write_text('')
                env = base_env | {'PUBLISHED_JOB': job, 'RELEASE_EVENT': event, 'RELEASE_REF': ref}
                subprocess.run(['bash', '-e', '-c', plan], cwd=self.root, env=env, check=True)
                self.assertEqual(output.read_text(), f'tag={tag}\n' if tag else '')

        for tag, current in [('nightly', 'b' * 40), ('nightly', commit), ('v1.0.0-rc.3', commit)]:
            with self.subTest(tag=tag, current=current):
                log.write_text('')
                (assets / 'SHA256SUMS').write_text('archive checksums')
                env = base_env | {'RELEASE_TAG': tag, 'PUBLISHED_COMMIT': current}
                subprocess.run(['bash', '-e', '-c', publish], cwd=self.root, env=env, check=True)
                calls = [json.loads(line) for line in log.read_text().splitlines()]
                uploads = [call[2:4] for call in calls if call[:2] == ['release', 'upload']]
                if current != commit:
                    self.assertEqual(len(calls), 1)
                elif tag == 'nightly':
                    self.assertEqual(uploads, [
                        ['nightly-macos', 'assets/bridge.tar.gz'],
                        ['nightly-macos', 'assets/SHA256SUMS'],
                    ])
                else:
                    self.assertEqual(uploads, [
                        [tag, 'assets/bridge.tar.gz'],
                        [tag, 'assets/SHA256SUMS-macos'],
                    ])

    def test_publish_commands_do_not_promote_prereleases_to_latest(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        publish = workflow.split('      - name: Publish multi-architecture image and release\n', 1)[1]
        script = textwrap.dedent(publish.split('        run: |\n', 1)[1].split('\n  publish-nightly:', 1)[0])
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        log = self.root / 'commands.jsonl'
        mock = '''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['COMMAND_LOG'], 'a') as stream:
    stream.write(json.dumps([os.path.basename(sys.argv[0]), *sys.argv[1:]]) + '\\n')
'''
        for command in ['gh', 'docker']:
            path = bin_dir / command
            path.write_text(mock)
            path.chmod(0o755)
        for directory in ['digests', 'assets', 'notes']:
            (self.root / directory).mkdir()
        for name in ['a', 'b']:
            (self.root / 'digests' / name).touch()
        (self.root / 'assets/archive.tar.gz').touch()
        for value in ['1.0.0', '1.0.0-rc.3']:
            log.write_text('')
            outputs = dict(line.split('=', 1) for line in release.github_outputs(value).splitlines())
            env = os.environ | {
                'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
                'COMMAND_LOG': str(log),
                'GH_REPO': 'openwebide/openwebide',
                'RELEASE_TAG': f'v{value}',
                'CHANNEL': outputs['channel'],
                'PRERELEASE': outputs['prerelease'],
            }
            subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env, check=True)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            with self.subTest(version=value):
                writes = [call[call.index('--tag') + 1] for call in calls if call[:4] == ['docker', 'buildx', 'imagetools', 'create']]
                image = 'ghcr.io/openwebide/openwebide'
                expected = [f'{image}:v{value}', f"{image}:{outputs['channel']}"]
                self.assertEqual(writes, expected)
                self.assertNotIn(image + ':latest', writes)
                edit = next(call for call in calls if call[:3] == ['gh', 'release', 'edit'])
                create = next(call for call in calls if call[:3] == ['gh', 'release', 'create'])
                if outputs['prerelease'] == 'true':
                    for call in [create, edit]:
                        self.assertIn('--prerelease', call)
                        self.assertIn('--latest=false', call)
                        self.assertNotIn('--latest', call)
                    self.assertNotIn(image + ':latest', writes)
                else:
                    self.assertIn('--latest', edit)
                    self.assertNotIn('--prerelease', edit)

    def test_main_and_tag_publication_selection_and_manual_dry_run(self):
        tools = self.root / 'tools'
        tools.mkdir()
        (tools / 'release.py').write_text((release.ROOT / 'tools/release.py').read_text())
        output = self.root / 'outputs'
        env = os.environ | {'RUNNER_TEMP': str(self.root), 'GITHUB_OUTPUT': str(output), 'BUILD_COMMIT': 'a' * 40}
        cases = [
            ('true', 'true', 'main', '1.0.0', True, '- Future.'),
            ('false', 'false', 'main', '1.0.0', True, '- Future.'),
            ('true', 'false', 'v1.0.0-rc.1', '1.0.0-rc.1', True, '- Shipped.'),
            ('true', 'false', 'v1.0.0', '1.0.0', True, '- Shipped.'),
            ('true', 'false', 'v1.0.0-alpha.7', '1.0.0-alpha.7', False, None),
        ]
        current = '1.0.0'
        for publish, rolling, tag, value, succeeds, notes in cases:
            self.set_version(value, previous=current)
            current = value
            output.write_text('')
            result = subprocess.run(['bash', '-e', '-c', workflow_script('Validate versions and changelog')],
                                    cwd=self.root, env=env | {'PUBLISH': publish, 'ROLLING': rolling, 'RELEASE_TAG': tag},
                                    capture_output=True, text=True)
            with self.subTest(tag=tag, publish=publish):
                self.assertEqual(result.returncode == 0, succeeds, result.stderr)
                if succeeds:
                    values = dict(line.split('=', 1) for line in output.read_text().splitlines())
                    self.assertEqual(values['publish'], publish)
                    self.assertEqual(values['rolling'], rolling)
                    expected_version = value.split('-', 1)[0] + '-nightly+sha.' + 'a' * 12 if rolling == 'true' else value
                    self.assertEqual(values['version'], expected_version)
                    self.assertEqual((self.root / 'release-notes.md').read_text().strip(), notes)
                else:
                    self.assertNotIn('publish=true', output.read_text())
            if rolling == 'true':
                self.set_version(value, previous=expected_version)

    def test_rolling_nightly_promotes_only_complete_platform_manifests(self):
        script = workflow_script('Publish rolling nightly image')
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        log = self.root / 'commands.jsonl'
        docker = bin_dir / 'docker'
        docker.write_text("#!/usr/bin/env python3\nimport json, os, sys\nwith open(os.environ['COMMAND_LOG'], 'a') as stream: stream.write(json.dumps(sys.argv[1:]) + '\\n')\n")
        docker.chmod(0o755)
        digests = self.root / 'digests'
        digests.mkdir()
        (digests / ('a' * 64)).touch()
        env = os.environ | {'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
                            'COMMAND_LOG': str(log), 'IMAGE': 'ghcr.io/openwebide/openwebide',
                            'BUILD_COMMIT': 'c' * 40}
        result = subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(log.exists())
        (digests / ('b' * 64)).touch()
        subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env, check=True)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        writes = [call[call.index('--tag') + 1] for call in calls if '--tag' in call]
        image = env['IMAGE']
        self.assertEqual(writes, [f"{image}:sha-{env['BUILD_COMMIT']}", f'{image}:nightly'])
        self.assertEqual(calls[-1][-1], writes[0])
        self.assertEqual(calls[0][-2:], [f'{image}@sha256:' + 'a' * 64, f'{image}@sha256:' + 'b' * 64])

    def test_push_uses_smoke_tested_image_and_stops_on_failure(self):
        script = workflow_script('Push tested platform image')
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        log = self.root / 'commands.jsonl'
        output = self.root / 'outputs'
        docker = bin_dir / 'docker'
        docker.write_text(r"""#!/usr/bin/env python3
import json, os, sys
with open(os.environ['COMMAND_LOG'], 'a') as stream:
    stream.write(json.dumps(sys.argv[1:]) + '\n')
if sys.argv[1] == 'push' and os.environ.get('FAIL_PUSH') == 'true': sys.exit(1)
if sys.argv[1] == 'inspect': print(os.environ['IMAGE'] + '@sha256:' + 'd' * 64)
""")
        docker.chmod(0o755)
        env = os.environ | {'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
                            'COMMAND_LOG': str(log), 'IMAGE': 'ghcr.io/openwebide/openwebide',
                            'BUILD_COMMIT': 'c' * 40, 'ARCH': 'arm64', 'GITHUB_OUTPUT': str(output), 'BUILD_COMMIT': 'a' * 40}
        failed = subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env | {'FAIL_PUSH': 'true'})
        self.assertNotEqual(failed.returncode, 0)
        self.assertFalse(output.exists())
        log.write_text('')
        subprocess.run(['bash', '-e', '-c', script], cwd=self.root, env=env, check=True)
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        platform = f"{env['IMAGE']}:sha-{env['BUILD_COMMIT']}-arm64"
        self.assertEqual(calls[0], ['tag', 'openwebide:release-test', platform])
        self.assertEqual(calls[1], ['push', platform])
        self.assertEqual(output.read_text(), 'digest=sha256:' + 'd' * 64 + '\n')

    def test_release_notes_only_include_tagged_section(self):
        self.assertEqual(release.validate(self.root, "v1.0.0"), ("1.0.0", "- Shipped.\n"))
        self.assertEqual(release.validate(self.root), ("1.0.0", "- Future.\n"))

    def test_oversized_publication_notes_fail_before_building_release_artifacts(self):
        (self.root / "CHANGELOG.md").write_text('## [Unreleased]\n\n## [1.0.0]\n\n' + 'x' * 125000)
        with self.assertRaisesRegex(ValueError, "125000-character limit"):
            release.validate(self.root, "v1.0.0")

    def test_rejects_tag_mismatch_and_injected_tag(self):
        for tag in ["v0.1.0", "v1.0.0; echo bad", "v1.0.0-rc.1"]:
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.validate(self.root, tag)

    def test_rejects_mismatched_manifest_or_lock(self):
        for file, content in [
            ("spin.toml", '[application]\nversion = "0.1.0"\n'),
            ("Cargo.lock", '[[package]]\nname = "bridge"\nversion = "0.1.0"\n'),
            ("bridge/Cargo.toml", '[package]\nname = "bridge"\nversion = "0.1.0"\n'),
        ]:
            with self.subTest(file=file):
                path = self.root / file
                previous = path.read_text()
                path.write_text(content)
                with self.assertRaises(ValueError):
                    release.validate(self.root)
                path.write_text(previous)

    def test_public_plugin_sdk_keeps_its_independent_locked_version(self):
        cargo = self.root / "Cargo.toml"
        cargo.write_text(cargo.read_text().replace('["bridge"]', '["bridge", "crates/plugin-sdk"]'))
        sdk = self.root / "crates/plugin-sdk/Cargo.toml"
        sdk.parent.mkdir(parents=True)
        sdk.write_text('[package]\nname = "openwebide-plugin-sdk"\nversion = "0.1.0"\n')
        lock = self.root / "Cargo.lock"
        lock.write_text(lock.read_text() + '\n[[package]]\nname = "openwebide-plugin-sdk"\nversion = "0.1.0"\n')
        self.assertEqual(release.version(self.root), "1.0.0")
        sdk.write_text(sdk.read_text().replace('"0.1.0"', '"0.2.0"'))
        with self.assertRaises(ValueError):
            release.version(self.root)

    def test_language_toolkits_keep_independent_versions_across_nightly_stamps(self):
        members = ["bridge"]
        lock = self.root / "Cargo.lock"
        for name in ("openwebide-language-grammars", "openwebide-language-syntax"):
            member = f"crates/{name}"
            members.append(member)
            manifest = self.root / member / "Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n')
            lock.write_text(lock.read_text() + f'\n[[package]]\nname = "{name}"\nversion = "0.1.0"\n')
        cargo = self.root / "Cargo.toml"
        cargo.write_text(cargo.read_text().replace('["bridge"]', json.dumps(members)))
        self.assertEqual(release.version(self.root), "1.0.0")
        release.stamp_nightly(self.root, "a" * 40)
        for name in ("openwebide-language-grammars", "openwebide-language-syntax"):
            self.assertIn(f'name = "{name}"\nversion = "0.1.0"', lock.read_text())

    def test_missing_empty_or_duplicate_notes_block_release(self):
        for changelog in ["## [Unreleased]\n- Only unreleased.\n", "## [1.0.0]\n\n", "## [1.0.0]\n- One.\n## [1.0.0]\n- Two.\n"]:
            (self.root / "CHANGELOG.md").write_text(changelog)
            with self.subTest(changelog=changelog), self.assertRaises(ValueError):
                release.validate(self.root, "v1.0.0")

    def test_empty_unreleased_section_does_not_block_tagged_release_ci(self):
        (self.root / "CHANGELOG.md").write_text("## [Unreleased]\n\n## [1.0.0]\n\n- Shipped.\n")
        self.assertEqual(release.validate(self.root), ("1.0.0", "\n"))
        self.assertEqual(release.validate(self.root, "v1.0.0"), ("1.0.0", "- Shipped.\n"))

    def test_archive_is_installable_and_checksums_match(self):
        binary = self.root / "bridge-binary"
        binary.write_bytes(b"test binary")
        (self.root / "LICENSE").write_text("MIT")
        (self.root / "docs/execution-bridge.md").write_text("Bridge instructions")
        output = self.root / "assets"
        archive = release.package_bridge(self.root, "aarch64-apple-darwin", binary, output)
        with tarfile.open(archive) as bundle:
            prefix = "openwebide-bridge-v1.0.0-aarch64-apple-darwin/"
            self.assertEqual(bundle.getnames(), [prefix + name for name in ["openwebide-bridge", "LICENSE", "execution-bridge.md"]])
            self.assertEqual(bundle.getmember(prefix + "openwebide-bridge").mode, 0o755)
            self.assertEqual(bundle.extractfile(prefix + "openwebide-bridge").read(), b"test binary")
        release.checksums(output)
        expected = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual((output / "SHA256SUMS").read_text(), f"{expected}  {archive.name}\n")
        with self.assertRaises(ValueError):
            release.package_bridge(self.root, "aarch64-apple-darwin", binary, output)


if __name__ == "__main__":
    unittest.main()
