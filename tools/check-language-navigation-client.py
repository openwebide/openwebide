#!/usr/bin/env python3
"""Production navigation and independent diagnostics in both project modes.

Installs two server-only community packages from a disposable Git repository.
Only the LSP peer is deterministic; the production client/host paths are real.
Local OPFS/native correspondence does not establish physical folder permissions.
"""
import base64
import json
import os
from pathlib import Path
import runpy
import secrets
import socket
import subprocess
import sys
import tempfile
import time

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, click, close, command, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
SOURCE = '文😀 Item\r\nType Item\r\nImpl Item\r\nLast Item\r\n'
EVIDENCE = r'''
window.navigationEvidence={requests:[],errors:[]};
addEventListener('error',e=>navigationEvidence.errors.push(String(e.message)));
addEventListener('unhandledrejection',e=>navigationEvidence.errors.push(String(e.reason)));
const navigationFetch=window.fetch;
window.fetch=async function(...args){
 const url=String(args[0]?.url??args[0]);
 const query=/\/plugins\/languages\/query$/.test(new URL(url,location.href).pathname);
 let input;
 if(query){
  let body={};try{body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());}catch{}
  input=body.input??body.selection?.input;
 }
 const response=await navigationFetch.apply(this,args);
 if(query){
  try{const data=await response.clone().json();if(input?.query?.operation!=='synchronize'||data.error)
   navigationEvidence.requests.push({query:input?.query,path:input?.path,result:data.result,error:data.error});}catch{}
 }
 return response;
};
'''


def source(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def key(browser, *values):
    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'navigation', 'actions':
        [{'type': 'keyDown', 'value': value} for value in values]
        + [{'type': 'keyUp', 'value': value} for value in reversed(values)]}]})


def selection(browser):
    return script(browser, '''const input=document.querySelector('textarea[data-editor-path]');
        return [input.selectionStart,input.selectionEnd];''')


def word_range(row):
    text = SOURCE.replace('\r\n', '\n')
    prefix = '\n'.join(text.split('\n')[:row]) + ('\n' if row else '')
    start = len((prefix + text.split('\n')[row].split('Item')[0]).encode('utf-16-le')) // 2
    return [start, start + 4]


def destination(row):
    start = word_range(row)[0]
    return [start, start]


def position(browser, row=0):
    start, end = word_range(row)
    script(browser, '''const input=document.querySelector('textarea[data-editor-path]');
        input.focus();input.setSelectionRange(arguments[0],arguments[1]);
        input.dispatchEvent(new Event('select',{bubbles:true}));''', start, end)


def wait_problems(browser, count):
    wait('Code intelligence control', lambda: script(browser, '''
        const button=document.querySelector('button[aria-label="Code intelligence"]');
        if(!button||button.disabled)return false;
        if(button.getAttribute('aria-expanded')!=='true')button.click();return true;'''))
    wait(f'{count} provider problems', lambda: script(browser,
        "return [...document.querySelectorAll('button')].some(b=>b.textContent.trim()===arguments[0]);",
        f'Problems ({count})'))
    script(browser, "document.querySelector('button[aria-label=\"Code intelligence\"]').click();")


def wait_saved_draft(runtime, project, expected):
    def persisted():
        recovery = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
        entry = next(file for file in recovery['state']['files'] if file['path'] == 'main.notes')
        return base64.b64decode(entry['document']['text']).decode().replace('\r\n', '\n') == expected
    wait('exact draft persisted before reload', persisted)


def install(runtime, repository, commit, name):
    reference = {'repository': repository, 'commit': commit, 'path': name}
    progress = runtime.request('POST', '/api/plugins/preparation', {'action': 'start', 'source': reference})
    deadline = time.monotonic() + 60
    while progress['state'] != 'ready':
        assert progress['state'] not in ('failed', 'cancelled'), progress.get('error')
        assert time.monotonic() < deadline, 'Server-only source preparation timed out'
        time.sleep(.1)
        progress = runtime.request('POST', '/api/plugins/preparation', {'action': 'status', 'id': progress['id']})
    prepared = progress['prepared']
    assert prepared['source'] == reference
    package = runtime.request('POST', '/api/plugins/package', prepared)
    entries = runtime.request('POST', '/api/plugins', {'prepared': prepared, 'package': package,
        'approved_capabilities': [], 'revision': None})
    entry = next(entry for entry in entries if entry['prepared']['source'] == reference)
    return entry, package


def git_repository(state, flag):
    repository = state / 'repository'
    repository.mkdir()
    for name, role in [('navigation-probe', 'language'), ('analysis-probe', 'diagnostics')]:
        folder = repository / name
        folder.mkdir()
        manifest = {'schemaVersion': 1, 'publisher': 'probe', 'name': name, 'version': '0.1.0',
            'displayName': name, 'description': 'Disposable production integration fixture', 'license': 'MIT',
            'compatibility': {'pluginApi': 9}, 'contributions': {'skills': [], 'languages': [{
                'id': 'notes', 'name': 'Notes', 'extensions': ['notes'], 'features': {'languageServer': {
                    'command': sys.executable, 'args': [str(ROOT / 'tools/fixtures/navigation_server.py'), role, str(flag)],
                    'rootMarkers': ['.git'], 'role': role}}}]}}
        (folder / 'plugin.json').write_text(json.dumps(manifest))
    subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(repository)], check=True)
    subprocess.run(['git', '-C', str(repository), 'add', '.'], check=True)
    subprocess.run(['git', '-C', str(repository), '-c', 'user.name=Test', '-c', 'user.email=test@example.org',
                    'commit', '--quiet', '-m', 'Production language fixtures'], check=True)
    commit = subprocess.check_output(['git', '-C', str(repository), 'rev-parse', 'HEAD'], text=True).strip()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    daemon = subprocess.Popen(['git', 'daemon', '--export-all', '--reuseaddr', '--listen=127.0.0.1',
        f'--port={port}', '--base-path=' + str(repository), str(repository)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            assert daemon.poll() is None, 'Disposable Git service exited'
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    return daemon, f'git://127.0.0.1:{port}/', commit
            except OSError:
                time.sleep(.05)
        raise AssertionError('Disposable Git service did not become ready')
    except BaseException:
        daemon.terminate()
        daemon.wait(timeout=10)
        raise


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-navigation-') as state_path, \
            tempfile.TemporaryDirectory(prefix='navigation-project-', dir=ROOT.parent) as project_path:
        state, project_root = Path(state_path), Path(project_path)
        flag, log = state / 'unavailable', state / 'lsp.jsonl'
        previous = os.environ.get('OPENWEBIDE_NAVIGATION_PROBE_LOG')
        os.environ['OPENWEBIDE_NAVIGATION_PROBE_LOG'] = str(log)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        host, browser, daemon = ExecutionHost(runtime), None, None
        mirror = ProbeMirror(runtime.url)
        try:
            daemon, repository, commit = git_repository(state, flag)
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'nav-' + secrets.token_hex(6),
                'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            host.initialize_defaults()
            primary, primary_package = install(runtime, repository, commit, 'navigation-probe')
            analyzer, analyzer_package = install(runtime, repository, commit, 'analysis-probe')
            print('Independent public server-only packages installed', flush=True)
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value,
                    'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument', 'params': {'source': EVIDENCE}})
            for mode in ('remote', 'local'):
                folder = project_root / mode
                folder.mkdir()
                (folder / 'main.notes').write_bytes(SOURCE.encode())
                subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name': 'Navigation ' + mode,
                    'mode': mode, 'path': relative if mode == 'remote' else folder.name}, expected=201)
                endpoint = f"/api/projects/{project['id']}/plugins"
                for installation, package in [(primary, primary_package), (analyzer, analyzer_package)]:
                    bindings = runtime.request('GET', endpoint)
                    binding = next((entry for entry in bindings
                        if entry['prepared']['source'] == installation['prepared']['source']), None)
                    runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
                        'installation_revision': installation['revision'],
                        'revision': binding['revision'] if binding else None})
                if mode == 'local':
                    local_handle(browser, project['id'], user, folder.name, {'main.notes': SOURCE})
                    mirror.install(browser, project['id'], folder)
                encoded = base64.b64encode(SOURCE.encode()).decode()
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0,
                    'state': {'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None},
                    'selected': 'main.notes', 'files': [{'path': 'main.notes', 'document': {'text': encoded,
                        'saved': encoded, 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []},
                        'scroll': {'top': 0, 'left': 0}, 'read_only': False}]}})
                for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
                    runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait_problems(browser, 3)
                for label, row in [('Go to type definition', 1), ('Go to implementation', 2)]:
                    position(browser)
                    command(browser, label)
                    wait(label + ' result', lambda: script(browser,
                        "return !!document.querySelector('[role=dialog] button.ui-dropdown-item');"))
                    script(browser, "document.querySelector('[role=dialog] button.ui-dropdown-item').click();")
                    wait(label + ' UTF16 selection', lambda: selection(browser) == destination(row))
                command(browser, 'Document symbols')
                wait('outline filter', lambda: script(browser,
                    "return !!document.querySelector('input[aria-label=\"Filter symbols and locations\"]');"))
                script(browser, "const input=document.querySelector('input[aria-label=\"Filter symbols and locations\"]');input.value='Implementation';input.dispatchEvent(new Event('input',{bubbles:true}));")
                wait('filtered outline', lambda: script(browser,
                    "return [...document.querySelectorAll('[role=dialog] button.ui-dropdown-item')].map(n=>n.textContent);") == ['Implementation Item'])
                close(browser)
                command(browser, 'Find workspace symbol…')
                wait('workspace symbol query', lambda: script(browser, "return !!document.querySelector('[role=dialog] input[type=search]');"))
                script(browser, "const input=document.querySelector('[role=dialog] input[type=search]');input.value='Item';input.dispatchEvent(new Event('input',{bubbles:true}));")
                assert click(browser, 'Find symbols')
                wait('lazy symbol', lambda: click(browser, 'Lazy Item'))
                wait('resolved location', lambda: script(browser,
                    "return [...document.querySelectorAll('[role=dialog] button.ui-dropdown-item')].some(b=>b.textContent.trim().endsWith(':4:6'));"))
                script(browser, "document.querySelector('[role=dialog] button.ui-dropdown-item').click();")
                wait('resolved symbol UTF16 selection', lambda: selection(browser) == destination(3))
                wait_problems(browser, 3)
                position(browser)
                key(browser, '\ue038')  # F8: next, ordered across providers, wrapping.
                wait('next diagnostic', lambda: selection(browser) == destination(2))
                key(browser, '\ue038')
                wait('last diagnostic', lambda: selection(browser) == destination(3))
                key(browser, '\ue038')
                wait('wrapped diagnostic', lambda: selection(browser) == destination(0))
                key(browser, '\ue008', '\ue038')
                wait('reverse wrapped diagnostic', lambda: selection(browser) == destination(3))
                print(mode + ': navigation and diagnostic traversal verified', flush=True)
                command(browser, 'Problems (3)')
                wait('independent provider attribution', lambda: script(browser,
                    "return [...document.querySelectorAll('[role=dialog] section[aria-label]')].some(s=>s.getAttribute('aria-label').includes('analysis-probe')&&s.textContent.includes('Analyzer note'));"))
                assert script(browser, '''const section=[...document.querySelectorAll('[role=dialog] section[aria-label]')].find(s=>s.getAttribute('aria-label').includes('analysis-probe'));
                    const button=[...section.querySelectorAll('button')].find(b=>b.textContent.trim()==='Find fixes');button.click();return true;''')
                wait('owning provider fix', lambda: click(browser, 'Fix analyzer note'))
                wait('review analyzer edit', lambda: click(browser, 'Apply to editor tabs'))
                wait('analyzer fix staged', lambda: 'Impl Fixed' in source(browser))
                close(browser)
                wait_saved_draft(runtime, project, source(browser))
                # Disable only the analysis binding, then fail its executable on
                # re-enable. The primary service and plain editing remain usable.
                binding = next(entry for entry in runtime.request('GET', endpoint)
                    if entry['prepared']['manifest']['name'] == 'analysis-probe')
                changed = runtime.request('POST', endpoint, {'action': 'disable', 'id': binding['id'], 'revision': binding['revision']})
                disabled = next(entry for entry in changed if entry['id'] == binding['id'])
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait_problems(browser, 2)
                flag.touch()
                # The flag simulates a startup dependency failure; ensure no
                # previously warm process can mask it on either transport.
                host.stop_process()
                host.start()
                current_analyzer = next(entry for entry in runtime.request('GET', '/api/plugins')
                    if entry['prepared']['source'] == analyzer['prepared']['source'])
                runtime.request('POST', endpoint, {'action': 'enable', 'package': analyzer_package,
                    'installation_revision': current_analyzer['revision'], 'revision': disabled['revision']})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait('primary survives analyzer startup failure', lambda: bool(source(browser)))
                command(browser, 'Problems (2)')
                wait('explicit analyzer failure', lambda: script(browser,
                    "return [...document.querySelectorAll('[role=dialog] section[aria-label]')].some(s=>s.getAttribute('aria-label').includes('analysis-probe')&&s.textContent.includes('Cannot initialize language server'));"))
                close(browser)
                text = source(browser)
                script(browser, "const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(input.value.length,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
                insert(browser, 'plain editing')
                wait('typing during provider failure', lambda: source(browser) == text + 'plain editing')
                wait_saved_draft(runtime, project, text + 'plain editing')
                flag.unlink()
                host.stop_process()
                host.start()
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait_problems(browser, 3)
                assert source(browser) == text + 'plain editing'
                assert not script(browser, 'return navigationEvidence.errors;')
                assert (folder / 'main.notes').read_bytes() == SOURCE.encode(), 'Navigation or review saved a file'
                events = [json.loads(line) for line in log.read_text().splitlines()]
                assert any(event['role'] == 'diagnostics' and event['method'] == 'textDocument/codeAction' for event in events)
                assert not any(event['role'] == 'language' and event['method'] == 'textDocument/codeAction' for event in events)
                print(json.dumps({'mode': mode, 'public_server_only_packages': True, 'type_definition': True,
                    'implementation': True, 'filtered_outline': True, 'lazy_workspace_symbol': True,
                    'utf16_diagnostic_traversal_and_wrap': True, 'independent_analysis_fixes': True,
                    'disable_failure_recovery': True, 'plain_editing': True, 'source_files_unchanged': True}), flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "const input=document.querySelector('textarea[data-editor-path]');return {selection:input&&[input.selectionStart,input.selectionEnd],dataset:input&&{...input.dataset},source:input?.value,evidence:window.navigationEvidence&&{...navigationEvidence,requests:navigationEvidence.requests.slice(-8)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(n=>n.textContent.slice(0,1800)),alerts:[...document.querySelectorAll('[role=alert]')].map(n=>n.textContent.slice(0,1000))};")), flush=True)
                    recovery = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                    print(json.dumps({'recovered_selections': [file['document']['selections'] for file in recovery['state']['files']]}), flush=True)
                except Exception:
                    pass
            for name, path in [('runtime', runtime.state / 'runtime.log'), ('bridge', host.state / 'bridge.log'), ('lsp', log)]:
                if path.exists():
                    Path('/tmp/openwebide-navigation-' + name + '.log').write_bytes(path.read_bytes())
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
            if previous is None:
                os.environ.pop('OPENWEBIDE_NAVIGATION_PROBE_LOG', None)
            else:
                os.environ['OPENWEBIDE_NAVIGATION_PROBE_LOG'] = previous


if __name__ == '__main__':
    check()
