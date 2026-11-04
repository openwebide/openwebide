#!/usr/bin/env python3
"""Check the built app against real native host language components in both modes.

Build the frontend, WASI backend, and release native bridge with bundled-defaults first;
OPENWEBIDE_BUNDLED_ARTIFACTS must select the compiled default bundle when building
that bridge. Set CHROMEDRIVER and optionally CHROME. The repository's target path
must expose the backend module referenced by spin.toml (including shared-target
worktrees). Run: python3 tools/check-language-host.py
Pass --marketplace-ref with the published immutable example catalog commit to
also verify catalog UI installation on both execution hosts.

All account, project, pairing, host-cache and browser data is disposable. The
local case restores a draft without a File System Access directory handle: it
checks pure host syntax through the paired transport, not folder permissions or
native file access. No frontend transport or component result is mocked.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import runpy
import secrets
import shutil
import sqlite3
import subprocess
import tempfile
import time

from language_host_support import ExecutionHost

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
DEFAULTS = json.loads((ROOT / 'plugins/bundled.json').read_text())
PREVIOUS_LANGUAGE_COMMIT = '88ec5ab1e563219d5d9f2045d4216f400842f64b'
NOTES_LANGUAGE_COMMIT = '231b9039373048b267c027bf83c98c730527b650'

def wait(label, predicate, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(.1)
    raise AssertionError(label + ' timed out')


def preparation_urls(runtime, host, mode, project):
    if mode == 'local':
        return (host.url + '/plugins/prepare', host.url + '/plugins/prepare/status')
    return (runtime.url + '/api/plugins/preparation',
            runtime.url + f'/api/projects/{project}/plugins/preparation')


def install_source(runtime, source, revision=None):
    progress = runtime.request('POST', '/api/plugins/preparation', {'action': 'start', 'source': source})
    operation = progress['id']
    deadline = time.monotonic() + 180
    try:
        while progress['state'] != 'ready':
            assert progress['state'] not in ('failed', 'cancelled'), progress.get('error')
            assert time.monotonic() < deadline, 'Custom source preparation timed out'
            time.sleep(.1)
            progress = runtime.request('POST', '/api/plugins/preparation', {'action': 'status', 'id': operation})
        prepared = progress['prepared']
        assert prepared['source'] == source
        assert prepared['manifest']['executable']['capabilities'] == []
        package = runtime.request('POST', '/api/plugins/package', prepared)
        installations = runtime.request('POST', '/api/plugins', {'prepared': prepared, 'package': package,
            'approved_capabilities': [], 'revision': revision})
        return installations
    finally:
        if progress['state'] != 'ready':
            runtime.request('POST', '/api/plugins/preparation', {'action': 'cancel', 'id': operation})

def check_marketplace(runtime, host, browser, reference, contexts):
    """Install the SDK-only provider through real catalog controls on each host."""
    source = {'repository': DEFAULTS['repository'], 'reference': reference,
              'path': 'examples/language-marketplace.json'}
    markets = runtime.request('GET', '/api/plugin-marketplaces')
    runtime.request('POST', '/api/plugin-marketplaces', {
        'revision': markets['revision'], 'sources': [*markets['sources'], source]})
    refreshed = runtime.request('POST', '/api/plugin-marketplaces/refresh', None)
    catalog = next(entry for entry in refreshed['settings']['catalogs'] if entry['source'] == source)
    assert catalog['commit'] == reference, 'Use an immutable published catalog commit'
    listing = next(entry for entry in catalog['catalog']['plugins'] if entry['name'] == 'notes-language')
    assert [release['source']['commit'] for release in listing['releases']] == [
        NOTES_LANGUAGE_COMMIT, PREVIOUS_LANGUAGE_COMMIT]
    for context in contexts:
        mode, project, normalized = context['mode'], context['project'], context['text']
        installations = runtime.request('GET', '/api/plugins')
        installation = next(entry for entry in installations
                            if entry['prepared']['manifest']['name'] == 'notes-language')
        runtime.request('POST', '/api/plugins/remove', {
            'source': installation['prepared']['source'], 'revision': installation['revision']})
        for key, value in {'open_tabs': json.dumps([project]), 'active_project': str(project)}.items():
            runtime.request('PUT', '/api/settings', {'key': key, 'value': value})
        browser.call('POST', '/url', {'url': runtime.url + '/'})
        wait(f'{mode} custom catalog keeps exact plain draft', lambda: browser.script('''
            const input = document.querySelector('textarea[data-editor-path]');
            return input?.value === __TEXT__ &&
                !document.querySelector('.editor-code .tok-keyword,.editor-code .tok-string');
        '''.replace('__TEXT__', json.dumps(normalized))))
        wait('Plugins control', lambda: browser.script('''
            const button = document.querySelector('button[title="Browse and manage plugins"]');
            if (!button) return false;
            button.click(); return true;
        '''))
        wait(f'{mode} published custom release is available', lambda: browser.script('''
            return [...document.querySelectorAll('[role="dialog"] .plugin-entry')].some(entry =>
                entry.querySelector('.plugin-name')?.textContent === 'Notes language' &&
                [...entry.querySelectorAll('button')].some(button =>
                    button.textContent.trim() === 'Install' && !button.disabled));
        '''))
        browser.script('window.nativeLanguageDiagnostics.requests = [];')
        assert browser.script('''
            const entry = [...document.querySelectorAll('[role="dialog"] .plugin-entry')]
                .find(entry => entry.querySelector('.plugin-name')?.textContent === 'Notes language');
            const button = [...entry.querySelectorAll('button')]
                .find(button => button.textContent.trim() === 'Install');
            button.click(); return true;
        ''')
        def installed():
            return next((entry for entry in runtime.request('GET', '/api/plugins')
                         if entry['prepared']['source'] == installation['prepared']['source']), None)
        installed_entry = wait(f'{mode} catalog UI installs pinned component', installed, seconds=180)
        assert installed_entry['prepared']['manifest']['version'] == listing['releases'][0]['version']
        assert installed_entry['prepared']['manifest']['executable']['capabilities'] == []
        endpoint = f'/api/projects/{project}/plugins'
        def enabled():
            bindings = runtime.request('GET', endpoint)
            current = next((entry for entry in bindings if entry['prepared']['source'] ==
                            installed_entry['prepared']['source']), None)
            if current is not None and current['enabled']:
                return current
            # Respect a retained project opt-out by explicitly enabling through
            # the same user-facing controls, rather than overriding it in setup.
            browser.script('''
                const entry = [...document.querySelectorAll('[role="dialog"] .plugin-entry')]
                    .find(entry => entry.querySelector('.plugin-name')?.textContent === 'Notes language');
                const button = entry && [...entry.querySelectorAll('button')]
                    .find(button => button.textContent.trim() === 'Enable' && !button.disabled);
                button?.click();
            ''')
            return None
        wait(f'{mode} catalog installation is enabled', enabled)
        requests = browser.script('return window.nativeLanguageDiagnostics.requests;')
        assert any(request['url'] in preparation_urls(runtime, host, mode, project)
                   and request['status'] == 200
                   and request.get('preparation', {}).get('state') == 'ready'
                   and request.get('preparation', {}).get('source') == installed_entry['prepared']['source']
                   for request in requests), \
            'Catalog installation must prepare on the selected execution host'
        browser.call('POST', '/url', {'url': runtime.url + '/'})
        wait(f'{mode} catalog component paints preserved draft after reload', lambda: browser.script('''
            const input = document.querySelector('textarea[data-editor-path]');
            const paint = document.querySelector('.editor-highlight-content');
            return input?.value === __TEXT__ && paint?.textContent === __TEXT__ &&
                [...document.querySelectorAll('.editor-code .tok-keyword')]
                    .some(token => token.textContent === 'note') &&
                [...document.querySelectorAll('.editor-code .tok-string')]
                    .some(token => token.textContent.includes('文😀'));
        '''.replace('__TEXT__', json.dumps(normalized))))
        print(json.dumps({'custom_marketplace_ui': True, 'mode': mode,
                          'catalog_commit': reference, 'source': installed_entry['prepared']['source'],
                          'source_exact_after_reload': True}), flush=True)


def check_embedded_providers(runtime, host, browser, files, mode, custom=False):
    """A live dependency change must refresh the unchanged outer provider."""
    outer = 'markdown' if custom else 'html'
    child = 'notes-language' if custom else 'javascript-typescript'
    child_name = 'Notes language' if custom else 'JavaScript and TypeScript'
    folder = files / f'{mode}-embedded-{outer}'
    folder.mkdir()
    subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
    filename = 'embedded.md' if custom else 'embedded.html'
    source = ('# Heading\r\n\r\n```notes\r\nnote {\r\n    task "文😀" 42\r\n}\r\n```\r\n' if custom else
        '<html>\r\n<script>\r\nfunction greet() {\r\n    return "文😀";\r\n}\r\n</script>\r\n<style>\r\np { color: red; }\r\n</style>\r\n</html>\r\n')
    (folder / filename).write_bytes(source.encode())
    path = folder.relative_to(ROOT.parent.parent).as_posix() if mode == 'remote' else f'language-native-embedded-{mode}-{outer}'
    project = runtime.request('POST', '/api/projects', {'name': f'Embedded {mode}', 'mode': mode, 'path': path}, expected=201)
    encoded = base64.b64encode(source.encode()).decode()
    recovery = {'format': 1, 'root': {'mode': mode, 'path': path if mode == 'remote' else None}, 'selected': filename,
        'files': [{'path': filename, 'document': {'text': encoded, 'saved': encoded, 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []}, 'scroll': {'top': 0, 'left': 0}, 'read_only': False}]}
    runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': recovery})
    for key, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
        runtime.request('PUT', '/api/settings', {'key': key, 'value': value})
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    normalized = source.replace('\r\n', '\n')
    def colors(enabled):
        value = browser.script('''
            const input = document.querySelector('textarea[data-editor-path]');
            const paint = document.querySelector('.editor-highlight-content');
            return {input: input?.value, paint: paint?.textContent,
                html: [...document.querySelectorAll('.editor-code .tok-keyword')].some(token => token.textContent === 'html'),
                markdown: [...document.querySelectorAll('.editor-code .tok-keyword')].some(token => token.textContent === '#'),
                child: [...document.querySelectorAll('.editor-code .tok-keyword')].some(token => token.textContent === __KEYWORD__),
                string: [...document.querySelectorAll('.editor-code .tok-string')].some(token => token.textContent.includes('文😀')),
                css: [...document.querySelectorAll('.editor-code .tok-attribute')].some(token => token.textContent === 'color')};
        '''.replace('__KEYWORD__', json.dumps('task' if custom else 'function')))
        return (value['input'] == normalized and value['paint'] == normalized
                and (value['markdown'] if custom else (value['html'] and value['css']))
                and value['child'] == enabled and value['string'] == enabled)
    wait(f'{mode} independent {outer}/{child} native providers paint', lambda: colors(True))
    endpoint = f"/api/projects/{project['id']}/plugins"
    before = runtime.request('GET', endpoint)
    outer_before = next(entry for entry in before if entry['prepared']['manifest']['name'] == outer)
    wait('Plugins control for embedded dependency', lambda: browser.script('''
        const button = document.querySelector('button[title="Browse and manage plugins"]');
        if (!button) return false;
        button.click(); return true;
    '''))
    def choose(label):
        return browser.script('''
            const entry = [...document.querySelectorAll('[role="dialog"] .plugin-entry')]
                .find(entry => entry.querySelector('.plugin-name')?.textContent === __CHILD__);
            const button = entry && [...entry.querySelectorAll('button')]
                .find(button => button.textContent.trim() === __LABEL__ && !button.disabled);
            if (!button) return false;
            button.click(); return true;
        '''.replace('__LABEL__', json.dumps(label)).replace('__CHILD__', json.dumps(child_name)))
    wait(f'{mode} disable {child} through UI', lambda: choose('Disable'))
    wait(f'{mode} disabled embedded {child} stays plain', lambda: colors(False))
    after = runtime.request('GET', endpoint)
    assert next(entry for entry in after if entry['prepared']['manifest']['name'] == outer) == outer_before
    assert not next(entry for entry in after if entry['prepared']['manifest']['name'] == child)['enabled']
    wait(f'{mode} reenable {child} through UI', lambda: choose('Enable'))
    wait(f'{mode} embedded {child} returns without changing the outer provider', lambda: colors(True))
    assert next(entry for entry in runtime.request('GET', endpoint)
                if entry['prepared']['manifest']['name'] == outer) == outer_before
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait(f'{mode} composed native providers keep exact source after reload', lambda: colors(True))
    print(json.dumps({'embedded_provider_ui': True, 'mode': mode, 'outer': outer, 'child': child,
        'outer_provider_unchanged': True, 'child_disable_plain_reenable': True,
        'html_css_preserved': not custom, 'source_exact_after_reload': True}), flush=True)


def check(marketplace_reference=None):
    assert (ROOT / 'target/wasm32-wasip2/release/openwebide_backend.wasm').is_file(), 'Build the WASI backend at the path referenced by spin.toml'
    assert (ROOT / 'frontend/dist/index.html').is_file(), 'Build the production frontend first'
    with tempfile.TemporaryDirectory(prefix='openwebide-language-native-') as directory, tempfile.TemporaryDirectory(prefix='language-native-fixture-', dir=ROOT.parent) as files:
        state = Path(directory)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        host = ExecutionHost(runtime)
        browser = None
        try:
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'language-' + secrets.token_hex(8), 'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            checked = time.monotonic()
            account = runtime.request('GET', '/api/auth/me')
            assert account['user'] is not None
            with sqlite3.connect(runtime.state / 'sqlite_db.db') as database:
                assert database.execute('SELECT COUNT(*) FROM plugin_records').fetchone()[0] == 0, \
                    'Account lookup must leave bundled installation to plugin loading'
            print(json.dumps({'authenticated_without_plugin_wait': True, 'seconds': round(time.monotonic() - checked, 3)}), flush=True)
            # Existing host preparation allows 120 seconds per source. Bootstrap
            # defaults through the ordinary API separately from the 30s editor check.
            initialized = time.monotonic()
            installations = host.initialize_defaults()
            print(json.dumps({'compiled_defaults': len(installations), 'seconds': round(time.monotonic() - initialized, 3)}), flush=True)
            custom_source = {'repository': DEFAULTS['repository'], 'commit': NOTES_LANGUAGE_COMMIT, 'path': 'examples/language-plugin'}
            installations = install_source(runtime, custom_source)
            print(json.dumps({'custom_source_installed': custom_source}), flush=True)
            cases = [
                {'name': 'rust', 'file': 'fixture.rs', 'keyword': 'fn', 'literal': '// plain fallback',
                 'source': {'repository': DEFAULTS['repository'], 'commit': DEFAULTS.get('commits', {}).get('plugins/rust', DEFAULTS['commit']), 'path': 'plugins/rust'}},
                {'name': 'notes-language', 'file': 'fixture.notes', 'keyword': 'note', 'literal': '# plain fallback', 'source': custom_source,
                 'version_history': [(PREVIOUS_LANGUAGE_COMMIT, '0.1.0'), (NOTES_LANGUAGE_COMMIT, '0.1.1')]},
            ]
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value, 'domain': '127.0.0.1', 'path': cookie.path, 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument', 'params': {'source': r"""
                window.nativeLanguageDiagnostics = {errors: [], requests: []};
                window.addEventListener('error', event => nativeLanguageDiagnostics.errors.push(String(event.message)));
                window.addEventListener('unhandledrejection', event => nativeLanguageDiagnostics.errors.push(String(event.reason)));
                const originalFetch = window.fetch;
                window.fetch = function(...args) {
                    const entry = {url: String(args[0]?.url ?? args[0]), status: null};
                    nativeLanguageDiagnostics.requests.push(entry);
                    if (nativeLanguageDiagnostics.requests.length > 100) nativeLanguageDiagnostics.requests.shift();
                    return originalFetch.apply(this, args).then(async response => {
                        entry.status = response.status;
                        if (response.ok && /\/plugins\/(preparation|prepare(?:\/status)?)$/.test(new URL(entry.url, location.href).pathname)) {
                            const progress = await response.clone().json();
                            entry.preparation = {state: progress.state, source: progress.prepared?.source,
                                error: progress.error?.slice(0, 2000)};
                        }
                        return response;
                    }, error => {entry.error = String(error); throw error;});
                };
            """}})
            receipts = []
            catalog_contexts = []
            for mode in ('remote', 'local'):
                for case in cases:
                    filename = case['file']
                    source = (f'pub fn {mode}_fixture() {{\r\n    let greeting = "文😀 {mode}";\r\n    println!("{{greeting}}");\r\n}}\r\n' if case['name'] == 'rust'
                        else f'note {{\r\n    task "文😀 {mode}" 42\r\n}}\r\n')
                    folder = Path(files) / f"{mode}-{case['name']}"
                    folder.mkdir()
                    subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                    (folder / filename).write_bytes(source.encode())
                    path = folder.relative_to(ROOT.parent.parent).as_posix() if mode == 'remote' else f"language-native-{case['name']}"
                    project = runtime.request('POST', '/api/projects', {'name': f"Language {mode} {case['name']}", 'mode': mode, 'path': path}, expected=201)
                    encoded = base64.b64encode(source.encode()).decode()
                    recovery = {'format': 1, 'root': {'mode': mode, 'path': path if mode == 'remote' else None}, 'selected': filename,
                        'files': [{'path': filename, 'document': {'text': encoded, 'saved': encoded, 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []}, 'scroll': {'top': 0, 'left': 0}, 'read_only': False}]}
                    runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': recovery})
                    for key, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
                        runtime.request('PUT', '/api/settings', {'key': key, 'value': value})
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    normalized = source.replace('\r\n', '\n')
                    def painted():
                        value = browser.script('''
                            const input = document.querySelector('textarea[data-editor-path]');
                            const paint = document.querySelector('.editor-highlight-content');
                            return {input: input?.value, paint: paint?.textContent,
                                keyword: [...document.querySelectorAll('.editor-code .tok-keyword')].some(token => token.textContent === __KEYWORD__),
                                string: [...document.querySelectorAll('.editor-code .tok-string')].some(token => token.textContent.includes('文😀'))};
                        '''.replace('__KEYWORD__', json.dumps(case['keyword'])))
                        return value if value['input'] == normalized and value['paint'] == normalized and value['keyword'] and value['string'] else None
                    value = wait(f'{mode} native component editor paint', painted)
                    requests = browser.script('return window.nativeLanguageDiagnostics.requests;')
                    transport = host.url + '/plugins/' if mode == 'local' else runtime.url + '/api/plugins/'
                    assert any(request['url'].startswith(transport) and '/continue' in request['url']
                               and request['status'] == 200 for request in requests), \
                        'Syntax must complete through the selected host transport'
                    bindings = runtime.request('GET', f"/api/projects/{project['id']}/plugins")
                    languages = [binding for binding in bindings if binding['prepared']['manifest']['name'] == case['name']]
                    assert len(languages) == 1
                    assert languages[0]['prepared']['source'] == case['source']
                    language = languages[0]
                    installation = next(entry for entry in installations if entry['prepared']['source'] == language['prepared']['source'])
                    package = runtime.request('POST', '/api/plugins/package', language['prepared'])
                    endpoint = f"/api/projects/{project['id']}/plugins"
                    changed = runtime.request('POST', endpoint, {'action': 'disable', 'id': language['id'], 'revision': language['revision']})
                    disabled = next(entry for entry in changed if entry['prepared']['source'] == language['prepared']['source'])
                    assert not disabled['enabled']
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    def plain_painted():
                        value = browser.script("""
                            const input = document.querySelector('textarea[data-editor-path]');
                            const paint = document.querySelector('.editor-highlight-content');
                            return {input: input?.value, paint: paint?.textContent,
                                colored: !!document.querySelector('.editor-code .tok-keyword,.editor-code .tok-string,.editor-code .tok-comment')};
                        """)
                        return value['input'] == normalized and value['paint'] == normalized and not value['colored']
                    wait(f'{mode} disabled package leaves exact plain text', plain_painted)
                    browser.script("""
                        const input = document.querySelector('textarea[data-editor-path]');
                        input.focus(); input.setSelectionRange(input.value.length, input.value.length);
                    """)
                    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': case['literal']}})
                    normalized += case['literal']
                    wait(f'{mode} plain editing remains usable', plain_painted)
                    def recovered_plain_edit():
                        record = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                        document = next(entry['document'] for entry in record['state']['files'] if entry['path'] == filename)
                        return base64.b64decode(document['text']).decode().replace('\r\n', '\n') == normalized
                    wait(f'{mode} plain edit reaches recovery storage', recovered_plain_edit, seconds=15)
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    wait(f'{mode} disabled package preserves recovered edits', plain_painted)
                    changed = runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
                        'installation_revision': installation['revision'], 'revision': disabled['revision']})
                    assert next(entry for entry in changed if entry['prepared']['source'] == language['prepared']['source'])['enabled']
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    wait(f'{mode} re-enabled package analyzes the recovered source', painted)
                    assert browser.script("return [...document.querySelectorAll('.editor-code .tok-comment')].some(token => token.textContent.includes('plain fallback'));")
                    host.stop_process()
                    browser.script("""
                        const input = document.querySelector('textarea[data-editor-path]');
                        input.focus(); input.setSelectionRange(input.value.length, input.value.length);
                    """)
                    literal = ' host outage'
                    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': literal}})
                    normalized += literal
                    wait(f'{mode} host outage leaves current plain editing', plain_painted)
                    assert browser.script("""return !!document.querySelector('button[aria-label="Retry language support"]');""")
                    wait(f'{mode} host outage preserves recovery', recovered_plain_edit, seconds=15)
                    host.start()
                    browser.script("""document.querySelector('button[aria-label="Retry language support"]').click();""")
                    wait(f'{mode} retry restores the native provider after host restart', painted)
                    assert browser.script("return [...document.querySelectorAll('.editor-code .tok-comment')].some(token => token.textContent.includes('host outage'));")
                    installations = runtime.request('POST', '/api/plugins/remove', {
                        'source': installation['prepared']['source'], 'revision': installation['revision']})
                    assert not any(entry['prepared']['source'] == case['source'] for entry in installations)
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    wait(f'{mode} uninstall clears host syntax without changing source', plain_painted)
                    # Cached receipts cannot reactivate an uninstalled version.
                    runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
                        'installation_revision': installation['revision'], 'revision': None}, expected=409)
                    browser.script("""
                        const input = document.querySelector('textarea[data-editor-path]');
                        input.focus(); input.setSelectionRange(input.value.length, input.value.length);
                    """)
                    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': ' after uninstall'}})
                    normalized += ' after uninstall'
                    wait(f'{mode} uninstall preserves ordinary editing', plain_painted)
                    wait(f'{mode} uninstall preserves draft recovery', recovered_plain_edit, seconds=15)
                    installations = runtime.request('POST', '/api/plugins', {
                        'prepared': package['prepared'], 'package': package,
                        'approved_capabilities': [], 'revision': None})
                    reinstalled = next(entry for entry in installations if entry['prepared']['source'] == case['source'])
                    current = runtime.request('GET', endpoint)
                    previous = next((entry for entry in current if entry['prepared']['source'] == case['source']), None)
                    runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
                        'installation_revision': reinstalled['revision'],
                        'revision': previous['revision'] if previous else None})
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    wait(f'{mode} reinstall analyzes the preserved draft', painted)
                    assert browser.script("return [...document.querySelectorAll('.editor-code .tok-comment')].some(token => token.textContent.includes('after uninstall'));")
                    # Notes has two published releases; retain its real immutable
                    # rollback/update contract on both execution hosts. Rust is
                    # a new independent package with its first release.
                    history = case.get('version_history', [])
                    current_installation = reinstalled
                    current_package = package
                    latest_version = reinstalled['prepared']['manifest']['version']
                    if history:
                        assert history[-1] == (case['source']['commit'], latest_version)
                        assert latest_version != '0.1.0'
                    version_receipts = []
                    for commit, expected_version in history:
                        changed_source = {**case['source'], 'commit': commit}
                        previous_installation = current_installation
                        previous_package = current_package
                        installations = install_source(runtime, changed_source,
                                                       revision=previous_installation['revision'])
                        current_installation = next(entry for entry in installations
                            if entry['prepared']['source'] == changed_source)
                        assert current_installation['revision'] > previous_installation['revision']
                        assert current_installation['prepared']['manifest']['version'] == expected_version
                        assert current_installation['prepared']['digest'] != previous_installation['prepared']['digest']
                        current_package = runtime.request('POST', '/api/plugins/package', current_installation['prepared'])
                        # An old receipt must not restore the previous version behind the current installation.
                        runtime.request('POST', endpoint, {'action': 'enable', 'package': previous_package,
                            'installation_revision': previous_installation['revision'], 'revision': None}, expected=409)
                        current_bindings = runtime.request('GET', endpoint)
                        current_binding = next(entry for entry in current_bindings
                            if entry['prepared']['source'] == changed_source)
                        assert current_binding['enabled']
                        assert current_binding['prepared']['manifest']['version'] == expected_version
                        browser.call('POST', '/url', {'url': runtime.url + '/'})
                        # Installation prepares on the server. A paired host may
                        # still need a cold source build of the historical version.
                        # Observe the normal 900-second preparation contract, then
                        # retain the existing 30-second editor-paint deadline.
                        preparation_started = time.monotonic()
                        preparation_transports = preparation_urls(runtime, host, mode, project['id'])
                        def host_prepared():
                            requests = browser.script('return window.nativeLanguageDiagnostics.requests;')
                            for request in requests:
                                if request['url'] not in preparation_transports:
                                    continue
                                assert request['status'] is None or request['status'] < 400, request
                                progress = request.get('preparation', {})
                                assert progress.get('state') != 'failed', progress.get('error')
                                if progress.get('state') == 'ready' and progress.get('source') == changed_source:
                                    return True
                            return False
                        wait(f'{mode} {case["name"]} version {expected_version} selected-host preparation',
                             host_prepared, seconds=900)
                        print(json.dumps({'selected_host_preparation': True, 'mode': mode,
                            'package': case['name'], 'version': expected_version,
                            'seconds': round(time.monotonic() - preparation_started, 3)}), flush=True)
                        wait(f'{mode} {case["name"]} version {expected_version} paints the preserved draft', painted)
                        wait(f'{mode} version change preserves draft recovery', recovered_plain_edit, seconds=15)
                        version_requests = browser.script('return window.nativeLanguageDiagnostics.requests;')
                        assert any(request['url'].startswith(transport) and '/continue' in request['url']
                                   and request['status'] == 200 for request in version_requests), \
                            'The changed version must execute through the selected host transport'
                        version_receipts.append({'version': expected_version, 'commit': commit,
                                                 'stale_receipt_rejected': True, 'source_exact': True})
                    receipt = {'mode': mode, 'package': case['name'], 'transport': 'paired' if mode == 'local' else 'server',
                        'source': languages[0]['prepared']['source'], 'host': languages[0]['prepared']['host_id'],
                        'keyword': value['keyword'], 'string': value['string'], 'source_exact': True,
                        'disable_plain_edit_recovery_reenable': True, 'host_outage_restart_retry': True,
                        'uninstall_plain_edit_recovery_reinstall': True, 'uninstalled_receipt_rejected': True,
                        'immutable_version_rollback_update': version_receipts}
                    print(json.dumps(receipt), flush=True)
                    receipts.append(receipt)
                    if case['name'] == 'notes-language':
                        catalog_contexts.append({'mode': mode, 'project': project['id'], 'text': normalized})
            for mode in ('remote', 'local'):
                check_embedded_providers(runtime, host, browser, Path(files), mode)
                check_embedded_providers(runtime, host, browser, Path(files), mode, custom=True)
            if marketplace_reference is not None:
                check_marketplace(runtime, host, browser, marketplace_reference, catalog_contexts)
            print(json.dumps({'native_component_browser_cases': len(receipts)}), flush=True)
        except BaseException:
            # Keep bounded diagnostics; never print credentials, cookies or payloads.
            diagnostics = Path(os.environ.get('OPENWEBIDE_LANGUAGE_DIAGNOSTICS', str(Path(tempfile.gettempdir()) / 'openwebide-language-host-diagnostics')))
            diagnostics.mkdir(exist_ok=True)
            for name, original in [('bridge.log', host.state / 'bridge.log'), ('runtime.log', runtime.state / 'runtime.log')]:
                if original.exists():
                    (diagnostics / name).write_bytes(original.read_bytes())
            if (runtime.state / 'logs').exists():
                shutil.copytree(runtime.state / 'logs', diagnostics / 'components', dirs_exist_ok=True)
            if browser is not None:
                try:
                    (diagnostics / 'network.json').write_text(json.dumps(browser.script('return window.nativeLanguageDiagnostics;'), indent=2))
                    (diagnostics / 'browser.json').write_text(json.dumps(browser.call('POST', '/log', {'type': 'browser'}), indent=2))
                except Exception:
                    pass
                try:
                    (diagnostics / 'page.txt').write_text(browser.script('return document.body.innerText;'))
                except Exception:
                    pass
            raise
        finally:
            if browser is not None:
                browser.stop()
            runtime.stop()
            host.stop()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--marketplace-ref', help='Published immutable commit containing examples/language-marketplace.json')
    check(parser.parse_args().marketplace_ref)
