#!/usr/bin/env python3
"""Closed-source warming through installed public plugins in both production editors.

Requires current release frontend/backend and the bridge with bundled defaults.
Local OPFS/native correspondence establishes adapter behavior, not physical
folder permissions. Only the LSP peer and folder correspondence are fixtures.
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
import time
import urllib.error
import urllib.request

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, caret, close, command, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
INSTALL = runpy.run_path(str(ROOT / 'tools/check-project-inputs-client.py'))['install']
SOURCE = 'Item main 文😀\n'
DRAFT = 'Item retained unsaved 文😀\n'
EVIDENCE = r'''
window.prefetchEvidence={queries:[],errors:[],reads:[]};
addEventListener('error',e=>prefetchEvidence.errors.push(String(e.message)));
addEventListener('unhandledrejection',e=>prefetchEvidence.errors.push(String(e.reason)));
const originalPrefetchFetch=window.fetch;
window.fetch=async function(...args){
 const path=new URL(String(args[0]?.url??args[0]),location.href).pathname;
 let trace;
 if(/\/plugins\/languages\/query$/.test(path)){
  const body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());
  const input=body.input??body.selection?.input;
  trace={input};prefetchEvidence.queries.push(trace);
 }
 if(path.endsWith('/files/bounded'))prefetchEvidence.reads.push(String(args[0]?.url??args[0]));
 try{
  const response=await originalPrefetchFetch.apply(this,args);
  if(trace){const body=await response.clone().json();Object.assign(trace,{status:response.status,response:body});}
  return response;
 }catch(error){if(trace)trace.failure=String(error);throw error;}
};
'''


def fixture(state):
    repository = state / 'repository'
    package = repository / 'prefetch-probe'
    package.mkdir(parents=True)
    manifest = {'schemaVersion': 1, 'publisher': 'probe', 'name': 'prefetch-probe', 'version': '0.1.0',
        'displayName': 'Prefetch probe', 'description': 'Disposable public language service fixture',
        'license': 'MIT', 'compatibility': {'pluginApi': 9}, 'contributions': {'skills': [], 'languages': [{
            'id': 'notes', 'name': 'Notes', 'extensions': ['notes'], 'features': {'languageServer': {
                'command': sys.executable, 'args': [str(ROOT / 'tools/fixtures/prefetch_server.py')],
                'rootMarkers': ['.git']}}}]}}
    (package / 'plugin.json').write_text(json.dumps(manifest))
    subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(repository)], check=True)
    subprocess.run(['git', '-C', str(repository), 'add', '.'], check=True)
    subprocess.run(['git', '-C', str(repository), '-c', 'user.name=Test', '-c', 'user.email=test@example.org',
        '-c', 'commit.gpgsign=false', 'commit', '--quiet', '-m', 'Disposable prefetch fixture'], check=True)
    commit = subprocess.check_output(['git', '-C', str(repository), 'rev-parse', 'HEAD'], text=True).strip()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    daemon = subprocess.Popen(['git', 'daemon', '--export-all', '--reuseaddr', '--listen=127.0.0.1',
        f'--port={port}', '--base-path=' + str(repository), str(repository)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        def ready():
            assert daemon.poll() is None, 'Disposable package repository exited'
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    return True
            except OSError:
                return False
        wait('disposable package repository', ready)
    except BaseException:
        daemon.terminate()
        daemon.wait(timeout=10)
        raise
    return daemon, {'repository': f'git://127.0.0.1:{port}/', 'commit': commit, 'path': 'prefetch-probe'}


def bounded_contract(runtime, project, folder):
    """Use the actual authenticated WASI route, including exact byte boundaries."""
    endpoint = f"/api/projects/{project['id']}/files/bounded?"
    text = '文😀'
    (folder / 'boundary.notes').write_bytes(text.encode())
    for path, maximum, expected, payload in [
        ('boundary.notes', 7, 200, text.encode()),
        ('boundary.notes', 6, 400, None),
        ('missing.notes', 7, 404, None),
        ('../escape.notes', 7, 400, None),
    ]:
        request = urllib.request.Request(runtime.url + endpoint + f'path={path}&max_bytes={maximum}',
            headers={'x-openwebide': '1'})
        try:
            response = runtime.client.open(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            assert response.status == expected, (path, maximum, response.status)
            data = response.read()
            if payload is not None:
                assert data == payload
                assert response.headers.get('Cache-Control') == 'no-store'
    runtime.request('GET', endpoint + 'path=boundary.notes&max_bytes=7', expected=401, authenticated=False)


def source(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def queries(browser):
    return script(browser, 'return prefetchEvidence.queries;')


def warmed(browser, path, text, revision):
    return [query for query in queries(browser) if query['input']['path'] == path
        and query['input']['source'] == text and query['input']['query'].get('close') is True
        and query['input']['revision'] == revision
        and query.get('status') == 200 and not query.get('response', {}).get('error')]


def synchronized_revision(browser, text, after):
    revisions = [query['input']['revision'] for query in queries(browser)[after:]
        if query['input']['path'] == 'main.notes' and query['input']['source'] == text
        and query['input']['query']['operation'] == 'synchronize' and query.get('status') == 200]
    return max(revisions, default=None)


def disk_write(browser, mode, folder, path, text):
    (folder / path).write_bytes(text.encode())
    if mode == 'local':
        result = browser.call('POST', '/execute/async', {'script': '''
            const [name,path,text]=arguments,done=arguments[arguments.length-1];
            (async()=>{const root=await(await navigator.storage.getDirectory()).getDirectoryHandle(name);
              const file=await root.getFileHandle(path);const writer=await file.createWritable();
              await writer.write(text);await writer.close();return true;
            })().then(done,error=>done({error:String(error)}));
        ''', 'args': [folder.name, path, text]})
        assert result is True, result


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-prefetch-') as state_path, \
            tempfile.TemporaryDirectory(prefix='prefetch-project-', dir=ROOT.parent) as project_path:
        state, parent = Path(state_path), Path(project_path)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        host, browser, daemon = ExecutionHost(runtime), None, None
        mirror = ProbeMirror(runtime.url)
        try:
            daemon, reference = fixture(state)
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'prefetch-' + secrets.token_hex(6),
                'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            host.initialize_defaults()
            installation, package = INSTALL(runtime, reference)
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value,
                    'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument',
                'params': {'source': EVIDENCE}})
            for mode in ('remote', 'local'):
                folder = parent / mode
                folder.mkdir()
                files = {'main.notes': SOURCE, 'draft.notes': 'Item saved draft\n',
                    'related.notes': 'Item old disk\n', 'recent.notes': 'Item old recent\n'}
                for path, text in files.items():
                    (folder / path).write_bytes(text.encode())
                subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name': 'Prefetch ' + mode,
                    'mode': mode, 'path': relative if mode == 'remote' else folder.name}, expected=201)
                endpoint = f"/api/projects/{project['id']}/plugins"
                bindings = runtime.request('GET', endpoint)
                binding = next((entry for entry in bindings if entry['prepared']['source'] == reference), None)
                runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
                    'installation_revision': installation['revision'], 'revision': binding['revision'] if binding else None})
                if mode == 'remote':
                    bounded_contract(runtime, project, folder)
                    print('Authenticated WASI bounded reads: exact UTF-8 bytes, oversize, missing, escape and unsigned rejection passed', flush=True)
                else:
                    local_handle(browser, project['id'], user, folder.name, files)
                    mirror.install(browser, project['id'], folder)
                recovered = []
                for path in ('main.notes', 'draft.notes', 'recent.notes'):
                    text = DRAFT if path == 'draft.notes' else files[path]
                    recovered.append({'path': path, 'document': {
                        'text': base64.b64encode(text.encode()).decode(),
                        'saved': base64.b64encode(files[path].encode()).decode(),
                        'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []},
                        'scroll': {'top': 0, 'left': 0}, 'read_only': False})
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0,
                    'state': {'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None},
                        'selected': 'main.notes', 'files': recovered}})
                for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id']),
                        'language_prefetch': 'false'}.items():
                    runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait(mode + ' restored editor', lambda: source(browser) == SOURCE)
                command(browser, 'Symbol information')
                wait('foreground response with prefetch off', lambda: any(
                    query['input']['query']['operation'] == 'hover' and query.get('status') == 200 for query in queries(browser)))
                close(browser)
                time.sleep(1.5)
                assert not any(query['input']['query']['operation'] == 'prefetch' for query in queries(browser))
                runtime.request('PUT', '/api/settings', {'key': 'language_prefetch', 'value': 'true'})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait(mode + ' opt-in editor', lambda: source(browser) == SOURCE)
                # Register a closed recent file without selecting an unopened target.
                wait('recent open-source observation', lambda: any(
                    query['input']['path'] == 'recent.notes'
                    and query['input']['query']['operation'] == 'prefetch'
                    and query.get('status') == 200 for query in queries(browser)))
                assert script(browser, "const b=document.querySelector('[aria-label=\"Close recent.notes\"]');if(!b)return false;b.click();return true;")
                recent = 'Item external recent 文😀\n'
                disk_write(browser, mode, folder, 'recent.notes', recent)
                # A source edit starts a fresh bounded context after closing.
                caret(browser, 'Item', select=True)
                before = len(queries(browser))
                insert(browser, 'Main')
                revision = wait('current edited source synchronized', lambda:
                    synchronized_revision(browser, SOURCE.replace('Item', 'Main'), before))
                wait('recent closed-file warming', lambda: len(warmed(browser, 'recent.notes', recent, revision)) == 2)
                command(browser, 'Go to definition')
                wait('related target returned', lambda: any(query['input']['query']['operation'] == 'definition'
                    and query.get('status') == 200 for query in queries(browser)))
                fresh = 'Item fresh related 文😀\n'
                disk_write(browser, mode, folder, 'related.notes', fresh)
                close(browser)
                # Preserve the hint through another edit and force a fresh source read.
                caret(browser, 'Main', select=True)
                before = len(queries(browser))
                insert(browser, 'Item')
                revision = wait('current related-source context synchronized', lambda:
                    synchronized_revision(browser, SOURCE, before))
                wait('unopened related-file warming', lambda: len(warmed(browser, 'related.notes', fresh, revision)) == 2)
                assert source(browser) == SOURCE
                assert not script(browser, "return !!document.querySelector('[data-editor-tab=\"related.notes\"]');")
                assert not script(browser, "return [...document.querySelectorAll('[role=dialog]')].length;")
                command(browser, 'Symbol information')
                def clean_sources():
                    hover = [query for query in queries(browser) if query['input']['query']['operation'] == 'hover'
                        and query.get('status') == 200][-1:]
                    if not hover:
                        return False
                    text = json.dumps(hover[0].get('response', {}).get('result'))
                    return 'draft.notes' in text and 'retained unsaved' in text and 'related.notes' not in text and 'recent.notes' not in text
                wait('foreground service keeps drafts and retires temporary sources', clean_sources)
                close(browser)
                events = [json.loads(line) for line in (folder / 'calls').read_text().splitlines()]
                assert sum(event['method'] == 'initialize' for event in events) == 1, 'Warming replaced the original service'
                for path in ('related.notes', 'recent.notes'):
                    assert any(event['method'] == 'textDocument/didClose'
                        and event['params']['textDocument']['uri'].endswith('/' + path) for event in events)
                assert (folder / 'main.notes').read_bytes() == SOURCE.encode()
                assert (folder / 'draft.notes').read_bytes() == files['draft.notes'].encode()
                assert not script(browser, 'return prefetchEvidence.errors;')
                if mode == 'remote':
                    assert script(browser, 'return prefetchEvidence.reads.length;') > 0
                print(json.dumps({'mode': mode, 'installed_public_package': True, 'default_off': True,
                    'fresh_unopened_related_source': True, 'fresh_recently_closed_source': True,
                    'temporary_sources_retired': True, 'original_service_and_drafts_preserved': True,
                    'no_result_dialog_or_file_write': True}), flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "return {source:document.querySelector('textarea[data-editor-path]')?.value,evidence:prefetchEvidence,dialogs:[...document.querySelectorAll('[role=dialog]')].map(n=>n.textContent),alerts:[...document.querySelectorAll('[role=alert]')].map(n=>n.textContent)};")), flush=True)
                except Exception:
                    pass
            for name, path in [('runtime', runtime.state / 'runtime.log'), ('bridge', host.state / 'bridge.log')]:
                if path.exists():
                    Path('/tmp/openwebide-prefetch-' + name + '.log').write_bytes(path.read_bytes())
            raise
        finally:
            if browser is not None:
                browser.stop()
            mirror.stop()
            runtime.stop()
            host.stop()
            if daemon is not None:
                daemon.terminate()
                daemon.wait(timeout=10)


if __name__ == '__main__':
    check()
