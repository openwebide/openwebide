"""Exercise host updater primitives without touching a real deployment."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / 'bridge/src/host_admin/update_host.py'
spec = importlib.util.spec_from_file_location('update_host', HELPER)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class HostUpdates(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.directory = self.root / 'job'
        self.directory.mkdir()

    def test_deployment_edits_preserve_configuration_and_keep_private_backups(self):
        for prefix, suffix in [('OPENWEBIDE_IMAGE=', '.env'), ('Image=', '.image')]:
            with self.subTest(prefix=prefix):
                path = self.root / ('deployment' + suffix)
                original = f'# existing config\n{prefix}old-image\nKEEP=this-value\n'
                path.write_text(original)
                path.chmod(0o600)
                with patch.object(helper.time, 'sleep'), patch.object(helper, 'run') as run:
                    helper.execute(self.directory, [
                        {'argv': ['podman', 'pull', 'new-image']},
                        {'path': str(path), 'prefix': prefix, 'value': 'new-image', 'require_existing': True},
                        {'argv': ['systemctl', '--user', 'restart', 'openwebide.service']},
                    ])
                self.assertEqual(path.read_text(), original.replace('old-image', 'new-image'))
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
                self.assertEqual(run.call_count, 2)
                self.assertEqual(json.loads((self.directory / 'status.json').read_text())['phase'], 'succeeded')
                self.assertIn(original, [p.read_text() for p in self.directory.glob('backup-*')])

    def test_failed_pull_cannot_change_deployment_or_restart(self):
        path = self.root / 'deployment.env'
        path.write_text('OPENWEBIDE_IMAGE=old-image\n')
        with patch.object(helper.time, 'sleep'), patch.object(helper, 'run', side_effect=RuntimeError('pull failed')) as run:
            with self.assertRaises(RuntimeError):
                helper.execute(self.directory, [
                    {'argv': ['docker', 'pull', 'new-image']},
                    {'path': str(path), 'prefix': 'OPENWEBIDE_IMAGE=', 'value': 'new-image'},
                    {'argv': ['docker', 'compose', 'up', '-d']},
                ])
        self.assertEqual(path.read_text(), 'OPENWEBIDE_IMAGE=old-image\n')
        self.assertEqual(run.call_count, 1)

    def test_compose_image_variable_must_control_the_selected_service(self):
        for image, succeeds in [('approved-image', True), ('hardcoded-image', False)]:
            with self.subTest(image=image):
                path = self.root / 'compose.env'
                path.write_text('OPENWEBIDE_IMAGE=old-image\n')
                with patch.object(helper.time, 'sleep'), patch.object(helper, 'run', return_value=json.dumps({'services': {'app': {'image': image}}})):
                    plan = [
                        {'argv': ['docker', 'compose', 'config', '--format', 'json'], 'keys': ['services', 'app', 'image'], 'expected': 'approved-image'},
                        {'path': str(path), 'prefix': 'OPENWEBIDE_IMAGE=', 'value': 'approved-image'},
                    ]
                    if succeeds:
                        helper.execute(self.directory, plan)
                        self.assertIn('approved-image', path.read_text())
                    else:
                        with self.assertRaises(RuntimeError):
                            helper.execute(self.directory, plan)
                        self.assertEqual(path.read_text(), 'OPENWEBIDE_IMAGE=old-image\n')

    def test_launch_survives_initiator_exit_and_rejects_duplicate_identity(self):
        binary = self.root / 'bin'
        binary.mkdir()
        runner = binary / 'systemd-run'
        runner.write_text(f'#!{sys.executable}\nimport subprocess,sys\n'
                          'args=sys.argv[1:]\nstart=next(i for i,arg in enumerate(args) if arg.endswith("python3") or "python" in arg.rsplit("/",1)[-1])\n'
                          'subprocess.Popen(args[start:],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,start_new_session=True)\n')
        runner.chmod(0o755)
        env = os.environ | {'XDG_STATE_HOME': str(self.root / 'state'), 'PATH': str(binary) + os.pathsep + os.environ['PATH']}
        identity = 'a' * 32
        output = self.root / 'completed'
        request = {'id': identity, 'helper': HELPER.read_text(), 'plan': [{'argv': [sys.executable, '-c', 'from pathlib import Path; import sys; Path(sys.argv[1]).write_text("done")', str(output)]}]}
        result = subprocess.run([sys.executable, str(HELPER), 'start', json.dumps(request)], env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stdout)
        duplicate = subprocess.run([sys.executable, str(HELPER), 'start', json.dumps(request)], env=env, capture_output=True, text=True, timeout=5)
        self.assertNotEqual(duplicate.returncode, 0)
        deadline = time.monotonic() + 10
        while not output.exists() and time.monotonic() < deadline:
            time.sleep(.05)
        self.assertEqual(output.read_text(), 'done')
        receipt = self.root / 'state/openwebide/updates' / identity / 'status.json'
        self.assertEqual(json.loads(receipt.read_text())['phase'], 'succeeded')

    def test_invalid_job_identity_cannot_escape_host_state_directory(self):
        for identity in ['../escaped', 'g' * 32, 'a' * 31]:
            with self.assertRaises(ValueError):
                helper.job_directory(identity)


if __name__ == '__main__':
    unittest.main()
