#!/usr/bin/env python3
"""Public API 9 configuration edits and attached commands in the production UI.

Local OPFS changes are mirrored to a disposable native folder. This establishes
the adapter contract, not physical browser-folder permission behavior.
"""
import base64
import json
from pathlib import Path
import runpy
import secrets
import socket
import subprocess
import sys
import tempfile

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, click, command, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
SOURCE, AFTER = 'text 文😀\r\n', 'next 文😀\r\n'
EVIDENCE = r'''
window.projectInputProbe={queries:[]};
const inputFetch=window.fetch;
window.fetch=async function(...args){
 const path=new URL(String(args[0]?.url??args[0]),location.href).pathname;
 let trace=null;
 if(/\/plugins\/languages\/query$/.test(path)){
  const body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());
  const input=body.input??body.selection?.input;
  trace={operation:input?.query?.operation};projectInputProbe.queries.push(trace);
 }
 const response=await inputFetch.apply(this,args);
 if(trace){const data=await response.clone().json();Object.assign(trace,{status:response.status,error:data.error});}
 return response;
};
'''


def git(folder, *arguments):
    return subprocess.check_output(['git', '-C', str(folder), *arguments], text=True).strip()


def fixture(state):
    repository = state / 'repository'
    package = repository / 'inputs-probe'
    package.mkdir(parents=True)
    manifest = {'schemaVersion': 1, 'publisher': 'probe', 'name': 'inputs-probe', 'version': '0.1.0',
        'displayName': 'Project inputs probe', 'description': 'Disposable public API configuration fixture',
        'license': 'MIT', 'compatibility': {'pluginApi': 9}, 'contributions': {'skills': [], 'languages': [{
            'id': 'notes', 'name': 'Notes', 'extensions': ['notes'], 'features': {'languageServer': {
                'command': sys.executable, 'args': [str(ROOT / 'tools/fixtures/commands_server.py'), 'inputs'],
                'rootMarkers': ['.git'], 'projectInputs': {'files': ['settings*.json']}}}}]}}
    (package / 'plugin.json').write_text(json.dumps(manifest))
    git(repository, 'init', '--quiet', '--initial-branch=main', '--template=')
    git(repository, 'add', '.')
    git(repository, '-c', 'user.name=Test', '-c', 'user.email=test@example.org',
        '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Disposable project input fixture')
    commit = git(repository, 'rev-parse', 'HEAD')
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    daemon = subprocess.Popen(['git', 'daemon', '--export-all', '--reuseaddr', '--listen=127.0.0.1',
        f'--port={port}', '--base-path=' + str(repository), str(repository)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    def ready():
        assert daemon.poll() is None, 'Disposable Git service exited'
        try:
            with socket.create_connection(('127.0.0.1', port), timeout=.1):
                return True
        except OSError:
            return False
    try:
        wait('disposable package repository', ready)
    except BaseException:
        daemon.terminate()
        daemon.wait(timeout=10)
        raise
    return daemon, {'repository': f'git://127.0.0.1:{port}/', 'commit': commit, 'path': 'inputs-probe'}


def install(runtime, reference):
    progress = runtime.request('POST', '/api/plugins/preparation', {'action': 'start', 'source': reference})
    def prepared():
        nonlocal progress
        assert progress['state'] not in ('failed', 'cancelled'), progress.get('error')
        if progress['state'] == 'ready':
            return progress['prepared']
        progress = runtime.request('POST', '/api/plugins/preparation', {'action': 'status', 'id': progress['id']})
        return None
    prepared = wait('public API 9 package preparation', prepared)
    package = runtime.request('POST', '/api/plugins/package', prepared)
    entries = runtime.request('POST', '/api/plugins', {'prepared': prepared, 'package': package,
        'approved_capabilities': [], 'revision': None})
    return next(entry for entry in entries if entry['prepared']['source'] == reference), package


def source(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-inputs-') as state_path, \
            tempfile.TemporaryDirectory(prefix='inputs-project-', dir=ROOT.parent) as project_path:
        state, parent = Path(state_path), Path(project_path)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        host, browser, daemon = ExecutionHost(runtime), None, None
        mirror = ProbeMirror(runtime.url)
        try:
            daemon, reference = fixture(state)
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'inputs-' + secrets.token_hex(6),
                'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            host.initialize_defaults()
            installation, package = install(runtime, reference)
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value,
                    'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument',
                'params': {'source': EVIDENCE}})
            for mode, stale in [('remote', True), ('remote', False), ('local', True), ('local', False)]:
                folder = parent / (mode + ('-changed' if stale else '-applied'))
                folder.mkdir()
                files = {'main.notes': SOURCE, 'settings-old.json': '{"value":1}'}
                for path, text in files.items():
                    (folder / path).write_bytes(text.encode())
                git(folder, 'init', '--quiet', '--initial-branch=main', '--template=')
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name': 'Inputs ' + mode, 'mode': mode,
                    'path': relative if mode == 'remote' else folder.name}, expected=201)
                bindings = runtime.request('GET', f"/api/projects/{project['id']}/plugins")
                binding = next((entry for entry in bindings if entry['prepared']['source'] == reference), None)
                runtime.request('POST', f"/api/projects/{project['id']}/plugins", {'action': 'enable',
                    'package': package, 'installation_revision': installation['revision'],
                    'revision': binding['revision'] if binding else None})
                if mode == 'local':
                    local_handle(browser, project['id'], user, folder.name, files)
                    mirror.install(browser, project['id'], folder,
                        files=('settings-old.json', 'settings-new.json', 'settings-external.json'),
                        native_view=True)
                encoded = base64.b64encode(SOURCE.encode()).decode()
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': {
                    'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None},
                    'selected': 'main.notes', 'files': [{'path': 'main.notes', 'document': {'text': encoded,
                        'saved': encoded, 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []},
                        'scroll': {'top': 0, 'left': 0}, 'read_only': False}]}})
                for key, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
                    runtime.request('PUT', '/api/settings', {'key': key, 'value': value})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait(mode + ' restored editor', lambda: source(browser) == SOURCE.replace('\r\n', '\n'))
                command(browser, 'Code actions')
                wait('configuration action selected', lambda: click(browser, 'Edit then run'))
                wait('complete configuration and command review', lambda: script(browser,
                    "return document.querySelector('[role=dialog]')?.textContent.includes('settings-new.json')&&document.querySelector('[role=dialog]')?.textContent.includes('fixture.apply');"))
                assert (folder / 'settings-old.json').read_text() == files['settings-old.json']
                assert not (folder / 'settings-new.json').exists() and not (folder / 'executions').exists()
                if stale:
                    # This matching dependency is outside the action's file plan.
                    # An ordinary affected-path comparison cannot detect it.
                    if mode == 'remote':
                        (folder / 'settings-external.json').write_text('{"changed":true}')
                    else:
                        assert browser.call('POST', '/execute/async', {'script': '''
                            const name=arguments[0],done=arguments[arguments.length-1];
                            (async()=>{const root=await(await navigator.storage.getDirectory()).getDirectoryHandle(name);
                              const file=await root.getFileHandle('settings-external.json',{create:true});
                              const writer=await file.createWritable();await writer.write('{"changed":true}');await writer.close();return true;
                            })().then(done,error=>done({error:String(error)}));
                        ''', 'args': [folder.name]}) is True
                        assert (folder / 'settings-external.json').read_text() == '{"changed":true}'
                wait('explicit whole-action approval', lambda: click(browser, 'Apply edits and run command'))
                endpoint = f"/api/projects/{project['id']}/language-actions"
                if stale:
                    def rejected():
                        return script(browser, """
                            return projectInputProbe.queries.some(query=>query.operation==='validate_analysis'&&query.status>=400)
                              &&(document.body.textContent.includes('project inputs')||document.body.textContent.includes('original language service'));
                        """)
                    wait('stale configuration review rejected before intent', rejected)
                    assert runtime.request('POST', endpoint, {'operation': 'load', 'id': None}) is None
                    assert runtime.request('GET', f"/api/projects/{project['id']}/workspace-transactions") is None
                    assert script(browser, "return projectInputProbe.queries.every(query=>query.operation!=='prepare_action');")
                    assert not (folder / 'executions').exists() and not (folder / 'settings-new.json').exists()
                    assert (folder / 'settings-old.json').read_text() == files['settings-old.json']
                    assert source(browser) == SOURCE.replace('\r\n', '\n')
                    print(json.dumps({'mode': mode, 'public_api_9': True,
                        'changed_dependency_before_apply_rejects_whole_action': True,
                        'native_validation_rejects_before_any_action_or_transaction': True,
                        'no_filesystem_primitive_or_command': True}), flush=True)
                    continue
                def completed():
                    record = runtime.request('POST', endpoint, {'operation': 'load', 'id': None})
                    return record and record['phase'] == 'completed'
                wait('configuration edit and command completed', completed)
                assert script(browser, "return projectInputProbe.queries.some(query=>query.operation==='validate_analysis'&&query.status===200);")
                assert (folder / 'executions').read_text().splitlines() == ['executed']
                assert not (folder / 'settings-old.json').exists()
                assert (folder / 'settings-new.json').read_text() == files['settings-old.json']
                assert (folder / 'main.notes').read_bytes().decode() == SOURCE, 'Command implicitly saved a draft'
                assert source(browser) == AFTER.replace('\r\n', '\n')
                recovery = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                retained = next(file for file in recovery['state']['files'] if file['path'] == 'main.notes')
                assert base64.b64decode(retained['document']['text']).decode() == AFTER
                calls = (folder / 'calls').read_text().splitlines()
                assert calls.count('workspace/executeCommand') == 1
                assert calls[:calls.index('workspace/executeCommand')].count('initialize') == 1, \
                    'Configuration edit replaced the reviewed service before its command'
                print(json.dumps({'mode': mode, 'public_api_9': True, 'review_precedes_mutation': True,
                    'configuration_move_and_unsaved_edit': True, 'original_service_command_once': True,
                    'draft_saved_to_recovery_not_disk': True}), flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "return {source:document.querySelector('textarea[data-editor-path]')?.value,dialogs:[...document.querySelectorAll('[role=dialog]')].map(n=>n.textContent.slice(0,2500)),notices:[...document.querySelectorAll('[role=alert],[role=status]')].map(n=>n.textContent.slice(0,1500)),body:document.body.textContent.slice(-14000),probe:window.projectInputProbe};")), flush=True)
                except Exception:
                    pass
            for name, path in [('runtime', runtime.state / 'runtime.log'), ('bridge', host.state / 'bridge.log')]:
                if path.exists():
                    Path('/tmp/openwebide-project-input-client-' + name + '.log').write_bytes(path.read_bytes())
            raise
        finally:
            if browser is not None:
                browser.stop()
            host.stop()
            runtime.stop()
            mirror.stop()
            if daemon is not None:
                daemon.terminate()
                daemon.wait(timeout=10)


if __name__ == '__main__':
    check()
