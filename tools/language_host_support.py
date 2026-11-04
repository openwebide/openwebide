"""Disposable real execution host for production editor browser checks.

Only transport setup lives here; language policy, permissions and results remain
in the built app/bridge/plugin. Uses the caller's isolated Spin runtime and files.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
DEFAULTS = json.loads((ROOT / 'plugins/bundled.json').read_text())


class ExecutionHost:
    def __init__(self, runtime):
        self.runtime = runtime
        self.state = Path(runtime.state) / 'execution-host'
        self.state.mkdir(parents=True, exist_ok=True)
        target = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')).expanduser().resolve()
        self.binary = target / 'release/openwebide-bridge'
        assert self.binary.is_file(), 'Build the release bridge with compiled bundled-defaults first'
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            self.port = listener.getsockname()[1]
        self.url = f'http://127.0.0.1:{self.port}'
        self.pairing = secrets.token_urlsafe(32)
        self.secret = secrets.token_urlsafe(32)
        self.secret_file = self.state / 'secret'
        self.secret_file.write_text(self.secret)
        self.process = None
        self.log = None
        self.previous = None

    def start(self):
        assert self.process is None
        if self.previous is None:
            self.previous = {key: os.environ.get(key) for key in ('SPIN_VARIABLE_BRIDGE_URL', 'SPIN_VARIABLE_BRIDGE_SECRET')}
        os.environ['SPIN_VARIABLE_BRIDGE_URL'] = self.url
        os.environ['SPIN_VARIABLE_BRIDGE_SECRET'] = self.secret
        self.log = (self.state / 'bridge.log').open('ab')
        child_env = {**os.environ, 'OPENWEBIDE_PLUGIN_DIR': str(self.state / 'plugins'),
                     'XDG_DATA_HOME': str(self.state / 'data'), 'XDG_CACHE_HOME': str(self.state / 'cache'),
                     'XDG_CONFIG_HOME': str(self.state / 'config')}
        self.process = subprocess.Popen([str(self.binary), '--host', '127.0.0.1', '--port', str(self.port),
            '--workspace', str(ROOT.parent.parent), '--backend-url', self.runtime.url + '/api',
            '--secret-file', str(self.secret_file), '--token', self.pairing, '--allowed-origin', self.runtime.url],
            cwd=ROOT, env=child_env, stdout=self.log, stderr=subprocess.STDOUT)
        client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            assert self.process.poll() is None, 'Isolated execution host exited'
            try:
                with client.open(self.url + '/health', timeout=1) as response:
                    if response.status == 200:
                        return
            except (urllib.error.URLError, TimeoutError):
                time.sleep(.1)
        raise AssertionError('Isolated execution host did not become ready')

    def stop_process(self):
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
            self.process = None
        if self.log is not None:
            self.log.close()
            self.log = None

    def stop(self):
        self.stop_process()
        if self.previous is not None:
            for key, value in self.previous.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
            self.previous = None

    def initialize_defaults(self):
        # The same per-source bound used by the offline default-package check.
        request = urllib.request.Request(self.runtime.url + '/api/plugins', headers={'x-openwebide': '1'}, method='GET')
        with self.runtime.client.open(request, timeout=len(DEFAULTS['paths']) * 120) as response:
            installations = json.load(response)
        assert len(installations) == len(DEFAULTS['paths']), 'Compiled bundled defaults did not initialize'
        self.runtime.request('PUT', '/api/settings', {'key': 'bridge_url', 'value': self.url.replace('http:', 'ws:', 1)})
        return installations

    def pair_browser(self, browser):
        assert browser.script('return location.origin;') == self.runtime.url
        stored = browser.call('POST', '/execute/async', {'script': '''
            const token = arguments[0], done = arguments[arguments.length - 1];
            const open = indexedDB.open('openwebide', 1);
            open.onupgradeneeded = () => open.result.createObjectStore('directories');
            open.onerror = () => done(false);
            open.onsuccess = () => {
                const database = open.result;
                const transaction = database.transaction('directories', 'readwrite');
                transaction.objectStore('directories').put(token, 'bridge_pairing_token');
                transaction.oncomplete = () => { database.close(); done(true); };
                transaction.onerror = () => { database.close(); done(false); };
            };
        ''', 'args': [self.pairing]})
        assert stored, 'Pairing token not stored in isolated browser'
