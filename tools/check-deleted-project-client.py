#!/usr/bin/env python3
"""Built browser/WASI deletion after a real native command loses its settlement ACK.

The deterministic LSP peer, transport fault, Local OPFS correspondence and native
test home are fixtures. No outcome, cleanup receipt or deletion is fabricated.
The default host uses the shared CLI executor in a test home. The optional Linux
container uses the normal companion/CLI and shipped OS installation policy.
Neither establishes an optimized native/WASI distribution or physical permissions.
The optional account fault removes the sole guarded fixture user administratively
after real API project deletion; it does not add a public account-delete API.
"""
import argparse
from contextlib import ExitStack, suppress
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import runpy
import sqlite3
import subprocess
import tempfile
from threading import Event, Lock, Thread
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
CALLBACK = runpy.run_path(str(ROOT / 'tools/check-command-callback-client.py'))
RESOURCE = CALLBACK['RESOURCE']
Runtime, Browser = CALLBACK['Runtime'], CALLBACK['Browser']
script, wait, click = CALLBACK['script'], CALLBACK['wait'], CALLBACK['click']


class SettlementProxy:
    """Relay real requests; lose the exact reply that commits pending -> settled."""
    def __init__(self, runtime, project, output, callback_host='127.0.0.1'):
        self.runtime, self.project, self.output = runtime, project, output
        self.lock, self.lost = Lock(), Event()
        self.armed, self.blocked = False, False
        self.events = []
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                assert self.path.startswith('/api/')
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                with proxy.lock:
                    blocked = proxy.blocked
                if blocked:
                    status, response, content_type = 503, b'{"error":"Retained test interruption"}', 'application/json'
                else:
                    observed = self.path.startswith(('/api/language-commands/', '/api/workspace-transactions/'))
                    before = proxy.pending() if observed else None
                    headers = {key: value for key, value in self.headers.items()
                               if key.lower() not in ('host', 'content-length', 'connection')}
                    request = urllib.request.Request(runtime.url + self.path, data=body,
                                                     headers=headers, method='POST')
                    try:
                        reply = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=30)
                    except urllib.error.HTTPError as error:
                        reply = error
                    except urllib.error.URLError:
                        reply = None
                        status, response, content_type = 503, b'{"error":"Test upstream not ready"}', 'application/json'
                    if reply is not None:
                        with reply:
                            status, response, content_type = reply.code, reply.read(), reply.headers.get('Content-Type', 'application/json')
                    after = proxy.pending() if observed else None
                    with proxy.lock:
                        lose = (self.path.startswith('/api/language-commands/') and proxy.armed and status == 200 and before is not None
                                and before[0] == 1 and after is not None and after[:2] == [0, 1])
                        if lose:
                            proxy.armed, proxy.blocked = False, True
                        if observed:
                            proxy.events.append({'path': self.path, 'status': status,
                                'before': before, 'after': after, 'reply_lost': lose,
                                'reply_sha256': hashlib.sha256(response).hexdigest()})
                            output.write_text(json.dumps(proxy.events, indent=2) + '\n')
                    if lose:
                        # The backend reply is real and committed. End this
                        # connection without sending any HTTP response to native.
                        self.close_connection = True
                        proxy.lost.set()
                        return
                self.send_response(status)
                self.send_header('Content-Type', content_type)
                self.send_header('Content-Length', str(len(response)))
                self.end_headers()
                with suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(response)

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.url = f'http://{callback_host}:{self.server.server_port}/api'
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def pending(self):
        path = self.runtime.state / 'sqlite_db.db'
        if not path.exists():
            return None
        with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True) as connection:
            row = connection.execute('SELECT native_settlement_pending, command_cleanup_confirmed '
                'FROM language_actions WHERE project_id=? ORDER BY updated_at DESC LIMIT 1',
                (self.project,)).fetchone()
        return None if row is None else list(row)

    def resume(self):
        with self.lock:
            self.blocked = False

    def stop(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive()


class RecoveryHost(CALLBACK['IsolatedHost']):
    def __init__(self, *args):
        super().__init__(*args)
        # An open browser must reach the replacement companion at the same
        # address, as it does after a normal native daemon restart.
        self.listen_port = self.port

    def start(self):
        # The parent probe originally starts once. Retain the actual original
        # environment across this contract's separate native process lifetimes.
        previous = self.previous
        super().start()
        assert self.port == self.listen_port
        if previous is not None:
            self.previous = previous

    def administer(self, name, *args):
        result = subprocess.run([str(self.binary), '--recovery', str(self.home), *args],
                                cwd=ROOT, capture_output=True, timeout=30)
        (self.state / (name + '.stdout')).write_bytes(result.stdout)
        (self.state / (name + '.stderr')).write_bytes(result.stderr)
        assert result.returncode == 0, f'Native recovery {name} failed; see retained stderr'
        return json.loads(result.stdout)


def remove_fixture_account(runtime, user, state):
    """Decommission only this stopped fixture's account, with FK enforcement."""
    assert runtime.process is None
    database = runtime.state / 'sqlite_db.db'
    assert database.is_file() and database.resolve().parent == (state / 'spin').resolve()
    backup = state / 'before-account-removal.sqlite3'
    assert not backup.exists()

    def guard(connection):
        assert connection.execute('SELECT id,username FROM users').fetchall() == [(user, 'deleted-project')]
        assert connection.execute('SELECT count(*) FROM projects').fetchone()[0] == 0
        assert connection.execute('SELECT count(*) FROM sessions').fetchone()[0] == 0
        assert not connection.execute('PRAGMA foreign_key_check').fetchall()

    with sqlite3.connect(database) as connection:
        connection.execute('PRAGMA foreign_keys=ON')
        guard(connection)
        with sqlite3.connect(backup) as destination:
            connection.backup(destination)
        connection.execute('BEGIN IMMEDIATE')
        guard(connection)
        records_before = connection.execute('SELECT count(*) FROM plugin_records WHERE user_id=?', (user,)).fetchone()[0]
        assert records_before > 0, 'Missing real installed fixture records before account removal'
        removed = connection.execute('DELETE FROM users WHERE id=? AND username=?', (user, 'deleted-project'))
        assert removed.rowcount == 1
        assert connection.execute('SELECT count(*) FROM users').fetchone()[0] == 0
        for table in ['plugin_records', 'plugin_defaults', 'user_settings', 'workspace_transactions', 'language_command_settlements']:
            assert connection.execute('SELECT count(*) FROM ' + table + ' WHERE user_id=?', (user,)).fetchone()[0] == 0
        assert not connection.execute('PRAGMA foreign_key_check').fetchall()
    report = {'method': 'Administrative SQLite deletion after real API project deletion',
        'fixture_user': user, 'only_guarded_fixture_account_removed': True,
        'owned_projects_and_chat_sessions_absent_before_removal': True,
        'installed_records_before': records_before, 'installed_records_cascaded': True,
        'foreign_key_check_clean': True, 'before_database': str(backup),
        'public_account_deletion_API': False}
    (state / 'account-decommission.json').write_text(json.dumps(report, indent=2) + '\n')
    return report


def backend_writers(runtime, project, folder, output, blocked):
    """Real file API calls against a second project naming the same physical folder."""
    prefix = f'/api/projects/{project}/files/'
    records = json.loads(output.read_text()) if output.exists() else []

    def call(method, path, body=None, binary=False, expected=200):
        request = urllib.request.Request(runtime.url + prefix + path,
            data=body if binary else (None if body is None else json.dumps(body).encode()),
            headers={'x-openwebide': '1', 'Content-Type': 'application/octet-stream' if binary else 'application/json'},
            method=method)
        try:
            reply = runtime.client.open(request, timeout=30)
        except urllib.error.HTTPError as error:
            reply = error
        with reply:
            status, payload = reply.code, reply.read()
        records.append({'method': method, 'path': path, 'blocked': blocked,
            'status': status, 'expected': expected, 'reply_sha256': hashlib.sha256(payload).hexdigest()})
        output.write_text(json.dumps(records, indent=2) + '\n')
        output.with_name('backend-writer-files.json').write_text(json.dumps({
            path.name: {'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
            for path in folder.iterdir() if path.is_file()}, indent=2) + '\n')
        assert status == expected, f'Backend writer {method} {path}: expected {expected}, got {status}'

    if blocked:
        call('PUT', 'write?path=api-write.bin', b'\x00\xff\r\n', binary=True, expected=409)
        call('POST', 'create?path=api-dir&type=dir', expected=409)
        call('POST', 'copy', {'from': 'page.html', 'to': 'api-copy.html'}, expected=409)
        call('DELETE', 'delete?path=unrelated.html', expected=409)
        assert not any((folder / name).exists() for name in ['api-write.bin', 'api-dir', 'api-copy.html'])
    else:
        call('POST', 'create?path=api-dir&type=dir', expected=201)
        call('POST', 'create?path=api-dir/empty&type=file', expected=201)
        call('PUT', 'write?path=api-write.bin', b'\x00\xff\r\n', binary=True)
        assert (folder / 'api-write.bin').read_bytes() == b'\x00\xff\r\n'
        call('POST', 'copy', {'from': 'api-write.bin', 'to': 'api-dir/copied.bin'})
        assert (folder / 'api-dir/copied.bin').read_bytes() == b'\x00\xff\r\n'
        call('DELETE', 'delete?path=api-write.bin')
        call('DELETE', 'delete?path=api-dir')
        assert not (folder / 'api-write.bin').exists() and not (folder / 'api-dir').exists()


def check_mode(options, mode):
    state = options.evidence_dir / mode
    state.mkdir(mode=0o700)
    runtime = Runtime(state / 'spin', frontend=True,
        manifest=CALLBACK['manifest'](state, options.backend_wasm, options.frontend_dist))
    runtime.state.mkdir()
    # A fresh fixture account's first project has ID 1; validate it before arming.
    proxy = SettlementProxy(runtime, 1, state / 'transport.json',
        callback_host='host.docker.internal' if options.native_container_image else '127.0.0.1')
    project_directory = tempfile.TemporaryDirectory(prefix='deleted-project-', dir=ROOT.parent)
    if options.native_container_image:
        from language_recovery_container_support import ContainerRecoveryHost
        host = ContainerRecoveryHost(runtime, options.host_binary, [runtime.url],
            options.native_container_image, Path(project_directory.name))
    else:
        host = RecoveryHost(runtime, options.host_binary, [runtime.url])
    host.backend_url = proxy.url
    browser, mirror = None, CALLBACK['ProbeMirror'](runtime.url)
    account = None
    executable = state / 'bin/vscode-html-language-server'
    executable.parent.mkdir()
    executable.write_text('#!' + ('/usr/bin/python3' if options.native_container_image else os.sys.executable) + '\n' +
        (ROOT / 'tools/fixtures/workspace_edit_server.py').read_text().split('\n', 1)[1])
    executable.chmod(0o755)
    log = state / 'lsp.jsonl'
    previous = {key: os.environ.get(key) for key in ('PATH', 'OPENWEBIDE_RESOURCE_PROBE_LOG')}
    os.environ['PATH'] = str(executable.parent) + os.pathsep + os.environ['PATH']
    os.environ['OPENWEBIDE_RESOURCE_PROBE_LOG'] = str(log)
    try:
        print(mode + ': starting built app and compiled defaults', flush=True)
        host.start()
        original_pid = host.process.pid
        runtime.start()
        runtime.request('POST', '/api/auth/register', {'username': 'deleted-project',
            'password': 'disposable-deleted-project-fixture-password'}, expected=201)
        runtime.acknowledge_release()
        user = runtime.request('GET', '/api/auth/me')['user']['id']
        host.initialize_defaults()
        browser = Browser(state)
        browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
        for cookie in runtime.cookies:
            browser.call('POST', '/cookie', {'cookie': {'name': cookie.name,
                'value': cookie.value, 'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
        host.pair_browser(browser)
        trace = RESOURCE['EVIDENCE'] + '\nresourceProbe.journalProxy=' + json.dumps(proxy.url) + ';' + r'''
const commandFetch=window.fetch;
window.fetch=async function(...args){
 const request=args[0],url=String(request?.url??request),method=args[1]?.method??request?.method;
 const path=new URL(url,location.href).pathname;
 if(method==='POST'&&(path==='/plugins/languages/command'||path.endsWith('/language-commands/reconnect')||path==='/plugins/languages/command/reconnect')){
  const original=new Request(request instanceof Request?request.clone():request,args[1]);
  const body=await original.clone().json();
  if(body.journal) body.journal.api_url=resourceProbe.journalProxy;
  // Keep all callback candidates on the same real forwarding transport so a
  // valid browser reconnect cannot accidentally route around the lost reply.
  if(path.endsWith('/reconnect')) body.callback=resourceProbe.journalProxy;
  args=[new Request(original,{body:JSON.stringify(body)})];
 }
 return commandFetch.apply(this,args);
};
'''
        browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument',
            'params': {'source': trace}})
        with project_directory as project_path:
            folder, project, _files = RESOURCE['command_project'](
                runtime, browser, mirror, Path(project_path), mode, user)
            assert project['id'] == proxy.project
            endpoint = f"/api/projects/{project['id']}/language-actions"
            record = lambda: runtime.request('POST', endpoint, {'operation': 'load', 'id': None})
            # Folder proof/marker requests can still finish after editor readiness.
            # Capture the fixture baseline only after those initial files drain.
            wait('initial folder proof and marker files drained', lambda:
                {path.name for path in folder.iterdir() if path.is_file()} ==
                {'page.html', 'unrelated.html', 'package.json'})
            alias = None
            if options.check_backend_writers:
                alias_path = Path(project_path) / 'backend-writer-alias'
                alias_path.symlink_to(folder.name, target_is_directory=True)
                alias = runtime.request('POST', '/api/projects', {'name': 'Backend writer alias',
                    'mode': 'remote', 'path': alias_path.relative_to(ROOT.parent.parent).as_posix()}, expected=201)
                assert alias['id'] != project['id'], 'The writer alias must be a separate project'
                assert alias_path.samefile(folder)
                (state / 'backend-writer-project.json').write_text(json.dumps({
                    'original_project': project['id'], 'alias_project': alias['id'],
                    'distinct_paths_same_physical_folder': True}, indent=2) + '\n')
                backend_writers(runtime, alias['id'], folder, state / 'backend-writers.json', blocked=False)
            original_source = {path.name: path.read_bytes() for path in folder.iterdir() if path.is_file()}
            source_stages = {}

            def record_files(stage):
                def read():
                    try:
                        return ({path.name: path.read_bytes() for path in folder.iterdir() if path.is_file()},)
                    except FileNotFoundError:
                        return None
                files = wait('stable folder observation: ' + stage, read, seconds=5)[0]
                source_stages[stage] = {name: {'bytes': len(data),
                    'sha256': hashlib.sha256(data).hexdigest()} for name, data in files.items()}
                (state / 'source-stages.json').write_text(json.dumps(source_stages, indent=2) + '\n')

            record_files('before approval')
            RESOURCE['open_action'](browser, 'Run reviewed command only')
            wait('complete original command review', lambda: script(browser,
                "return document.querySelector('[role=dialog]')?.textContent.includes('fixture.execute')&&[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Apply edits and run command'&&!b.disabled);"))
            with proxy.lock:
                proxy.armed = True
            assert click(browser, 'Apply edits and run command')
            wait('real backend settlement committed and reply lost', proxy.lost.is_set, seconds=60)
            record_files('after lost settlement reply')
            if options.page_after_settlement == 'close':
                browser.call('POST', '/url', {'url': 'about:blank'})
            elif options.page_after_settlement == 'reload':
                # This is the same SPA fallback URL used by the earlier failing
                # fixture. Explicitly keep its real polling/reconnection alive.
                browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
                wait('reloaded application project', lambda: project['name'] in
                    browser.script('return document.body.innerText;'), seconds=60)
            record_files('before project deletion')
            action = record()
            assert action['phase'] == 'completed' and action['outcome'] is not None
            (state / 'original-command.json').write_text(json.dumps({
                'user': user, 'project': project['id'], 'transaction': action['approval']['id'],
                'phase': action['phase'], 'outcome': action['outcome']}, indent=2) + '\n')
            assert proxy.pending() == [0, 1]
            pending = wait('original native owner retained', lambda: CALLBACK['native'](host.home))
            assert len(pending['owners']) == len(pending['commands']) == 1
            assert pending['owners'][0]['origin'] == proxy.url
            ordinary = {'command': 'printf verified > after-retirement.txt', 'cwd': str(folder), 'timeout_seconds': 5}
            native_status, native_reply = host.request('/exec', ordinary)
            assert native_status == 400
            assert not (folder / 'after-retirement.txt').exists()
            if alias:
                (state / 'backend-writer-fence.json').write_text(json.dumps({
                    'ordinary_native_status': native_status,
                    'ordinary_native_reply_sha256': hashlib.sha256(native_reply).hexdigest(),
                    'native_owners': len(pending['owners']), 'native_commands': len(pending['commands']),
                    'backend_committed_settlement': proxy.pending(),
                    'native_output_absent': not (folder / 'after-retirement.txt').exists()}, indent=2) + '\n')
                backend_writers(runtime, alias['id'], folder, state / 'backend-writers.json', blocked=True)
                runtime.request('DELETE', f"/api/projects/{alias['id']}")
            runtime.request('DELETE', f"/api/projects/{project['id']}")
            runtime.request('GET', f"/api/projects/{project['id']}", expected=404)
            record_files('after project deletion')
            if options.decommission_account:
                runtime.stop()
                account = remove_fixture_account(runtime, user, state)
                runtime.start()
                runtime.request('GET', '/api/auth/me', expected=401)
                runtime.request('GET', f"/api/projects/{project['id']}", expected=401)
                account['old_authentication_rejected_after_restart'] = True
                (state / 'account-decommission.json').write_text(json.dumps(account, indent=2) + '\n')
                record_files('after account decommission')
                print(mode + ': fixture account removed; old authentication rejected by restarted backend', flush=True)
            print(mode + ': real command settled; project deleted with original native fence retained', flush=True)
            host.stop_process()
            record_files('after original native shutdown')
            proxy.resume()
            before = len(proxy.events)
            host.start()
            second_pid = host.process.pid
            assert second_pid != original_pid
            wait('fresh native callback reaches deleted original project', lambda:
                any(row['status'] != 200 for row in proxy.events[before:]), seconds=30)
            assert host.request('/exec', ordinary)[0] == 400
            assert not (folder / 'after-retirement.txt').exists()
            host.stop_process()
            record_files('after recovery native shutdown')
            inspected = host.administer('inspect-original', 'inspect')
            assert len(inspected['owners']) == 1 and not inspected['retired'] and not inspected['settlements']
            entry = inspected['owners'][0]
            assert entry['outcome'] == action['outcome'] and entry['transaction'] == action['approval']['id']
            assert entry['user'] == user and entry['project'] == project['id'] and entry['mode'] == mode
            assert entry['retirement_blocked'] is None
            args = ('retire', entry['id'], '--expected', entry['expected'], '--note',
                    'Reviewed the actual command and files after deleting this fixture project'
                    + (' and decommissioning its account' if account else ''))
            audit = host.administer('retire-original', *args)
            assert audit['original'] == entry
            assert host.administer('retire-again', *args) == audit
            expected = {'owners': [], 'settlements': [], 'retired': [audit]}
            host.start()
            final_pid = host.process.pid
            assert final_pid not in (original_pid, second_pid)
            status, body = host.request('/exec', ordinary)
            assert status == 200 and json.loads(body)['exit_code'] == 0
            assert (folder / 'after-retirement.txt').read_bytes() == b'verified'
            record_files('after ordinary writing')
            final_source = {path.name: path.read_bytes() for path in folder.iterdir()
                            if path.is_file() and path.name != 'after-retirement.txt'}
            fingerprint = lambda files: {name: {'bytes': len(data),
                'sha256': hashlib.sha256(data).hexdigest()} for name, data in files.items()}
            (state / 'source-comparison.json').write_text(json.dumps({
                'before': fingerprint(original_source), 'after': fingerprint(final_source)}, indent=2) + '\n')
            assert final_source == original_source, 'Original folder contents changed; see source-comparison.json'
            events = [json.loads(line) for line in log.read_text().splitlines()]
            assert sum(row.get('method') == 'workspace/executeCommand' for row in events) == 1
            host.stop_process()
            assert host.administer('inspect-final', 'inspect') == expected
            result = {'mode': mode, 'real_browser_command_approval': True,
                'page_after_settlement': options.page_after_settlement,
                'native_address_preserved': True,
                'account_decommissioning': account,
                'real_WASI_project_deletion': True, 'settlement_reply_lost_after_commit': True,
                'native_processes': [original_pid, second_pid, final_pid], 'single_command_dispatch': True,
                'original_outcome_preserved': True, 'missing_project_did_not_release_claim': True,
                'actual_ordinary_file_write_after_explicit_retirement': True,
                'shared_CLI_executor_test_home': not bool(options.native_container_image),
                'OS_default_CLI_in_this_sequence': bool(options.native_container_image),
                'native_container': getattr(host, 'observations', None),
                'backend_writer_gate_checked': options.check_backend_writers,
                'optimized_native_WASI_distribution': False, 'physical_folder_permissions': False,
                'whole_goal_complete': False}
            (state / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps(result), flush=True)
    finally:
        with ExitStack() as cleanup:
            cleanup.callback(CALLBACK['restore_environment'], previous)
            cleanup.callback(proxy.stop)
            cleanup.callback(mirror.stop)
            cleanup.callback(runtime.stop)
            cleanup.callback(host.stop)
            if browser is not None:
                cleanup.callback(browser.stop)
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
    parser.add_argument('--page-after-settlement', choices=('close', 'keep', 'reload'), default='close',
                        help='Close, keep or reload the approval page before deleting the project')
    parser.add_argument('--decommission-account', action='store_true',
                        help='Remove the sole guarded fixture account administratively after API project deletion')
    parser.add_argument('--native-container-image',
                        help='Run the normal Linux companion and recovery CLI in this image, with the shipped OS policy')
    parser.add_argument('--check-backend-writers', action='store_true',
                        help='Verify real backend file API calls share the native gate through another project alias')
    options = parser.parse_args()
    assert options.backend_wasm.is_file() and (options.frontend_dist / 'index.html').is_file()
    options.evidence_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    for mode in (options.mode,) if options.mode else ('remote', 'local'):
        check_mode(options, mode)


if __name__ == '__main__':
    main()
