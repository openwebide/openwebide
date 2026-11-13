#!/usr/bin/env python3
"""Production provider-bound reviews and authority races in both project modes.

Installs two disposable API9 server-only packages through the public plugin API.
Only the protocol peer and bounded transport latches are fixtures; review,
authorization, native verification, journal admission, files and editor use the
built app. Local OPFS/native correspondence does not prove physical permissions.
Build current release frontend/backend and bundled-defaults bridge before use.
"""
import argparse
import base64
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import runpy
import secrets
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE_ROOT = None
sys.path.insert(0, str(ROOT / 'tools'))
from language_review_support import AdmissionProxy, assert_review_session
from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, click, close, command as menu_command, insert, local_handle, script, wait

NAV = runpy.run_path(str(ROOT / 'tools/check-language-navigation-client.py'))
RESOURCE = runpy.run_path(str(ROOT / 'tools/check-workspace-transactions-client.py'))
Runtime, Browser = NAV['Runtime'], NAV['Browser']
SOURCE, DRAFT, OTHER = 'Disk 文😀\r\n', 'Draft 文😀\r\n', 'Unrelated 文😀\r\n'
REVIEWED = 'Reviewed 文😀\n'
SCENARIOS = ('refresh', 'authorization', 'admission', 'identical', 'late-reply',
             'replace-authorization', 'replace-admission')
EVIDENCE = RESOURCE['EVIDENCE'].replace(
    'analysisResult:native?data.result:undefined,',
    'analysisResult:native?data.result:undefined,diagnostics:native?data.diagnostics:undefined,')
assert EVIDENCE != RESOURCE['EVIDENCE']
for before, after in [
    ('native:[],timeline:[]', 'native:[],authorizations:[],timeline:[]'),
    (r"const native=/\/plugins\/languages\/query$/.test(pathname),folder=",
     r"const native=/\/plugins\/languages\/query$/.test(pathname),authorize=/\/plugins\/languages\/authorize$/.test(pathname),folder="),
    ("const trace=native||folder||transaction||grant?{kind:native?'language':",
     "const trace=native||authorize||folder||transaction||grant?{kind:authorize?'language_authorization':native?'language':"),
    ('if(native){const input=', 'if(native||authorize){const input='),
    ('resourceProbe.native.push(trace);', '(native?resourceProbe.native:resourceProbe.authorizations).push(trace);'),
]:
    assert EVIDENCE.count(before) == 1, before
    EVIDENCE = EVIDENCE.replace(before, after)
VALIDATION_FETCH = ' const response=await resourceFetch.apply(this,args);'
assert EVIDENCE.count(VALIDATION_FETCH) == 1
EVIDENCE = EVIDENCE.replace(VALIDATION_FETCH, r'''
 if(trace?.query?.operation==='validate_analysis'&&resourceProbe.holdValidation
    &&(authorize||(native&&new URL(url,location.href).origin===location.origin))) {
  trace.heldForRevocation=true;
  resourceProbe.validationPaused=true;
  await new Promise(resolve=>resourceProbe.releaseValidation=resolve);
  resourceProbe.validationPaused=false;resourceProbe.holdValidation=false;
 }
''' + VALIDATION_FETCH)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()




def repository(state, control, log):
    path = state / 'repository'
    path.mkdir()
    for name, role in [('review-owner', 'language'), ('review-analyzer', 'diagnostics')]:
        folder = path / name
        folder.mkdir()
        manifest = {'schemaVersion': 1, 'publisher': 'probe', 'name': name, 'version': '0.1.0',
            'displayName': name, 'description': 'Disposable retained-review production fixture',
            'license': 'MIT', 'compatibility': {'pluginApi': 9}, 'contributions': {
                'skills': [], 'languages': [{'id': 'notes', 'name': 'Notes', 'extensions': ['notes'],
                    'features': {'languageServer': {'command': sys.executable,
                        'args': [str(ROOT / 'tools/fixtures/language_review_server.py'), role, str(control), str(log)],
                        'rootMarkers': ['.git'], 'role': role}}}]}}
        (folder / 'plugin.json').write_text(json.dumps(manifest))
    subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(path)], check=True)
    subprocess.run(['git', '-C', str(path), 'add', '.'], check=True)
    subprocess.run(['git', '-C', str(path), '-c', 'user.name=Test', '-c', 'user.email=test@example.org',
                    'commit', '--quiet', '-m', 'Public retained-review fixtures'], check=True)
    commit = subprocess.check_output(['git', '-C', str(path), 'rev-parse', 'HEAD'], text=True).strip()
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        port = listener.getsockname()[1]
    daemon = subprocess.Popen(['git', 'daemon', '--export-all', '--reuseaddr', '--listen=127.0.0.1',
        f'--port={port}', '--base-path=' + str(path), str(path)],
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


def events(log):
    if not log.exists():
        return []
    # Each process appends a complete small JSON line. An incomplete trailing
    # read is pending evidence, never a successful acknowledgement.
    lines = log.read_text().splitlines(keepends=True)
    return [json.loads(line) for line in lines if line.endswith('\n')]


def saved(runtime, project, path):
    files = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")['state']['files']
    entry = next(file for file in files if file['path'] == path)
    return base64.b64decode(entry['document']['text']).decode()


def prepare_owner_update(runtime, state, url, packages):
    """Prepare outside the timed latch; installation remains unchanged here."""
    old = packages[0][0]
    path = state / 'repository/review-owner/plugin.json'
    manifest = json.loads(path.read_text())
    assert manifest['version'] == old['prepared']['manifest']['version']
    major, minor, patch = map(int, manifest['version'].split('.'))
    manifest['version'] = f'{major}.{minor + 1}.{patch}'
    path.write_text(json.dumps(manifest))
    repository_path = path.parent.parent
    subprocess.run(['git', '-C', str(repository_path), 'add', 'review-owner/plugin.json'], check=True)
    subprocess.run(['git', '-C', str(repository_path), '-c', 'user.name=Test', '-c', 'user.email=test@example.org',
                    'commit', '--quiet', '-m', 'Update the public review provider'], check=True)
    commit = subprocess.check_output(['git', '-C', str(repository_path), 'rev-parse', 'HEAD'], text=True).strip()
    package = NAV['prepare_package'](runtime, url, commit, 'review-owner')
    prepared = package['prepared']
    assert prepared['digest'] != old['prepared']['digest']
    current = next(entry for entry in runtime.request('GET', '/api/plugins')
                   if entry['prepared']['source'] == old['prepared']['source'])
    assert current['prepared']['digest'] == old['prepared']['digest']
    return package


def activate_owner_update(runtime, project, packages, package):
    old = packages[0][0]
    endpoint = f"/api/projects/{project['id']}/plugins"
    before = next(entry for entry in runtime.request('GET', endpoint)
                  if entry['prepared']['manifest']['name'] == 'review-owner')
    current = next(entry for entry in runtime.request('GET', '/api/plugins')
                   if entry['prepared']['source'] == old['prepared']['source'])
    prepared = package['prepared']
    updated = NAV['install_package'](runtime, package, current['revision'])
    assert updated['revision'] > current['revision']
    assert updated['prepared']['digest'] != old['prepared']['digest']
    binding = next(entry for entry in runtime.request('GET', endpoint)
                   if entry['prepared']['manifest']['name'] == 'review-owner')
    changed = runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
        'installation_revision': updated['revision'], 'revision': binding['revision']})
    enabled = next(entry for entry in changed if entry['id'] == before['id'])
    assert enabled['enabled'] and enabled['revision'] > before['revision']
    assert enabled['prepared']['digest'] == prepared['digest']
    assert enabled['prepared']['source']['repository'] == before['prepared']['source']['repository']
    assert enabled['prepared']['source']['path'] == before['prepared']['source']['path']
    assert enabled['prepared']['manifest']['name'] == before['prepared']['manifest']['name']
    packages[0] = updated, package
    return {'previous_digest': old['prepared']['digest'], 'current_digest': prepared['digest'],
            'previous_binding_revision': before['revision'], 'current_binding_revision': enabled['revision']}


def change_owner_authority(runtime, project, packages, replacement):
    if replacement is not None:
        return activate_owner_update(runtime, project, packages, replacement)
    endpoint = f"/api/projects/{project['id']}/plugins"
    owner = next(entry for entry in runtime.request('GET', endpoint)
                 if entry['prepared']['manifest']['name'] == 'review-owner')
    changed = runtime.request('POST', endpoint, {'action': 'disable', 'id': owner['id'],
                                               'revision': owner['revision']})
    disabled = next(entry for entry in changed if entry['id'] == owner['id'])
    assert disabled['enabled'] is False
    return {'disabled': disabled}


def restore_owner_authority(runtime, project, packages, changed):
    if 'disabled' not in changed:
        return
    owner, package = packages[0]
    current = next(entry for entry in runtime.request('GET', '/api/plugins')
                   if entry['prepared']['source'] == owner['prepared']['source'])
    runtime.request('POST', f"/api/projects/{project['id']}/plugins", {'action': 'enable', 'package': package,
        'installation_revision': current['revision'], 'revision': changed['disabled']['revision']})


def unaccepted(runtime, user, project, journal):
    # Observe actual persisted authority without introducing a product endpoint
    # or modifying the database. The real application remains its only writer.
    with closing(sqlite3.connect((runtime.state / 'sqlite_db.db').as_uri() + '?mode=ro', uri=True)) as db:
        row = db.execute('SELECT worker_id,worker_digest,accepted_language_review_digest '
                         'FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=? AND generation=?',
                         (user, project['id'], journal['journal']['id'], journal['journal']['generation'])).fetchone()
    assert row == ('', '', None), 'A rejected review accepted native worker or review authority'


def revoke_pending_validation(runtime, browser, project, folder, packages, original, replacement=None):
    transactions = f"/api/projects/{project['id']}/workspace-transactions"
    actions = f"/api/projects/{project['id']}/language-actions"
    wait('validation request held before backend authorization', lambda: script(browser, 'return resourceProbe.validationPaused;'))
    assert runtime.request('GET', transactions) is None
    assert runtime.request('POST', actions, {'operation': 'load', 'id': None}) is None
    changed = change_owner_authority(runtime, project, packages, replacement)
    script(browser, 'resourceProbe.releaseValidation();resourceProbe.releaseValidation=null;')
    validation = wait('real revoked validation authorization reply', lambda: script(browser,
        "return resourceProbe.timeline.filter(r=>r.heldForRevocation&&r.completed).slice(-1)[0];"))
    assert validation['status'] == 409 and validation.get('error') == 'Plugin is no longer enabled at the selected version', validation
    # This is the earlier no-intent contract. It is deliberately distinct from
    # revocation after verification, where an untouched Prepared record exists.
    assert runtime.request('GET', transactions) is None
    assert runtime.request('POST', actions, {'operation': 'load', 'id': None}) is None
    assert RESOURCE['source'](browser) == original
    assert not (folder / 'created.notes').exists()
    assert (folder / 'main.notes').read_bytes() == SOURCE.encode()
    assert saved(runtime, project, 'unrelated.notes') == OTHER
    restore_owner_authority(runtime, project, packages, changed)
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait('fresh retry preserves draft after validation rejection', lambda: RESOURCE['source'](browser) == original)
    print(json.dumps({'mode': project['mode'], 'boundary': 'before_real_validation_authorization',
        'actual_authorization_route_kind': validation['kind'],
        'actual_authorization_status': validation['status'], 'actual_authorization_error': validation.get('error'),
        'version_replacement': replacement is not None,
        'no_journal_intent': True, 'no_command_approval': True,
        'no_file_or_editor_mutation': True}), flush=True)


def revoke_admission(runtime, browser, proxy, user, project, folder, packages, original, command, retained, replacement=None):
    transactions = f"/api/projects/{project['id']}/workspace-transactions"
    wait('actual initial native worker admission paused before forwarding', proxy.paused.is_set)
    row = proxy.rows[-1]
    assert row['mode'] == project['mode'] and row['has_manual_grant']
    assert len(row['original_review_digest']) == 64
    assert not row['original_scope'].get('accepted_language_review_digest')
    assert row['original_scope'].get('worker') is None
    prepared = runtime.request('GET', transactions)
    assert prepared['journal']['id'] == row['id']
    assert prepared['journal']['generation'] == row['generation']
    assert prepared['journal']['progress']['phase'] == 'prepared'
    assert prepared['language_review']['input'] == retained['input']
    assert prepared['language_review']['context'] == retained['analysisContext']
    assert prepared['language_review']['result'] == retained['analysisResult']
    validation = script(browser, "return resourceProbe.native.filter(r=>r.query.operation==='validate_analysis').slice(-1)[0];")
    assert validation['status'] == 200 and validation['server'] == retained['server']
    unaccepted(runtime, user, project, prepared)
    changed = change_owner_authority(runtime, project, packages, replacement)
    if replacement is not None:
        assert prepared['language_review']['prepared']['digest'] == changed['previous_digest']
        assert prepared['language_review']['binding_revision'] < changed['current_binding_revision']
    revoked = time.monotonic()
    proxy.release.set()
    wait('real native worker admission rejected by backend', proxy.replied.is_set)
    assert row['forwarded'] and not row.get('latch_expired')
    assert row['paused_monotonic'] < revoked < row['forwarded_monotonic'] < row['reply_monotonic']
    assert row['status'] == 409 and row['error'] == 'Plugin is no longer enabled at the selected version', row
    assert row.get('result') != 'worker_claimed'
    rejected = runtime.request('GET', transactions)
    assert rejected['journal'] == prepared['journal']
    assert rejected['language_review'] == prepared['language_review']
    unaccepted(runtime, user, project, rejected)
    assert RESOURCE['source'](browser) == original
    assert not (folder / 'created.notes').exists()
    assert (folder / 'main.notes').read_bytes() == SOURCE.encode()
    assert saved(runtime, project, 'unrelated.notes') == OTHER
    assert not any(event.get('event') == 'command_executed'
                   and event.get('root') == folder.resolve().as_uri() + '/'
                   for event in events(Path(proxy.peer_log)))
    close(browser)
    wait('untouched Prepared cancellation available', lambda: click(browser, 'Restore interrupted changes'))
    wait('untouched Prepared cancellation committed', lambda:
         (runtime.request('GET', transactions) or {}).get('journal', {}).get('progress', {}).get('phase') == 'restored')
    unaccepted(runtime, user, project, runtime.request('GET', transactions))
    if command:
        wait('unstarted command discard available', lambda: click(browser, 'Discard unstarted command'))
        wait('unstarted command cancelled without execution', lambda:
             (runtime.request('POST', f"/api/projects/{project['id']}/language-actions",
                              {'operation': 'load', 'id': None}) or {}).get('phase') == 'cancelled')
    restore_owner_authority(runtime, project, packages, changed)
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait('fresh retry preserves the original draft', lambda: RESOURCE['source'](browser) == original)
    print(json.dumps({'mode': project['mode'], 'command': command,
        'boundary': 'actual_initial_native_worker_admission_callback', 'admission_status': row['status'],
        'version_replacement': replacement is not None,
        'revocation_precedes_forwarding': True, 'accepted_worker_absent': True,
        'accepted_review_digest_absent': True, 'prepared_journal_untouched_then_cancelled': True,
        'no_file_or_editor_mutation_or_command_dispatch': True}), flush=True)




def replacement_review(browser, original, has_command, late_reply):
    title = 'Create file and run reviewed command' if has_command else 'Create file with reviewed draft'
    apply = 'Apply edits and run command' if has_command else 'Apply file operations and editor edits'
    first = original
    if late_reply:
        script(browser, r'''
            window.followupReview={hold:true,held:false,delivered:false};
            const earlierFetch=window.fetch;
            window.fetch=async function(...args){
                const url=String(args[0]?.url??args[0]);
                let input,prepared;
                if(/\/plugins\/languages\/query$/.test(new URL(url,location.href).pathname)){
                    const body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());
                    input=body.input??body.selection?.input;prepared=body.prepared??body.selection?.prepared;
                }
                const response=await earlierFetch.apply(this,args);
                if(followupReview.hold&&input?.query?.operation==='review_action'&&prepared?.manifest?.name==='review-owner'){
                    const data=await response.clone().json();
                    if(response.status===200&&!data.error&&data.context){
                        followupReview.hold=false;followupReview.held=true;
                        await new Promise(resolve=>followupReview.release=resolve);
                        followupReview.delivered=true;
                    }
                }
                return response;
            };
        ''')
        RESOURCE['open_action'](browser, title)
        wait('actual successful A review reply held before delivery', lambda: script(browser, 'return followupReview.held;'))
        first = script(browser, "return resourceProbe.native.filter(r=>r.query.operation==='review_action').slice(-1)[0];")
        assert first['status'] == 200 and first['server'] == original['server']
        wait('pending A review has closed its old result dialog', lambda: script(browser,
            "return !document.querySelector('[role=dialog]');"))
        # This normal editor command supersedes the request without changing
        # source or bypassing a disabled provider-fix control.
        script(browser, "document.querySelector('textarea[data-editor-path]').focus();")
        RESOURCE['keys'](browser, '\ue03c')
        wait('explicit F12 superseding request completed', lambda: script(browser,
            "return resourceProbe.native.some(r=>r.query.operation==='definition'&&r.completed);"))
    else:
        close(browser)
    checkpoint = script(browser, 'return resourceProbe.timeline.length;')
    menu_command(browser, 'Problems (2)')
    wait('B owning-provider fixes control', lambda: script(browser, '''
        const section=[...document.querySelectorAll('[role=dialog] section[aria-label]')]
            .find(s=>s.getAttribute('aria-label').includes('review-analyzer'));
        const button=section&&[...section.querySelectorAll('button')]
            .find(b=>b.textContent.trim()==='Find fixes'&&!b.disabled);
        if(!button)return false;button.click();return true;
    '''))
    wait('B identical-looking action', lambda: click(browser, title))
    wait('B complete resource review', lambda: script(browser,
        "return [...document.querySelectorAll('[role=dialog] button')].some(b=>b.textContent.trim()===arguments[0]&&!b.disabled);", apply))
    second = script(browser, "return resourceProbe.native.filter(r=>r.query.operation==='review_action').slice(-1)[0];")
    assert second['status'] == 200 and second['server'] != first['server']
    assert second['input']['source'] == first['input']['source']
    assert second['input']['revision'] == first['input']['revision']
    assert second['analysisContext'] != first['analysisContext']
    assert second['analysisResult']['edit'] == first['analysisResult']['edit']
    assert second['analysisResult']['title'] == first['analysisResult']['title']
    if has_command:
        left, right = first['analysisResult']['command'], second['analysisResult']['command']
        assert left['name'] == right['name'] and left['arguments'] == right['arguments']
        assert left['id'] != right['id'] and left['server'] != right['server']
    else:
        assert second['analysisResult'] == first['analysisResult']
    if late_reply:
        script(browser, 'followupReview.release();followupReview.release=null;')
        wait('old A native response delivered after B review', lambda: script(browser, 'return followupReview.delivered;'))
        assert script(browser, "return [...document.querySelectorAll('[role=dialog] button')].some(b=>b.textContent.trim()===arguments[0]&&!b.disabled);", apply)
    return checkpoint, second


def check(scenario):
    revoke_at_admission = scenario in ('admission', 'replace-admission')
    revoke_before_validation = scenario in ('authorization', 'replace-authorization')
    replace_review = scenario in ('identical', 'late-reply')
    replace_version = scenario in ('replace-authorization', 'replace-admission')
    completed = []
    with tempfile.TemporaryDirectory(prefix='independent-review-') as state_path, \
            tempfile.TemporaryDirectory(prefix='review-project-', dir=ROOT.parent) as project_path:
        state, project_root = Path(state_path), Path(project_path)
        control, log = state / 'diagnostic-control.json', state / 'lsp.jsonl'
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        proxy = AdmissionProxy(runtime)
        proxy.peer_log = str(log)
        host, browser, daemon = ExecutionHost(runtime, backend_url=proxy.url), None, None
        proxy.secret = host.secret
        mirror = ProbeMirror(runtime.url)
        try:
            daemon, url, commit = repository(state, control, log)
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'review-' + secrets.token_hex(6),
                'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            host.initialize_defaults()
            packages = [NAV['install'](runtime, url, commit, name)
                        for name in ('review-owner', 'review-analyzer')]
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value,
                    'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument',
                'params': {'source': EVIDENCE + '\nresourceProbe.journalProxy=' + json.dumps(proxy.url) + ';'}})
            for mode in ('remote', 'local'):
                for command in (False, True):
                    folder = project_root / (mode + ('-command' if command else '-resource'))
                    folder.mkdir()
                    baseline = {'main.notes': SOURCE, 'unrelated.notes': OTHER}
                    for path, text in baseline.items():
                        (folder / path).write_bytes(text.encode())
                    subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                    relative = folder.relative_to(ROOT.parent.parent).as_posix()
                    project = runtime.request('POST', '/api/projects', {'name': folder.name, 'mode': mode,
                        'path': relative if mode == 'remote' else folder.name}, expected=201)
                    endpoint = f"/api/projects/{project['id']}/plugins"
                    for installation, package in packages:
                        bindings = runtime.request('GET', endpoint)
                        binding = next((entry for entry in bindings
                            if entry['prepared']['source'] == installation['prepared']['source']), None)
                        runtime.request('POST', endpoint, {'action': 'enable', 'package': package,
                            'installation_revision': installation['revision'],
                            'revision': binding['revision'] if binding else None})
                    if mode == 'local':
                        local_handle(browser, project['id'], user, folder.name, baseline)
                        mirror.install(browser, project['id'], folder,
                            files=['main.notes', 'unrelated.notes', 'created.notes'], native_view=True)
                    drafts = []
                    for path, text in [('main.notes', DRAFT), ('unrelated.notes', OTHER)]:
                        drafts.append({'path': path, 'document': {'text': base64.b64encode(text.encode()).decode(),
                            'saved': base64.b64encode(baseline[path].encode()).decode(),
                            'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []},
                            'scroll': {'top': 0, 'left': 0}, 'read_only': False})
                    runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0,
                        'state': {'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None},
                                  'selected': 'main.notes', 'files': drafts}})
                    for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
                        runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
                    browser.call('POST', '/url', {'url': runtime.url + '/'})
                    wait('original draft', lambda: RESOURCE['source'](browser) == DRAFT.replace('\r\n', '\n'))
                    NAV['wait_problems'](browser, 1)
                    insert(browser, '!')
                    original = RESOURCE['source'](browser)
                    checkpoint = script(browser, 'return resourceProbe.timeline.length;')
                    RESOURCE['open_action'](browser, 'Create file and run reviewed command' if command
                                            else 'Create file with reviewed draft')
                    apply = 'Apply edits and run command' if command else 'Apply file operations and editor edits'
                    wait('retained A review', lambda: script(browser,
                        "return [...document.querySelectorAll('[role=dialog] button')].some(b=>b.textContent.trim()===arguments[0]&&!b.disabled);", apply))
                    wait('original B diagnostic visible', lambda: script(browser,
                        "return document.querySelectorAll('.editor-diagnostic').length===1;"))
                    retained = script(browser, "return resourceProbe.native.filter(r=>r.query.operation==='review_action').slice(-1)[0];")
                    assert retained['status'] == 200 and retained['server']
                    nonce = secrets.token_hex(8)
                    control.write_text(json.dumps({'root': folder.resolve().as_uri() + '/', 'nonce': nonce}))
                    wait('B notification acknowledged outside project', lambda: any(row.get('event') == 'refresh_published'
                        and row.get('nonce') == nonce and row.get('versions') for row in events(log)))
                    wait('B refresh delivered through production client', lambda: script(browser,
                        "return resourceProbe.native.some(r=>r.server!==arguments[0]&&r.diagnostics?.some(d=>d.message===arguments[1]));",
                        retained['server'], 'Independent note ' + nonce))
                    wait('refreshed B diagnostics published in editor', lambda: script(browser,
                        "return document.querySelectorAll('.editor-diagnostic').length===2;"))
                    assert RESOURCE['source'](browser) == original
                    assert not (folder / 'created.notes').exists()
                    assert script(browser, "return [...document.querySelectorAll('[role=dialog] button')].some(b=>b.textContent.trim()===arguments[0]&&!b.disabled);", apply)
                    if replace_review:
                        checkpoint, retained = replacement_review(browser, retained, command, scenario == 'late-reply')
                    replacement = prepare_owner_update(runtime, state, url, packages) if replace_version else None
                    if revoke_at_admission:
                        proxy.arm()
                    if revoke_before_validation:
                        script(browser, 'resourceProbe.holdValidation=true;')
                    assert click(browser, apply)
                    if revoke_at_admission:
                        revoke_admission(runtime, browser, proxy, user, project, folder, packages, original, command, retained, replacement)
                    if revoke_before_validation:
                        revoke_pending_validation(runtime, browser, project, folder, packages, original, replacement)
                    if revoke_at_admission or revoke_before_validation:
                        checkpoint = script(browser, 'return resourceProbe.timeline.length;')
                        RESOURCE['open_action'](browser, 'Create file and run reviewed command' if command
                                                else 'Create file with reviewed draft')
                        wait('fresh explicit retry review', lambda: click(browser, apply))
                    transactions = f"/api/projects/{project['id']}/workspace-transactions"
                    wait('retained A transaction committed', lambda: (runtime.request('GET', transactions) or {}).get('journal', {}).get('progress', {}).get('phase') == 'committed')
                    if command:
                        wait('retained A command completed', lambda: (runtime.request('POST', f"/api/projects/{project['id']}/language-actions", {'operation': 'load', 'id': None}) or {}).get('phase') == 'completed')
                    wait('original reviewed draft published', lambda: RESOURCE['source'](browser) == REVIEWED)
                    assert_review_session(browser, mode, checkpoint, command)
                    assert (folder / 'main.notes').read_bytes() == SOURCE.encode()
                    assert (folder / 'unrelated.notes').read_bytes() == OTHER.encode()
                    assert (folder / 'created.notes').read_bytes() == b''
                    rows = events(log)
                    executed = [row for row in rows if row.get('event') == 'command_executed'
                                and row.get('root') == folder.resolve().as_uri() + '/']
                    expected_provider = 'diagnostics' if replace_review else 'language'
                    assert len(executed) == int(command) and all(row['role'] == expected_provider for row in executed)
                    journal = runtime.request('GET', transactions)
                    expected = packages[1 if replace_review else 0][0]['prepared']
                    assert journal['language_review']['prepared']['digest'] == expected['digest']
                    assert journal['language_review']['prepared']['source'] == expected['source']
                    if replace_version:
                        current = next(entry for entry in runtime.request('GET', endpoint)
                                       if entry['prepared']['manifest']['name'] == 'review-owner')
                        assert journal['language_review']['binding_revision'] == current['revision']
                        assert journal['language_review']['prepared']['manifest']['version'] == expected['manifest']['version']
                    if replace_review:
                        assert any(row['role'] == 'diagnostics' and row.get('method') == 'textDocument/codeAction' for row in rows)
                        assert journal['language_review']['input'] == retained['input']
                        assert journal['language_review']['context'] == retained['analysisContext']
                        assert journal['language_review']['result'] == retained['analysisResult']
                        assert journal['language_review']['prepared']['manifest']['name'] == 'review-analyzer'
                        with closing(sqlite3.connect((runtime.state / 'sqlite_db.db').as_uri() + '?mode=ro', uri=True)) as database:
                            encoded, accepted, worker = database.execute(
                                'SELECT language_review,accepted_language_review_digest,worker_id '
                                'FROM workspace_transactions WHERE user_id=? AND project_id=? AND id=? AND generation=?',
                                (user, project['id'], journal['journal']['id'], journal['journal']['generation'])).fetchone()
                        assert json.loads(encoded) == journal['language_review']
                        assert accepted == hashlib.sha256(encoded.encode()).hexdigest() and len(worker) == 32
                    else:
                        assert not any(row['role'] == 'diagnostics' and row.get('method') == 'textDocument/codeAction' for row in rows)
                    close(browser)
                    script(browser, "document.querySelector('textarea[data-editor-path]').focus();")
                    RESOURCE['keys'](browser, '\ue009', 'z')
                    wait('original draft undo history', lambda: RESOURCE['source'](browser) == original)
                    wait('original draft persisted', lambda: saved(runtime, project, 'main.notes').replace('\r\n', '\n') == original)
                    assert saved(runtime, project, 'unrelated.notes') == OTHER
                    assert not script(browser, 'return resourceProbe.errors;')
                    print(json.dumps({'mode': mode, 'command': command, 'scenario': scenario,
                        'public_independent_packages': True, 'source_neutral_refresh': True,
                        'retained_original_review_and_service': True, 'approved_provider': expected_provider,
                        'single_approved_execution': True, 'no_other_provider_execution': True,
                        'disk_sources_unchanged': True, 'drafts_and_undo_preserved': True,
                        'version_replacement': replace_version,
                        'accepted_package_version': expected['manifest']['version']}), flush=True)
                    completed.append((mode, command))
        except BaseException:
            failure = EVIDENCE_ROOT / ('failure-state-' + str(time.time_ns()))
            failure.mkdir(mode=0o700)
            # Preserve a consistent readonly view before the fixture's existing
            # finally removes its temporary database and project roots.
            try:
                backup_deadline = time.monotonic() + 5
                def backup_progress(_status, _remaining, _total):
                    if time.monotonic() >= backup_deadline:
                        raise TimeoutError('Owned failure database backup exceeded five seconds')
                with closing(sqlite3.connect((runtime.state / 'sqlite_db.db').as_uri() + '?mode=ro', uri=True)) as source_db, \
                        closing(sqlite3.connect(failure / 'state.sqlite3')) as destination_db:
                    source_db.backup(destination_db, pages=64, progress=backup_progress, sleep=.05)
            except Exception as error:
                (failure / 'database-backup-error.txt').write_text(repr(error))
            physical = {}
            for path in sorted(project_root.rglob('*')):
                info = path.lstat()
                row = {'device': info.st_dev, 'inode': info.st_ino, 'mode': info.st_mode,
                       'bytes': info.st_size, 'mtime_ns': info.st_mtime_ns, 'ctime_ns': info.st_ctime_ns}
                if path.is_file() and not path.is_symlink():
                    row['sha256'] = sha(path)
                physical[str(path.relative_to(project_root))] = row
            (failure / 'physical-files.json').write_text(json.dumps(physical, indent=2) + '\n')
            if browser is not None:
                try:
                    snapshot = script(browser, "return {trace:resourceProbe.timeline,errors:resourceProbe.errors,dialogs:[...document.querySelectorAll('[role=dialog]')].map(n=>n.textContent)};")
                    (failure / 'browser-evidence.json').write_text(json.dumps(snapshot, indent=2) + '\n')
                    print(json.dumps(snapshot), flush=True)
                except Exception:
                    pass
            for name, path in [('runtime', runtime.state / 'runtime.log'), ('bridge', host.state / 'bridge.log'), ('lsp', log)]:
                if path.exists():
                    (failure / (name + '.log')).write_bytes(path.read_bytes())
            print(json.dumps({'preserved_failure_state': str(failure)}), flush=True)
            raise
        finally:
            proxy.release.set()
            if browser is not None:
                try:
                    script(browser, 'resourceProbe.releaseValidation?.();')
                except Exception:
                    pass
            if browser is not None:
                browser.stop()
            mirror.stop()
            runtime.stop()
            host.stop()
            proxy.stop()
            if daemon is not None:
                daemon.terminate()
                daemon.wait(timeout=10)


    assert len(completed) == 4 and set(completed) == {(mode, command)
        for mode in ('remote', 'local') for command in (False, True)}
    print(json.dumps({'scenario': scenario, 'completed_mode_kind_cases': 4}), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--case', choices=['all', *SCENARIOS], default='all')
    parser.add_argument('--evidence-dir', type=Path)
    args = parser.parse_args()
    EVIDENCE_ROOT = args.evidence_dir.resolve() if args.evidence_dir else Path(tempfile.mkdtemp(prefix='openwebide-language-reviews-evidence-'))
    EVIDENCE_ROOT.mkdir(mode=0o700, parents=True, exist_ok=True)
    for scenario in (SCENARIOS if args.case == 'all' else [args.case]):
        check(scenario)
