"""Release gates prevent mismatched tags, stale versions and incomplete notes."""

import hashlib
import json
import os
import subprocess
import tarfile
import tempfile
import textwrap
import unittest
from pathlib import Path

import release


class ReleaseTests(unittest.TestCase):
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
        for channel in ['alpha', 'beta', 'rc']:
            value = f'1.0.0-{channel}.1'
            self.set_version(value)
            with self.subTest(channel=channel):
                self.assertEqual(release.validate(self.root, f'v{value}'), (value, '- Shipped.\n'))
                self.assertEqual(release.release_channel(value), channel)
                self.assertEqual(release.github_outputs(value), f'version={value}\nchannel={channel}\nprerelease=true\n')
            self.set_version('1.0.0', previous=value)

    def test_stable_versions_publish_to_latest(self):
        self.assertEqual(release.github_outputs('1.0.0'), 'version=1.0.0\nchannel=latest\nprerelease=false\n')

    def test_rejects_unsupported_or_malformed_versions(self):
        for value in ['01.0.0', '1.0', '1.0.0-alpha', '1.0.0-alpha.01', '1.0.0-dev.1', '1.0.0-beta.1+build', '1.0.0-rc.1;echo bad']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                release.release_channel(value)

    def test_install_assets_pin_compose_and_follow_correct_quadlet_channel(self):
        sources = {
            'docker-compose.release.yml': 'image: ghcr.io/openwebide/openwebide:latest\n',
            'docker-compose.https.yml': 'image: ghcr.io/openwebide/openwebide:latest\n',
            'docker-compose.ssh.yml': 'services: {}\n',
            'deploy/openwebide.image': '[Image]\nImage=ghcr.io/openwebide/openwebide:latest\n',
            'deploy/openwebide.container': '[Container]\nImage=openwebide.image\nPull=newer\nAutoUpdate=registry\n',
            'docker/tailscale/serve-config.sh': '#!/bin/sh\necho config\n',
        }
        for name, content in sources.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        for value, channel in [('1.0.0', 'latest'), ('1.0.0-beta.2', 'beta')]:
            self.set_version(value)
            output = self.root / value
            release.prepare_install_files(self.root, output)
            with self.subTest(version=value):
                for name in ['docker-compose.yml', 'docker-compose.https.yml']:
                    self.assertIn(f'openwebide:v{value}', (output / name).read_text())
                self.assertIn(f'openwebide:{channel}', (output / 'openwebide.image').read_text())
                self.assertEqual((output / 'docker-compose.ssh.yml').read_text(), sources['docker-compose.ssh.yml'])
                self.assertIn('AutoUpdate=registry', (output / 'openwebide.container').read_text())
                with self.assertRaises(ValueError):
                    release.prepare_install_files(self.root, output)

    def test_publish_commands_do_not_promote_prereleases_to_latest(self):
        workflow = (release.ROOT / '.github/workflows/release.yml').read_text()
        publish = workflow.split('      - name: Publish multi-architecture image and release\n', 1)[1]
        script = textwrap.dedent(publish.split('        run: |\n', 1)[1])
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
        for value in ['1.0.0', '1.0.0-alpha.1', '1.0.0-beta.2', '1.0.0-rc.3']:
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
                self.assertEqual(writes, [f'{image}:v{value}', f"{image}:{outputs['channel']}"])
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
