#!/usr/bin/env python3
"""Real file API contracts for coordinated and explicitly direct deployments.

No models, language packages or browser fixtures are needed. The same API
contract runs through the native VFS and the directory-bound WASI primitive.
The container cases run the actual entrypoint with the companion disabled.
"""
import argparse
import json
import os
from pathlib import Path
import runpy
import secrets
import subprocess
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
DELETION = runpy.run_path(str(ROOT / 'tools/check-deleted-project-client.py'))
CALLBACK = DELETION['CALLBACK']
Runtime, wait = CALLBACK['Runtime'], CALLBACK['wait']


def docker(*args):
    reply = subprocess.run(['docker', *args], capture_output=True, timeout=45)
    assert reply.returncode == 0, reply.stderr.decode(errors='replace')[:1500]
    return reply.stdout + reply.stderr if args[0] == 'logs' else reply.stdout


class ContainerBackend(Runtime):
    def __init__(self, state, manifest, image, policy):
        super().__init__(state / 'spin', manifest=manifest)
        self.state.mkdir()
        self.workspace = state / 'workspace'
        self.workspace.mkdir()
        self.image, self.policy = image, policy
        self.container = 'openwebide-direct-files-' + secrets.token_hex(6)
        self.created = False

    def start(self):
        args = ['run', '--detach', '--name', self.container, '--workdir', '/app',
            '--publish', f'127.0.0.1:{self.port}:3000',
            '--mount', f'type=bind,src={self.state},dst=/app/.spin',
            '--mount', f'type=bind,src={self.workspace},dst=/workspace',
            '--mount', f'type=bind,src={self.manifest.parent},dst=/fixture,readonly',
            '--mount', f'type=bind,src={ROOT / "docker/entrypoint.sh"},dst=/usr/local/bin/openwebide-entrypoint,readonly',
            '--mount', f'type=bind,src={ROOT / "docker/ssh-init.sh"},dst=/usr/local/bin/openwebide-ssh-init,readonly',
            '--env', 'OPENWEBIDE_BRIDGE=0', '--env', 'OPENWEBIDE_APP_HOST=0.0.0.0',
            '--env', 'SPIN_VARIABLE_BRIDGE_SECRET=fixture-unavailable-native-secret']
        if self.policy is not None:
            args.extend(['--env', 'SPIN_VARIABLE_WORKSPACE_WRITE_POLICY=' + self.policy])
        args.extend(['--entrypoint', '/usr/local/bin/openwebide-entrypoint', self.image,
                     '--from', '/fixture/spin.toml', '--state-dir', '/app/.spin'])
        self.created = True
        docker(*args)

        def healthy():
            details = json.loads(docker('inspect', self.container))[0]['State']
            assert details['Running'] and not details['OOMKilled'], 'Backend container exited'
            try:
                self.request('GET', '/api/health', authenticated=False)
                return True
            except (urllib.error.URLError, ConnectionError, TimeoutError):
                return False
        wait('standalone container backend', healthy, seconds=90)

    def stop(self):
        if self.created:
            docker('stop', '--time', '10', self.container)
            (self.state.parent / 'container.log').write_bytes(docker('logs', self.container))
            details = json.loads(docker('inspect', self.container))[0]
            assert not details['State']['Running'] and not details['State']['OOMKilled']
            (self.state.parent / 'container.json').write_text(json.dumps({
                'id': details['Id'], 'image': details['Image'], 'state': details['State']}, indent=2) + '\n')
            self.created = False


def manifest(state, backend, container=False):
    text = (ROOT / 'spin.toml').read_text()
    text = text.replace('[[trigger.http]]\nroute = "/..."\ncomponent = "frontend"\n', '')
    text = text[:text.index('[component.frontend]')]
    source = '/fixture/backend.wasm' if container else str(backend.resolve())
    text = text.replace('target/wasm32-wasip2/release/openwebide_backend.wasm', source)
    text = text.replace('source = "../.."', 'source = ' + json.dumps('/workspace' if container else str(ROOT.parent.parent)))
    destination = state / 'fixture'
    destination.mkdir()
    if container:
        subprocess.run(['cp', '-cp', str(backend), str(destination / 'backend.wasm')], check=True)
    path = destination / 'spin.toml'
    path.write_text(text)
    return path


def file_request(runtime, project, path, method='GET', body=None):
    request = urllib.request.Request(runtime.url + f'/api/projects/{project}/files/' + path,
        data=body, method=method, headers={'x-openwebide': '1', 'Content-Type': 'application/octet-stream'})
    try:
        reply = runtime.client.open(request, timeout=30)
    except urllib.error.HTTPError as error:
        reply = error
    with reply:
        return reply.code, reply.read()


def check(options, name, policy, live_native=False, container=False, blocked=False):
    state = options.evidence_dir / name
    state.mkdir()
    runtime = (ContainerBackend(state, manifest(state, options.backend_wasm, True), options.container_image, policy)
        if container else Runtime(state / 'spin', manifest=manifest(state, options.backend_wasm)))
    if not container:
        runtime.state.mkdir()
        runtime.workspace = ROOT.parent.parent
    folder = (runtime.workspace / 'project') if container else state / 'project'
    folder.mkdir()
    for filename, contents in [('page.html', b'<p>source</p>\r\n'), ('unrelated.html', b'untouched\r\n')]:
        (folder / filename).write_bytes(contents)
    before = {p.name: p.read_bytes() for p in folder.iterdir()}
    keys = ['SPIN_VARIABLE_WORKSPACE_WRITE_POLICY', 'SPIN_VARIABLE_BRIDGE_URL', 'SPIN_VARIABLE_BRIDGE_SECRET']
    previous = {key: os.environ.get(key) for key in keys}
    host = None
    try:
        if not container:
            os.environ['SPIN_VARIABLE_WORKSPACE_WRITE_POLICY'] = policy
            os.environ['SPIN_VARIABLE_BRIDGE_URL'] = 'http://127.0.0.1:1'
            os.environ['SPIN_VARIABLE_BRIDGE_SECRET'] = 'fixture-unavailable-native-secret'
        if live_native:
            host = CALLBACK['IsolatedHost'](runtime, options.host_binary, [runtime.url])
            host.start()
        runtime.start()
        runtime.request('POST', '/api/auth/register', {'username': 'file-writer',
            'password': 'disposable-file-writer-fixture-password'}, expected=201)
        project = runtime.request('POST', '/api/projects', {'name': name, 'mode': 'remote',
            'path': folder.relative_to(runtime.workspace).as_posix()}, expected=201)
        DELETION['backend_writers'](runtime, project['id'], folder, state / 'operations.json', blocked)
        if not blocked:
            status, payload = file_request(runtime, project['id'], 'read?path=unrelated.html')
            assert status == 200 and json.loads(payload)['content'].encode() == before['unrelated.html']
            outside = folder.parent / 'outside'
            outside.mkdir()
            (folder / 'escape').symlink_to('../outside', target_is_directory=True)
            status, _ = file_request(runtime, project['id'], 'write?path=escape/never', 'PUT', b'never')
            assert status == 400 and not (outside / 'never').exists(), 'A mutation escaped the selected project'
            (folder / 'escape').unlink()
        assert {p.name: p.read_bytes() for p in folder.iterdir()} == before
        if container:
            programs = docker('top', runtime.container, '-eo', 'pid,comm').decode()
            assert 'openwebide-brid' not in programs
        result = {'case': name, 'success': True, 'explicit_policy': policy,
            'actual_entrypoint_bridge_disabled': container, 'native_live': live_native,
            'unreachable_native_did_not_fall_back': blocked, 'original_files_preserved': True,
            'project_escape_rejected': not blocked, 'full_CI_complete': False}
        (state / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result), flush=True)
    finally:
        runtime.stop()
        if host is not None:
            host.stop()
        CALLBACK['restore_environment'](previous)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend-wasm', type=Path, required=True)
    parser.add_argument('--host-binary', type=Path, required=True)
    parser.add_argument('--container-image')
    parser.add_argument('--evidence-dir', type=Path, required=True)
    options = parser.parse_args()
    options.evidence_dir.mkdir(parents=True, exist_ok=False)
    check(options, 'native', 'coordinated', live_native=True)
    check(options, 'native-unreachable', 'coordinated', blocked=True)
    check(options, 'direct', 'direct')
    if options.container_image:
        check(options, 'entrypoint-direct', None, container=True)
        check(options, 'entrypoint-required-override', 'coordinated', container=True, blocked=True)


if __name__ == '__main__':
    main()
