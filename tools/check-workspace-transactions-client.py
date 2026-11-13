#!/usr/bin/env python3
"""Production editor/LSP/journal/filesystem contract in both project modes.

Uses the published HTML component with a deterministic stdio LSP executable.
The only substitutions are a private executable PATH, a paused journal transport
and OPFS/native discovery-marker correspondence. Feature behavior runs in the app.
OPFS picker regrant is not a physical Chrome folder-permission prompt test.
"""
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import runpy
import secrets
import stat
import subprocess
import sys
import tempfile
from threading import Event, Lock, Thread
import urllib.error
import urllib.request

from language_host_support import ExecutionHost
from language_review_support import assert_review_session as assert_original_review_session
from language_server_client_support import ProbeMirror, click, close, command, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
DISK = '<h1>disk 文😀</h1>\r\n'
DRAFT = '<h1>draft 文😀</h1>\r\n'
OTHER = '<p>unrelated draft 文😀</p>\r\n'
STAGE = '<section>stage 文😀</section>\r\n'
FINAL = '<section>final 文😀</section>\r\n'
PATHS = ['page.html', 'moved.html', 'created.html', 'unrelated.html', 'assets/gone.txt', 'recover.txt', 'unapproved.txt', 'command-recover.html']
EVIDENCE = r'''
window.resourceProbe={errors:[],pause:false,paused:false,native:[],timeline:[],clock:0};
addEventListener('error',e=>resourceProbe.errors.push(String(e.message)));
addEventListener('unhandledrejection',e=>resourceProbe.errors.push(String(e.reason)));
const resourceFetch=window.fetch;
window.fetch=async function(...args) {
 const url=String(args[0]?.url??args[0]),method=args[1]?.method??args[0]?.method;
 if(resourceProbe.pauseCommand&&method==='POST'&&/\/(?:language-commands|plugins\/languages\/command)$/.test(new URL(url,location.href).pathname)) {
  const data=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());
  if((data.command?.operation??data.operation)==='start') {resourceProbe.commandPaused=true;return new Promise(()=>{});}
 }
 // Fault injection stays at the real native journal transport. Local sends
 // one complete Execute; rewrite only its backend destination to this probe.
 if(method==='POST'&&new URL(url,location.href).pathname==='/workspace/files'&&resourceProbe.journalProxy) {
  const request=new Request(args[0] instanceof Request?args[0].clone():args[0],args[1]),data=await request.clone().json();
  if(data.action?.operation==='execute') {
   data.action.api_url=resourceProbe.journalProxy;
   args=[new Request(request,{body:JSON.stringify(data)})];
  }
 }
 const pathname=new URL(url,location.href).pathname;
 const native=/\/plugins\/languages\/query$/.test(pathname),folder=pathname==='/workspace/files';
 const transaction=method==='POST'&&/\/projects\/\d+\/workspace-transactions(?:\/[a-f0-9]{32}\/execute)?$/.test(pathname);
 const grant=method==='POST'&&/\/plugins\/execution-grants$/.test(pathname);
 const trace=native||folder||transaction||grant?{kind:native?'language':folder?'folder':transaction?'transaction':'grant',began:++resourceProbe.clock}:null;
 if(trace){try{const body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());
   if(native){const input=body.input??body.selection?.input;Object.assign(trace,{input,query:input?.query,path:input?.path,request:input?.request,revision:input?.revision});
     resourceProbe.native.push(trace);}
   else if(folder) Object.assign(trace,{operation:body.action?.operation,fileOperation:body.action?.action?.operation,session:body.action?.session,sequence:body.action?.sequence,hasGrant:/^[a-f0-9]{32}$/.test(body.action?.request?.language_grant??'')});
   else if(grant) Object.assign(trace,{context:body.context,packages:body.plugins?.map(plugin=>plugin.digest)});
   else Object.assign(trace,{operation:pathname.endsWith('/execute')?'execute':'begin',id:body.id,body:pathname.endsWith('/execute')?undefined:body,hasGrant:/^[a-f0-9]{32}$/.test(body.language_grant??'')});
   resourceProbe.timeline.push(trace);}catch{}}
 const response=await resourceFetch.apply(this,args);
 if(trace){try{const data=await response.clone().json();Object.assign(trace,{completed:++resourceProbe.clock,status:response.status,error:data.error,server:data.server,result:folder?data.result:undefined,analysisContext:native?data.context:undefined,analysisResult:native?data.result:undefined,grantDigests:grant?Object.keys(data):undefined,openedSession:folder?data.session:undefined,titles:Array.isArray(data.result)?data.result.map(x=>x.title):undefined});}catch{}}
 return response;
};
'''


class JournalProxy:
    """Pause exactly one native cursor acknowledgement, without inventing results."""
    def __init__(self, runtime):
        self.runtime, self.paused, self.release, self.lock = runtime, Event(), Event(), Lock()
        self.armed = False
        self.admission_armed, self.admitted = False, Event()
        self.secret = None
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def relay(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                data = json.loads(body) if body else {}
                operation = data.get('operation', {})
                progress = operation.get('progress', {})
                remote = (proxy.secret is not None and self.headers.get('Authorization') == 'Bearer ' + proxy.secret
                          and self.path == '/api/workspace-transactions/service')
                paired = (self.path == '/api/workspace-transactions/delegated'
                          and isinstance(data.get('delegation'), dict) and isinstance(data.get('worker'), dict))
                with proxy.lock:
                    admission_pause = (proxy.admission_armed and (remote or paired)
                                       and operation.get('command') == ('claim_worker' if remote else 'admit'))
                    if admission_pause:
                        proxy.admission_armed = False
                    pause = (proxy.armed and (remote or paired)
                             and operation.get('command') == ('advance_owned' if remote else 'advance')
                             and progress.get('phase') == 'applying'
                             and progress.get('completed') == 1 and not progress.get('active'))
                    if pause:
                        proxy.armed = False
                if pause:
                    proxy.paused.set()
                    proxy.release.wait(45)
                    status, response, content_type = 503, b'{"error":"Interrupted probe transport"}', 'application/json'
                else:
                    headers = {name: value for name, value in self.headers.items()
                               if name.lower() not in ('host', 'content-length', 'connection')}
                    request = urllib.request.Request(runtime.url + self.path, data=body or None,
                                                     headers=headers, method=self.command)
                    reply = None
                    try:
                        reply = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=30)
                    except urllib.error.HTTPError as error:
                        reply = error
                    except urllib.error.URLError:
                        status, response, content_type = 503, b'{"error":"Probe backend not ready"}', 'application/json'
                    if reply is not None:
                        with reply:
                            status, response, content_type = reply.code, reply.read(), reply.headers.get('Content-Type', 'application/json')
                    if admission_pause and status == 200:
                        claimed = json.loads(response)
                        assert claimed['result'] == 'worker_claimed' and claimed['scope']['worker']['active']
                        proxy.admitted.set()
                        proxy.release.wait(45)
                        status, response = 503, b'{"error":"Admission reply lost after commit"}'
                try:
                    self.send_response(status)
                    self.send_header('Content-Type', content_type)
                    self.send_header('Content-Length', str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            do_GET = do_POST = do_PUT = do_DELETE = relay

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.url = f'http://127.0.0.1:{self.server.server_port}/api'
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def arm(self):
        self.paused.clear()
        self.release.clear()
        with self.lock:
            self.armed = True

    def arm_admission(self):
        self.admitted.clear()
        self.release.clear()
        with self.lock:
            self.admission_armed = True

    def stop(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def source(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def normalize(value):
    return value.replace('\r\n', '\n')


def keys(browser, *values):
    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'resources', 'actions':
        [{'type': 'keyDown', 'value': value} for value in values]
        + [{'type': 'keyUp', 'value': value} for value in reversed(values)]}]})


class ProjectFiles:
    """Filesystem/permission primitives, shared test scenario above this adapter."""
    def __init__(self, browser, folder, project, user):
        self.browser, self.folder, self.project, self.user = browser, folder, project, user
        self.mode = project['mode']
        self.original_mode = stat.S_IMODE(folder.stat().st_mode)

    def snapshot(self):
        native = {path: ((self.folder / path).read_bytes().decode() if (self.folder / path).is_file() else None) for path in PATHS}
        if self.mode == 'remote':
            return native
        browser = self.browser.call('POST', '/execute/async', {'script': '''
            const [name, paths] = arguments, done=arguments[arguments.length-1];
            (async()=>{await window.refreshNativeProbeView();
              const root=await(await navigator.storage.getDirectory()).getDirectoryHandle(name),result={};
              for(const path of paths){try{let parent=root;const parts=path.split('/'),leaf=parts.pop();
                for(const part of parts)parent=await parent.getDirectoryHandle(part);
                result[path]=await(await(await parent.getFileHandle(leaf)).getFile()).text();
              }catch(error){if(error.name!=='NotFoundError')throw error;result[path]=null;}}return result;
            })().then(done,error=>done({error:String(error)}));
        ''', 'args': [self.folder.name, PATHS]})
        assert browser == native, 'The OPFS test view diverged from actual native filesystem operations'
        return native

    def lose_access(self, journal):
        if self.mode == 'remote':
            self.folder.chmod(0)
        else:
            assert self.browser.call('POST', '/execute/async', {'script': '''
                const key=arguments[0],done=arguments[arguments.length-1],open=indexedDB.open('openwebide',1);
                open.onsuccess=()=>{const db=open.result,tx=db.transaction('directories','readwrite');
                  tx.objectStore('directories').delete(key);tx.oncomplete=()=>{db.close();done(true);};};
            ''', 'args': [f"transaction-root:{self.user}:{self.project['id']}:{journal}"]}) is True

    def grant_access(self):
        if self.mode == 'remote':
            self.folder.chmod(self.original_mode)
            assert click(self.browser, 'Check workspace recovery')
        else:
            script(self.browser, '''
                window.savedResourcePicker=window.showDirectoryPicker;
                window.showDirectoryPicker=async()=>await(await navigator.storage.getDirectory()).getDirectoryHandle(arguments[0]);
            ''', self.folder.name)
            assert click(self.browser, 'Choose original folder')
            wait('reviewed original-folder grant', lambda: click(self.browser, 'Grant this folder'))
            wait('original folder retained', lambda: script(self.browser, "return ![...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Choose original folder');"))
            script(self.browser, 'window.showDirectoryPicker=window.savedResourcePicker;')

    def restore_permissions(self):
        self.folder.chmod(self.original_mode)


def open_action(browser, title):
    command(browser, 'Code actions')
    wait(title, lambda: click(browser, title))




def command_contract(runtime, host, browser, state, folder, project, files, proxy):
    endpoint = f"/api/projects/{project['id']}/language-actions"
    current_id = None

    def record():
        return runtime.request('POST', endpoint, {'operation': 'load', 'id': None})

    def phase(expected):
        current = record() or {}
        return current.get('phase') == expected and current.get('approval', {}).get('id') == current_id

    def executions():
        root = folder.resolve().as_uri() + '/'
        return [json.loads(line) for line in (state / 'lsp.jsonl').read_text().splitlines()
                if json.loads(line).get('method') == 'workspace/executeCommand' and json.loads(line).get('root') == root]

    def approve(title):
        nonlocal current_id
        before = len(executions())
        previous_id = (record() or {}).get('approval', {}).get('id')
        open_action(browser, title)
        wait('complete command review', lambda: script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes('fixture.execute')&&[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Apply edits and run command'&&!b.disabled);"))
        assert len(executions()) == before, 'Review executed a command'
        assert click(browser, 'Apply edits and run command')
        wait('new command approval persisted', lambda: (record() or {}).get('approval', {}).get('id') not in (None, previous_id))
        current_id = record()['approval']['id']

    for title in ('Run reviewed command only', 'Run reviewed edit and command'):
        before = len(executions())
        approve(title)
        wait('approved command completed durably', lambda: phase('completed'))
        assert len(executions()) == before + 1
        retained = record()
        assert retained['approval']['review']['command']['name'] == 'fixture.execute'
        journal = runtime.request('GET', f"/api/projects/{project['id']}/workspace-transactions/{retained['approval']['id']}")
        assert journal['journal']['progress']['phase'] == 'committed'
        close(browser)

    before = len(executions())
    script(browser, 'resourceProbe.pauseCommand=true;')
    approve('Run command with lost dispatch')
    wait('durable claim before lost dispatch', lambda: script(browser, 'return resourceProbe.commandPaused;') and phase('running'))
    assert len(executions()) == before
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait('lost dispatch discovered after reload', lambda: click(browser, 'Check command outcome'))
    wait('read-only command check finished', lambda: script(browser, "return [...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Stop pending command');"))
    assert phase('running') and len(executions()) == before, 'Reload replayed a claimed command'
    assert click(browser, 'Stop pending command')
    wait('unstarted command fenced', lambda: phase('rejected'))
    wait('unstarted command acknowledged', lambda: click(browser, 'Acknowledge reviewed outcome'))
    wait('unstarted command released', lambda: phase('acknowledged'))
    assert len(executions()) == before

    approve('Run command with delayed outcome')
    wait('real command running in this window', lambda: len(executions()) == before + 1 and phase('running'))
    wait('command approval dialog closed', lambda: script(browser, "return !document.querySelector('[role=dialog]');"))
    script(browser, "const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(input.value.length,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
    insert(browser, ' draft while running')
    wait('draft saved while command runs', lambda: any(base64.b64decode(file['document']['text']).decode().endswith(' draft while running')
        for file in runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")['state']['files'] if file.get('document')))
    assert click(browser, 'Stop pending command')
    wait('same-window command stopped', lambda: phase('uncertain'))
    wait('same-window effects acknowledged', lambda: click(browser, 'Stop and acknowledge reviewed effects'))
    wait('same-window command released', lambda: phase('acknowledged'))
    assert source(browser).endswith(' draft while running') and len(executions()) == before + 1

    approve('Run command with delayed outcome')
    wait('real command executed before disconnect', lambda: len(executions()) == before + 2 and phase('running'))
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait('running command discovered after reload', lambda: click(browser, 'Stop pending command'))
    wait('interrupted command outcome retained', lambda: phase('uncertain'))
    wait('uncertain command acknowledged', lambda: click(browser, 'Stop and acknowledge reviewed effects'))
    wait('interrupted command released', lambda: phase('acknowledged'))
    assert len(executions()) == before + 2, 'Interrupted command replayed'

    approve('Run command with uncertain outcome')
    wait('server error retains uncertainty', lambda: phase('uncertain'))
    assert len(executions()) == before + 3, 'Server error used read retry'
    wait('server error explicitly acknowledged', lambda: click(browser, 'Stop and acknowledge reviewed effects'))
    wait('server error released', lambda: phase('acknowledged'))

    # A real file primitive can finish while its acknowledgement is lost. Its
    # attached command remains unclaimed until the entire transaction commits.
    proxy.arm()
    approve('Create interrupted command file')
    wait('command edit interrupted after primitive', lambda: proxy.paused.is_set())
    assert phase('approved') and files.snapshot()['command-recover.html'] == ''
    assert len(executions()) == before + 3
    host.stop_process()
    proxy.release.set()
    host.start()
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait('command edit recovered', lambda: click(browser, 'Restore interrupted changes'))
    wait('unstarted approval retained after rollback', lambda: click(browser, 'Discard unstarted command'))
    wait('rolled-back command cancelled', lambda: phase('cancelled'))
    assert files.snapshot()['command-recover.html'] is None and len(executions()) == before + 3
    assert not script(browser, 'return resourceProbe.errors;')


def command_project(runtime, browser, mirror, parent, mode, user):
    # Give command cases fresh corresponding folders and independent journals.
    folder = parent / (mode + '-commands')
    folder.mkdir()
    baseline = {'page.html': DISK, 'unrelated.html': '<p>unrelated disk</p>\r\n', 'package.json': '{}'}
    for path, text in baseline.items():
        (folder / path).write_bytes(text.encode())
    subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
    relative = folder.relative_to(ROOT.parent.parent).as_posix()
    project = runtime.request('POST', '/api/projects', {'name': 'Commands ' + mode, 'mode': mode,
        'path': relative if mode == 'remote' else folder.name}, expected=201)
    if mode == 'local':
        local_handle(browser, project['id'], user, folder.name, baseline)
        mirror.install(browser, project['id'], folder,
            files=[path for path in PATHS if '/' not in path], native_view=True)
    drafts = [{'path': path, 'document': {'text': base64.b64encode(draft.encode()).decode(),
        'saved': base64.b64encode(baseline[path].encode()).decode(), 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []},
        'scroll': {'top': 0, 'left': 0}, 'read_only': False} for path, draft in [('page.html', DRAFT), ('unrelated.html', OTHER)]]
    runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': {
        'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None}, 'selected': 'page.html', 'files': drafts}})
    for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
        runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
    browser.call('POST', '/url', {'url': runtime.url + '/'})
    wait(mode + ' command editor', lambda: source(browser) == normalize(DRAFT))
    return folder, project, ProjectFiles(browser, folder, project, user)


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-resource-client-') as state_path, \
            tempfile.TemporaryDirectory(prefix='resource-project-', dir=ROOT.parent) as project_path:
        state, project_root = Path(state_path), Path(project_path)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        proxy = JournalProxy(runtime)
        host = ExecutionHost(runtime, backend_url=proxy.url)
        proxy.secret = host.secret
        browser, mirror, files = None, ProbeMirror(runtime.url), None
        previous = {key: os.environ.get(key) for key in ('PATH', 'OPENWEBIDE_RESOURCE_PROBE_LOG')}
        executable = state / 'bin' / 'vscode-html-language-server'
        executable.parent.mkdir()
        fixture = (ROOT / 'tools/fixtures/workspace_edit_server.py').read_text().split('\n', 1)[1]
        executable.write_text('#!' + sys.executable + '\n' + fixture)
        executable.chmod(0o755)
        os.environ['PATH'] = str(executable.parent) + os.pathsep + os.environ['PATH']
        os.environ['OPENWEBIDE_RESOURCE_PROBE_LOG'] = str(state / 'lsp.jsonl')
        try:
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'resources-' + secrets.token_hex(6), 'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            installations = host.initialize_defaults()
            package = next(entry for entry in installations if entry['prepared']['manifest']['name'] == 'html')
            assert package['prepared']['source']['commit'] == 'b29aa4b6da3e29dc3ca3135ba7c5c3a8bc723f2e'
            print('Published HTML component initialized', flush=True)
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value, 'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument', 'params': {'source': EVIDENCE + '\nresourceProbe.journalProxy=' + json.dumps(proxy.url) + ';'}})
            for mode in ('remote', 'local'):
                print(mode + ': starting production resource contract', flush=True)
                folder = project_root / mode
                (folder / 'assets').mkdir(parents=True)
                baseline = {'page.html': DISK, 'unrelated.html': '<p>unrelated disk</p>\r\n', 'assets/gone.txt': 'remove this', 'package.json': '{}'}
                for path, text in baseline.items():
                    (folder / path).write_bytes(text.encode())
                subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name': 'Resources ' + mode, 'mode': mode, 'path': relative if mode == 'remote' else folder.name}, expected=201)
                if mode == 'local':
                    local_handle(browser, project['id'], user, folder.name, baseline)
                    mirror.install(browser, project['id'], folder,
                        files=[path for path in PATHS if '/' not in path], native_view=True)
                files = ProjectFiles(browser, folder, project, user)
                drafts = []
                for path, draft in [('page.html', DRAFT), ('unrelated.html', OTHER)]:
                    drafts.append({'path': path, 'document': {'text': base64.b64encode(draft.encode()).decode(), 'saved': base64.b64encode(baseline[path].encode()).decode(), 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []}, 'scroll': {'top': 0, 'left': 0}, 'read_only': False})
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': {'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None}, 'selected': 'page.html', 'files': drafts}})
                for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id'])}.items():
                    runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait(mode + ' editor', lambda: source(browser) == normalize(DRAFT))
                insert(browser, '!')
                live = source(browser)
                original = files.snapshot()
                open_action(browser, 'Reject command with edits')
                wait('partial command rejected', lambda: script(browser, "return document.body.textContent.includes('did not advertise this command');"))
                assert files.snapshot() == original
                assert runtime.request('GET', f"/api/projects/{project['id']}/workspace-transactions") is None
                checkpoint = script(browser, 'return resourceProbe.timeline.length;')
                open_action(browser, 'Move HTML files with drafts')
                wait('complete resource review', lambda: script(browser, "return [...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Apply file operations and editor edits'&&!b.disabled);"))
                assert files.snapshot() == original
                assert click(browser, 'Apply file operations and editor edits')
                endpoint = f"/api/projects/{project['id']}/workspace-transactions"
                wait('resource transaction committed', lambda: (runtime.request('GET', endpoint) or {}).get('journal', {}).get('progress', {}).get('phase') == 'committed')
                wait('renamed draft published', lambda: source(browser) == normalize(FINAL))
                assert_original_review_session(browser, mode, checkpoint)
                close(browser)
                expected = {**original, 'page.html': None, 'moved.html': DISK, 'created.html': '', 'assets/gone.txt': None}
                assert files.snapshot() == expected
                print(mode + ': approved resource operation committed', flush=True)
                for expected_draft in (STAGE, live):
                    script(browser, "document.querySelector('textarea[data-editor-path]').focus();")
                    keys(browser, '\ue009', 'z')
                    wait('resource text edits preserve undo', lambda: source(browser) == normalize(expected_draft))
                recovered = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                wait('undo draft persisted', lambda: next(file for file in runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")['state']['files'] if file['path'] == 'moved.html')['document']['text'] == base64.b64encode(source(browser).replace('\n', '\r\n').encode()).decode())
                other = next(file for file in recovered['state']['files'] if file['path'] == 'unrelated.html')
                assert base64.b64decode(other['document']['text']).decode() == OTHER
                created = next(file for file in recovered['state']['files'] if file['path'] == 'created.html')
                assert base64.b64decode(created['document']['text']).decode() == '<p>new draft</p>\r\n'
                proxy.arm()
                open_action(browser, 'Create interrupted file')
                wait('interrupted resource review', lambda: click(browser, 'Apply file operations and editor edits'))
                wait('actual primitive finished without cursor acknowledgement', lambda: proxy.paused.is_set())
                pending = runtime.request('GET', endpoint)
                assert pending['journal']['progress']['active'] and pending['journal']['progress']['completed'] == 0
                assert files.snapshot()['recover.txt'] == ''
                host.stop_process()
                proxy.release.set()
                files.lose_access(pending['journal']['id'])
                host.start()
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait('interrupted operation discovered after reload', lambda: click(browser, 'Restore interrupted changes'))
                wait('lost access preserves pending journal', lambda: script(browser, "return document.body.textContent.includes('Workspace recovery needs attention');"))
                assert runtime.request('GET', endpoint)['journal']['progress']['phase'] not in ('committed', 'restored')
                files.grant_access()
                wait('recovered draft available during pending recovery', lambda: bool(source(browser)))
                script(browser, "const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(input.value.length,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
                insert(browser, ' pending')
                pending_draft = source(browser)
                assert script(browser, '''
                    const save=document.querySelector('button[aria-label="Save file (Ctrl/⌘S)"]');
                    if(!save)return false;if(!save.disabled)save.click();return true;
                ''')
                assert files.snapshot()['recover.txt'] == ''
                assert files.snapshot()['moved.html'] == DISK
                wait('explicit interrupted-operation recovery', lambda: click(browser, 'Restore interrupted changes'))
                wait('rollback completed', lambda: runtime.request('GET', endpoint)['journal']['progress']['phase'] == 'restored')
                assert files.snapshot() == expected
                assert source(browser) == pending_draft
                assert not script(browser, 'return resourceProbe.errors;')
                files.restore_permissions()
                proxy.arm_admission()
                open_action(browser, 'Create interrupted file')
                wait('admission-loss review', lambda: click(browser, 'Apply file operations and editor edits'))
                wait('real worker admitted before first intent', lambda: proxy.admitted.is_set())
                admitted = runtime.request('GET', endpoint)
                assert admitted['journal']['progress']['phase'] == 'prepared'
                assert files.snapshot() == expected
                editor_before = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                files.lose_access(admitted['journal']['id'])
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait('cancel admitted untouched review without a folder grant', lambda: click(browser, 'Restore interrupted changes'))
                wait('admitted prepared journal cancelled', lambda: runtime.request('GET', endpoint)['journal']['progress']['phase'] == 'restored')
                proxy.release.set()
                files.restore_permissions()
                assert files.snapshot() == expected
                editor_after = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                texts = lambda record: {entry['path']: (entry.get('document') or {}).get('text') for entry in record['state']['files']}
                assert texts(editor_before) == texts(editor_after)
                folder, project, files = command_project(runtime, browser, mirror, project_root, mode, user)
                command_contract(runtime, host, browser, state, folder, project, files, proxy)
                print(json.dumps({'mode': mode, 'published_html_component': True, 'raw_lsp_resource_edits': True,
                    'no_mutation_before_approval': True, 'ordered_text_and_resource_review': True,
                    'original_review_and_folder_session_retained': True,
                    'durable_language_review_matches_original_reply': True,
                    'fresh_sessionless_manual_grant_before_initial_dispatch': True,
                    'dirty_baselines_and_undo_preserved': True, 'unrelated_draft_retained': True,
                    'edit_plus_command_rejected': True, 'interruption_after_actual_primitive': True,
                    'reload_and_access_regrant': True, 'recovery_preserves_newer_draft': True,
                    'command_only_and_combined_approval': True, 'lost_dispatch_never_replayed': True,
                    'running_and_uncertain_command_recovery': True, 'file_failure_prevents_command': True,
                    'same_window_stop_and_draft_persistence': True,
                    'admitted_prepared_cancellation_without_folder': True}), flush=True)
                files.restore_permissions()
            messages = [json.loads(line) for line in (state / 'lsp.jsonl').read_text().splitlines()]
            denials = [message['response'] for message in messages if message.get('response', {}).get('id') == 'unsolicited']
            assert denials and all(reply.get('error', {}).get('code') == -32601 for reply in denials), 'Unsolicited applyEdit was not explicitly rejected'
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "return {source:document.querySelector('textarea[data-editor-path]')?.value,dialogs:[...document.querySelectorAll('[role=dialog]')].map(n=>n.textContent.slice(0,2000)),notices:[...document.querySelectorAll('[role=alert],[role=status]')].map(n=>n.textContent.slice(0,1500)),body:document.body.textContent.slice(-14000),probe:window.resourceProbe};")), flush=True)
                except Exception:
                    pass
            for name, path in [('runtime', runtime.state / 'runtime.log'), ('bridge', host.state / 'bridge.log'), ('lsp', state / 'lsp.jsonl')]:
                if path.exists():
                    Path('/tmp/openwebide-resource-client-' + name + '.log').write_bytes(path.read_bytes())
            raise
        finally:
            if files is not None:
                files.restore_permissions()
            proxy.release.set()
            if browser is not None:
                browser.stop()
            host.stop()
            runtime.stop()
            proxy.stop()
            mirror.stop()
            for name, value in previous.items():
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value


if __name__ == '__main__':
    check()
