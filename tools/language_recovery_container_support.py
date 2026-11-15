"""Docker process/filesystem primitives for the shared command recovery check.

Only the supplied disposable project and state directories are writable mounts.
The actual normal bridge and shipped root-owned installation policy select the
namespace. Browser, backend, command orchestration and assertions are shared with
the host-native check; this adapter adds no language or recovery policy.
"""
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import urllib.error
import urllib.request

from language_host_support import ExecutionHost
from language_server_client_support import wait

ROOT = Path(__file__).resolve().parent.parent


def docker(*args, timeout=45):
    result = subprocess.run(['docker', *args], capture_output=True, timeout=timeout)
    assert result.returncode == 0, result.stderr.decode(errors='replace')[:1500]
    return result.stdout


class ContainerRecoveryHost(ExecutionHost):
    def __init__(self, runtime, binary, origins, image, project):
        super().__init__(runtime)
        self.binary, self.origins = binary.resolve(strict=True), origins
        self.image = json.loads(docker('image', 'inspect', image))[0]['Id']
        self.project = project.resolve(strict=True)
        self.data = self.state / 'data'
        self.data.mkdir(mode=0o700)
        self.home = self.data / 'native-principal'
        self.prefix = 'openwebide-command-recovery-' + secrets.token_hex(6)
        self.container = None
        self.observations = {'image': self.image, 'binary_sha256': hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            'policy_sha256': hashlib.sha256((ROOT / 'docker/native-recovery/0.json').read_bytes()).hexdigest(),
            'normal_entry_point': True, 'processes': [], 'administration': []}

    def mounts(self):
        return ['--mount', f'type=bind,src={self.binary},dst=/usr/local/bin/openwebide-bridge,readonly',
                '--mount', f'type=bind,src={ROOT / "docker/native-recovery/0.json"},dst=/etc/openwebide/native-recovery/0.json,readonly',
                '--mount', f'type=bind,src={self.data},dst=/app/.spin',
                '--mount', f'type=bind,src={self.state.parent.parent},dst={self.state.parent.parent}',
                '--mount', f'type=bind,src={self.project},dst={self.project}']

    def start(self):
        from types import SimpleNamespace
        assert self.process is None
        if self.previous is None:
            self.previous = {key: os.environ.get(key) for key in ('SPIN_VARIABLE_BRIDGE_URL', 'SPIN_VARIABLE_BRIDGE_SECRET')}
        os.environ['SPIN_VARIABLE_BRIDGE_URL'] = self.url
        os.environ['SPIN_VARIABLE_BRIDGE_SECRET'] = self.secret
        name = self.prefix + '-native-' + str(len(self.observations['processes']))
        # Set the owned name before starting so partial startup is still cleaned up.
        self.container = name
        args = ['run', '--detach', '--init', '--name', name,
                '--publish', f'127.0.0.1:{self.port}:3001', *self.mounts(),
                '--env', 'OPENWEBIDE_PLUGIN_DIR=' + str(self.state / 'plugins'),
                '--env', 'XDG_CACHE_HOME=' + str(self.state / 'cache'),
                '--env', 'PATH=' + str(self.state.parent.parent / 'bin') + ':/usr/local/bin:/usr/bin:/bin',
                '--env', 'OPENWEBIDE_RESOURCE_PROBE_LOG=' + os.environ['OPENWEBIDE_RESOURCE_PROBE_LOG'],
                '--entrypoint', '/usr/local/bin/openwebide-bridge', self.image,
                '--host', '0.0.0.0', '--port', '3001', '--workspace', str(ROOT.parent.parent),
                '--backend-url', self.backend_url, '--secret-file', str(self.secret_file), '--token', self.pairing]
        for origin in self.origins:
            args.extend(['--allowed-origin', origin])
        docker(*args)
        details = json.loads(docker('inspect', name))[0]
        rows = docker('top', name, '-eo', 'pid,comm').decode().splitlines()
        native_pids = [int(fields[0]) for row in rows if len(fields := row.split()) == 2
                       and fields[0].isdigit() and fields[1].startswith('openwebide-brid')]
        assert len(native_pids) == 1, 'Expected one normal companion process before service startup'
        self.process = SimpleNamespace(pid=native_pids[0])
        self.observations['processes'].append({'container': name, 'native_pid': self.process.pid,
            'container_init_pid': details['State']['Pid'], 'container_id': details['Id']})
        wait('normal container companion health', lambda: self.request('/health', authenticated=False)[0] == 200, seconds=45)

    def request(self, path, body=None, authenticated=True):
        headers = {'Content-Type': 'application/json'}
        if authenticated:
            headers['Authorization'] = 'Bearer ' + self.secret
        request = urllib.request.Request(self.url + path,
            data=None if body is None else json.dumps(body).encode(), headers=headers)
        try:
            reply = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=10)
        except urllib.error.HTTPError as error:
            reply = error
        except urllib.error.URLError:
            if path == '/health':
                return 503, b''
            raise
        with reply:
            return reply.code, reply.read()

    def stop_process(self):
        if self.container is None:
            return
        name = self.container
        docker('stop', '--time', '15', name, timeout=40)
        details = json.loads(docker('inspect', name))[0]['State']
        assert not details['Running'] and not details['OOMKilled']
        (self.state / (name + '.log')).write_bytes(docker('logs', name))
        self.container, self.process = None, None

    def administer(self, name, *args):
        assert self.process is None
        container = self.prefix + '-' + name
        result = subprocess.run(['docker', 'run', '--network', 'none', '--name', container,
            *self.mounts(), '--entrypoint', '/usr/local/bin/openwebide-bridge', self.image,
            'recovery', *args], capture_output=True, timeout=45)
        (self.state / (name + '.stdout')).write_bytes(result.stdout)
        (self.state / (name + '.stderr')).write_bytes(result.stderr)
        self.observations['administration'].append({'container': container, 'arguments': ['recovery', *args],
                                                   'returncode': result.returncode})
        assert result.returncode == 0, f'Normal recovery CLI {name} failed; see retained stderr'
        return json.loads(result.stdout)
