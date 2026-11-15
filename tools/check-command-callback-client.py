#!/usr/bin/env python3
"""Built browser -> WASI backend -> native command callback reconnection.

Only the LSP peer, lost Start transport, isolated native namespace and initial
Local OPFS/native correspondence are fixtures. Reconnect requests and replies
are never mocked. The new origin has no folder handle or picker substitution.
This does not establish physical permissions or installed-PWA behavior.
"""
import argparse
from contextlib import ExitStack, suppress
import hashlib
import json
import os
from pathlib import Path
import runpy
import select
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, click, script, wait

ROOT = Path(__file__).resolve().parent.parent
RESOURCE = runpy.run_path(str(ROOT / 'tools/check-workspace-transactions-client.py'))
Runtime, Browser = RESOURCE['Runtime'], RESOURCE['Browser']
TRACE = RESOURCE['EVIDENCE'] + r'''
resourceProbe.callbacks=[];
const callbackFetch=window.fetch;
window.fetch=async function(...args){
 const request=args[0],url=String(request?.url??request),path=new URL(url,location.href).pathname;
 const method=args[1]?.method??request?.method;
 if(method!=='POST'||!path.endsWith('/language-commands/reconnect')&&!path.endsWith('/plugins/languages/command/reconnect'))
   return callbackFetch.apply(this,args);
 const body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await request.clone().text());
 const trace={path,body};resourceProbe.callbacks.push(trace);
 const response=await callbackFetch.apply(this,args);
 trace.status=response.status;trace.changed=await response.clone().json();return response;
};
'''


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def digest(data):
    return hashlib.sha256(data).hexdigest()


class IsolatedHost(ExecutionHost):
    """Same native HTTP server; only its private namespace is test-selected."""
    def __init__(self, runtime, binary, origins):
        super().__init__(runtime)
        self.binary, self.origins = binary.resolve(), origins
        self.home = self.state / 'native-home'
        self.home.mkdir(mode=0o700)
        self.secret, self.pairing = 'dummy_secret', 'paired-token'

    def start(self):
        assert self.process is None and self.binary.is_file()
        self.previous = {key: os.environ.get(key) for key in
                         ('SPIN_VARIABLE_BRIDGE_URL', 'SPIN_VARIABLE_BRIDGE_SECRET')}
        self.log = (self.state / 'bridge.log').open('ab')
        env = {**os.environ, 'OPENWEBIDE_PLUGIN_DIR': str(self.state / 'plugins'),
               'XDG_CACHE_HOME': str(self.state / 'cache')}
        arguments = [str(self.binary)]
        if getattr(self, 'listen_port', None) is not None:
            arguments.extend(['--port', str(self.listen_port)])
        self.process = subprocess.Popen(arguments + [str(ROOT.parent.parent),
            str(self.home), self.backend_url, *self.origins], cwd=ROOT, env=env,
            stdout=subprocess.PIPE, stderr=self.log)
        assert select.select([self.process.stdout], [], [], 30)[0], 'Native host readiness timed out'
        self.port = int(self.process.stdout.readline())
        self.url = f'http://127.0.0.1:{self.port}'
        os.environ['SPIN_VARIABLE_BRIDGE_URL'] = self.url
        os.environ['SPIN_VARIABLE_BRIDGE_SECRET'] = self.secret
        wait('native host health', lambda: self.request('/health', authenticated=False)[0] == 200, seconds=30)

    def request(self, path, body=None, authenticated=True):
        headers = {'Content-Type': 'application/json'}
        if authenticated:
            headers['Authorization'] = 'Bearer ' + self.secret
        request = urllib.request.Request(self.url + path,
            data=None if body is None else json.dumps(body).encode(), headers=headers)
        try:
            response = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=10)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            return response.code, response.read()


def native(home):
    folder = home / '.openwebide-native-recovery'
    try:
        head = (folder / 'recovery-head.json').read_bytes()
        data = (folder / 'recovery.sqlite3').read_bytes()
        if head != (folder / 'recovery-head.json').read_bytes():
            return None
        header = json.loads(head)
        if header['phase'] != 'committed' or header['digest'] != digest(data):
            return None
        with sqlite3.connect(':memory:') as connection:
            connection.deserialize(data)
            key, = connection.execute('SELECT key FROM recovery_namespace WHERE id=1').fetchone()
            owners = [json.loads(row[0]) for row in connection.execute('SELECT record FROM recovery_owners')]
            commands = [json.loads(row[0]) for row in connection.execute('SELECT record FROM recovery_commands')]
            has_acknowledgements, = connection.execute("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='recovery_command_settlements'").fetchone()
            acknowledgements = (connection.execute('SELECT count(*) FROM recovery_command_settlements').fetchone()[0]
                                if has_acknowledgements else 0)
            return {'key': key, 'owners': owners, 'commands': commands, 'acknowledgements': acknowledgements}
    except (OSError, sqlite3.Error, ValueError, KeyError):
        return None


def manifest(state, backend, frontend):
    text = (ROOT / 'spin.toml').read_text()
    for old, new in [
        ('"target/wasm32-wasip2/release/openwebide_backend.wasm"', json.dumps(str(backend.resolve()))),
        ('source = "../.."', 'source = ' + json.dumps(str(ROOT.parent.parent))),
        ('source = "frontend/dist"', 'source = ' + json.dumps(str(frontend.resolve()))),
    ]:
        assert text.count(old) == 1, old
        text = text.replace(old, new)
    path = state / 'spin.toml'
    path.write_text(text)
    return path


def no_execution(log):
    if not log.is_file():
        return False
    entries = [json.loads(line) for line in log.read_text().splitlines()]
    return (any(entry.get('method') == 'textDocument/codeAction' for entry in entries)
            and all(entry.get('method') != 'workspace/executeCommand' for entry in entries))


def restore_environment(previous):
    for key, value in previous.items():
        if value is None:
            os.environ.pop(key, None)
        else:
            os.environ[key] = value


def check_mode(options, mode):
    state = options.evidence_dir / mode
    state.mkdir(mode=0o700)
    runtime = Runtime(state / 'spin', frontend=True,
                      manifest=manifest(state, options.backend_wasm, options.frontend_dist))
    runtime.state.mkdir()
    old_url, next_port = runtime.url, port()
    while next_port == runtime.port:
        next_port = port()
    new_url = f'http://127.0.0.1:{next_port}'
    host = IsolatedHost(runtime, options.host_binary, [old_url, new_url])
    browser, mirror = None, ProbeMirror(old_url)
    executable = state / 'bin/vscode-html-language-server'
    executable.parent.mkdir()
    executable.write_text('#!' + os.sys.executable + '\n' +
        (ROOT / 'tools/fixtures/workspace_edit_server.py').read_text().split('\n', 1)[1])
    executable.chmod(0o755)
    log = state / 'lsp.jsonl'
    previous = {key: os.environ.get(key) for key in ('PATH', 'OPENWEBIDE_RESOURCE_PROBE_LOG')}
    os.environ['PATH'] = str(executable.parent) + os.pathsep + os.environ['PATH']
    os.environ['OPENWEBIDE_RESOURCE_PROBE_LOG'] = str(log)
    try:
        print(mode + ': starting built application and bundled defaults', flush=True)
        host.start()
        native_pid = host.process.pid
        runtime.start()
        runtime.request('POST', '/api/auth/register', {'username': 'callback',
            'password': 'disposable-callback-fixture-password'}, expected=201)
        runtime.acknowledge_release()
        user = runtime.request('GET', '/api/auth/me')['user']['id']
        host.initialize_defaults()
        print(mode + ': defaults ready; opening editor', flush=True)
        browser = Browser(state)
        browser.call('POST', '/url', {'url': old_url + '/offline.html'})
        for cookie in runtime.cookies:
            browser.call('POST', '/cookie', {'cookie': {'name': cookie.name,
                'value': cookie.value, 'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
        host.pair_browser(browser)
        browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument',
            'params': {'source': TRACE}})
        with tempfile.TemporaryDirectory(prefix='callback-project-', dir=ROOT.parent) as project_path:
            folder, project, _files = RESOURCE['command_project'](
                runtime, browser, mirror, Path(project_path), mode, user)
            project_id = project['id']
            endpoint = f'/api/projects/{project_id}/language-actions'
            record = lambda: runtime.request('POST', endpoint, {'operation': 'load', 'id': None})
            script(browser, 'resourceProbe.pauseCommand=true;')
            RESOURCE['open_action'](browser, 'Run command with lost dispatch')
            wait('command review', lambda: script(browser,
                "return [...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Apply edits and run command'&&!b.disabled);"))
            assert click(browser, 'Apply edits and run command')
            wait('original command held before Start', lambda: script(browser,
                'return resourceProbe.commandPaused;') and (record() or {}).get('phase') == 'running')
            initial = wait('original native reservation', lambda: native(host.home))
            assert len(initial['owners']) == len(initial['commands']) == 1
            assert initial['owners'][0]['origin'] == old_url + '/api'
            assert host.request('/exec', {'command': 'true', 'cwd': str(folder), 'timeout_seconds': 5})[0] == 400
            assert no_execution(log)
            print(mode + ': original command retained without dispatch; moving backend', flush=True)
            original_source = {path.name: path.read_bytes() for path in folder.iterdir() if path.is_file()}
            if mirror.script_id is not None:
                browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.removeScriptToEvaluateOnNewDocument',
                    'params': {'identifier': mirror.script_id}})
                mirror.script_id = None
            runtime.stop()
            runtime.port, runtime.url = next_port, new_url
            runtime.start()
            browser.call('POST', '/url', {'url': new_url + '/offline.html'})
            host.pair_browser(browser)
            browser.call('POST', '/url', {'url': new_url + '/'})
            moved = wait(mode + ' browser-driven native callback move', lambda:
                (value if value and len(value['owners']) == 1 and
                 value['owners'][0]['origin'] == new_url + '/api' else None)
                if (value := native(host.home)) else None)
            expected = json.loads(json.dumps(initial))
            expected['owners'][0]['origin'] = new_url + '/api'
            assert moved == expected, 'Reconnection changed original command authority or state'
            print(mode + ': current browser verified the new native callback', flush=True)
            callbacks = wait('successful real reconnect transport', lambda: [entry for entry in
                script(browser, 'return resourceProbe.callbacks;') if entry.get('status') == 200
                and entry.get('changed') is True])
            route = (f'/api/projects/{project_id}/language-commands/reconnect' if mode == 'remote'
                     else '/plugins/languages/command/reconnect')
            assert any(entry['path'] == route and entry['body'] ==
                {'user': user, 'project': project_id, 'callback': new_url + '/api'} for entry in callbacks)
            assert host.process.pid == native_pid and host.process.poll() is None
            assert host.request('/exec', {'command': 'true', 'cwd': str(folder), 'timeout_seconds': 5})[0] == 400
            if mode == 'local':
                keys = browser.call('POST', '/execute/async', {'script': '''
                    const done=arguments[arguments.length-1],open=indexedDB.open('openwebide',1);
                    open.onsuccess=()=>{const db=open.result,tx=db.transaction('directories');
                      const request=tx.objectStore('directories').getAllKeys();
                      request.onsuccess=()=>{db.close();done(request.result);};};
                ''', 'args': []})
                assert keys == ['bridge_pairing_token'], 'New origin unexpectedly acquired folder access'
            wait('original Stop control', lambda: click(browser, 'Stop pending command'))
            wait('original no-dispatch rejection', lambda: (record() or {}).get('phase') == 'rejected')
            assert no_execution(log)
            wait('manual original acknowledgement', lambda: click(browser, 'Acknowledge reviewed outcome'))
            wait('original acknowledged result', lambda: (record() or {}).get('phase') == 'acknowledged')
            final = wait('native owner and metadata retirement', lambda:
                (value if value and not value['owners'] and not value['commands'] and
                 value['acknowledgements'] == 0 else None) if (value := native(host.home)) else None)
            assert final['key'] == initial['key']
            status, body = host.request('/exec', {'command': 'true', 'cwd': str(folder), 'timeout_seconds': 5})
            assert status == 200, 'Ordinary native writer did not resume'
            assert json.loads(body)['exit_code'] == 0
            assert {path.name: path.read_bytes() for path in folder.iterdir() if path.is_file()} == original_source
            assert no_execution(log)
            assert not script(browser, 'return resourceProbe.errors;')
            result = {'mode': mode, 'native_pid_unchanged': True, 'old_backend': old_url,
                'new_backend': new_url, 'reconnect_route': route, 'reconnect_requests': callbacks,
                'native_authority_preserved': True, 'fenced_until_acknowledgement': True,
                'ordinary_writer_resumed': True, 'no_command_dispatch': True,
                'new_origin_folder_access': False if mode == 'local' else None,
                'physical_folder_permissions_verified': False, 'installed_PWA_verified': False}
            (state / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps(result), flush=True)
    finally:
        with ExitStack() as cleanup:
            cleanup.callback(restore_environment, previous)
            cleanup.callback(mirror.stop)
            cleanup.callback(runtime.stop)
            cleanup.callback(host.stop)
            if browser is not None:
                cleanup.callback(browser.stop)
            if browser is not None:
                # Diagnostic failure must not prevent process cleanup.
                with suppress(Exception):
                    (state / 'last-browser-text.txt').write_text(browser.script('return document.body.innerText;'))
                    (state / 'last-browser-evidence.json').write_text(json.dumps(browser.script('return window.resourceProbe;'), indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--backend-wasm', type=Path, required=True)
    parser.add_argument('--frontend-dist', type=Path, default=ROOT / 'frontend/dist')
    parser.add_argument('--host-binary', type=Path, required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    parser.add_argument('--mode', choices=('remote', 'local'))
    options = parser.parse_args()
    assert options.backend_wasm.is_file() and (options.frontend_dist / 'index.html').is_file()
    options.evidence_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    for mode in (options.mode,) if options.mode else ('remote', 'local'):
        check_mode(options, mode)


if __name__ == '__main__':
    main()
