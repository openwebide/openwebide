"""Browser primitives for real language-server editor checks in both modes."""
import base64
import json
import os
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
import secrets
import time

def wait(label, predicate, seconds=45):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = predicate()
        if result:
            return result
        time.sleep(.1)
    raise AssertionError(label + ' timed out')


def script(browser, source, *args):
    return browser.call('POST', '/execute/sync', {'script': source, 'args': list(args)})


def click(browser, text):
    return script(browser, '''
        const button = [...document.querySelectorAll('button')]
            .find(button => button.textContent.trim() === arguments[0] && !button.disabled);
        if (!button) return false; button.click(); return true;
    ''', text)


def close(browser):
    script(browser, "[...document.querySelectorAll('[aria-label=\"Close dialog\"]')].at(-1)?.click();")


def command(browser, label):
    close(browser)
    wait('Code intelligence control', lambda: script(browser, '''
        const button = document.querySelector('button[aria-label="Code intelligence"]');
        if (!button || button.disabled) return false; button.click(); return true;
    '''))
    assert wait(label + ' menu', lambda: click(browser, label))


def caret(browser, token, delta=0, select=False):
    wait('current writable editor for cursor', lambda: script(browser, '''
        const input = document.querySelector('textarea[data-editor-path]');
        return input&&input.dataset.editorNativeValueScope===input.dataset.editorScope
            &&input.dataset.editorNativePreparing==='false'&&!input.readOnly;
    '''))
    assert script(browser, '''
        const input = document.querySelector('textarea[data-editor-path]');
        const start = input?.value.indexOf(arguments[0]);
        if (start == null || start < 0) return false;
        input.focus(); input.setSelectionRange(start + arguments[1], arguments[2] ? start + arguments[0].length : start + arguments[1]);
        input.dispatchEvent(new Event('select', {bubbles:true})); return true;
    ''', token, delta, select), 'Missing cursor token ' + token


def insert(browser, text):
    # Restored text may be installed while the asynchronous file-read lock is
    # still held. Preserve the intended cursor and send native input only once.
    selection = script(browser, '''
        const input = document.querySelector('textarea[data-editor-path]');
        return input && {path:input.dataset.editorPath, project:input.dataset.editorProject,
            value:input.value,
            start:input.selectionStart, end:input.selectionEnd};
    ''')
    assert selection, 'Missing editor for native input'
    wait('writable native input', lambda: script(browser, '''
        const input = document.querySelector('textarea[data-editor-path]');
        return input?.dataset.editorPath===arguments[0].path
            &&input.dataset.editorProject===arguments[0].project
            &&input.value===arguments[0].value
            &&input.dataset.editorNativeValueScope===input.dataset.editorScope
            &&input.dataset.editorNativePreparing==='false'&&!input.readOnly;
    ''', selection))
    assert script(browser, '''
        const input = document.querySelector('textarea[data-editor-path]');
        input.focus();
        return input.selectionStart===arguments[0].start&&input.selectionEnd===arguments[0].end;
    ''', selection), 'Editor lost the intended selection while loading'
    script(browser, '''
        window.recordLspInput ??= (type, event) => {
            if (!window.lspEvidence) return;
            const input = document.querySelector('textarea[data-editor-path]');
            const events = lspEvidence.inputEvents ??= [];
            events.push({type, trusted:event?.isTrusted, inputType:event?.inputType,
                prevented:event?.defaultPrevented, data:event?.data,
                documentFocused:document.hasFocus(), focused:input===document.activeElement,
                connected:input?.isConnected, readOnly:input?.readOnly,
                native:input&&{...input.dataset}, start:input?.selectionStart,
                end:input?.selectionEnd, value:input?.value.slice(0,512)});
            if (events.length > 32) events.shift();
        };
        if (!window.lspInputObserverInstalled) {
            window.lspInputObserverInstalled = true;
            for (const type of ['beforeinput', 'input']) {
                document.addEventListener(type, event => {
                    if (event.target.matches?.('textarea[data-editor-path]')) {
                        queueMicrotask(() => recordLspInput(type, event));
                    }
                }, true);
            }
        }
        recordLspInput('before CDP insertion');
    ''')
    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': text}})
    script(browser, "recordLspInput('after CDP insertion');")


def local_handle(browser, project, user, folder, files):
    result = browser.call('POST', '/execute/async', {'script': '''
        const [project, user, name, files] = arguments, done = arguments[arguments.length-1];
        (async()=> {
            const root = await (await navigator.storage.getDirectory()).getDirectoryHandle(name, {create:true});
            for (const [path, text] of Object.entries(files)) {
                let parent=root; const parts=path.split('/'); const leaf=parts.pop();
                for(const part of parts) parent=await parent.getDirectoryHandle(part,{create:true});
                const file=await parent.getFileHandle(leaf,{create:true});
                const writer=await file.createWritable(); await writer.write(text); await writer.close();
            }
            const db=await new Promise((resolve,reject)=> {const request=indexedDB.open('openwebide',1);
                request.onupgradeneeded=()=>request.result.createObjectStore('directories');
                request.onerror=()=>reject(request.error); request.onsuccess=()=>resolve(request.result);});
            await new Promise((resolve,reject)=> {const tx=db.transaction('directories','readwrite');
                tx.objectStore('directories').put({projectId:project,userId:user,handle:root,revision:'lsp-fixture'},project);
                tx.oncomplete=resolve;tx.onerror=()=>reject(tx.error);}); db.close(); return true;
        })().then(done,error=>done({error:String(error)}));
    ''', 'args': [project, user, folder, files]})
    assert result is True, result


class ProbeMirror:
    """Mirror markers and explicitly selected files between OPFS and native roots.

    OPFS cannot address native files. Only this test correspondence is supplied;
    host selection, grants, LSP requests and source results stay unchanged.
    """
    def __init__(self, origin):
        self.origin, self.token, self.folders = origin, secrets.token_urlsafe(32), {}
        self.files = {}
        self.native_views = set()
        self.script_id = None
        mirror = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_OPTIONS(self):
                self.reply(204)
            def reply(self, status, value=None):
                payload = b'' if value is None else json.dumps(value).encode()
                self.send_response(status)
                self.send_header('Access-Control-Allow-Origin', mirror.origin)
                self.send_header('Access-Control-Allow-Methods', 'POST, DELETE, OPTIONS')
                self.send_header('Access-Control-Allow-Headers', 'content-type, x-lsp-probe')
                self.send_header('Access-Control-Allow-Private-Network', 'true')
                self.send_header('Content-Type', 'application/json')
                self.send_header('Content-Length', str(len(payload)))
                self.end_headers()
                if payload:
                    self.wfile.write(payload)
            def mutate(self):
                if self.headers.get('Origin') != mirror.origin or self.headers.get('x-lsp-probe') != mirror.token:
                    self.reply(403); return
                length = int(self.headers.get('Content-Length', '0'))
                if length < 0 or length > 96 * 1024:
                    self.reply(413); return
                try:
                    body = json.loads(self.rfile.read(length))
                except (ValueError, UnicodeError):
                    self.reply(400); return
                folder=mirror.folders.get(body.get('project'))
                name=body.get('name','')
                if self.command == 'POST' and body.get('action') == 'snapshot':
                    if folder is None or body.get('project') not in mirror.native_views:
                        self.reply(400); return
                    directories, files, total = [], {}, 0
                    try:
                        for parent, children, names in os.walk(folder, followlinks=False):
                            children[:] = [name for name in children if name != '.git' and not name.startswith('.openwebide')]
                            for name in children + names:
                                if name == '.git' or name.startswith('.openwebide'):
                                    continue
                                path = type(folder)(parent) / name
                                if path.is_symlink():
                                    raise ValueError('Fixture snapshots do not follow symlinks')
                                relative = path.relative_to(folder).as_posix()
                                if path.is_dir():
                                    directories.append(relative)
                                else:
                                    if path.stat().st_size > 64 * 1024:
                                        raise ValueError('Fixture file exceeds its limit')
                                    data = path.read_bytes(); total += len(data)
                                    files[relative] = base64.b64encode(data).decode('ascii')
                                if len(directories) + len(files) > 256 or total > 512 * 1024:
                                    raise ValueError('Fixture snapshot exceeds its limit')
                    except (OSError, ValueError):
                        self.reply(400); return
                    self.reply(200, {'directories': directories, 'files': files}); return
                # OPFS cannot see a native challenge file. Copy only the exact
                # public proof shape from this project's declared counterpart;
                # the real host still creates and validates the random proof.
                if self.command == 'POST' and body.get('action') == 'proof':
                    if folder is None or not isinstance(name, str) or not re.fullmatch(r'\.openwebide-proof-[a-f0-9]{32}', name):
                        self.reply(400); return
                    proof = folder / name
                    try:
                        if proof.is_symlink() or proof.stat().st_size != 64:
                            self.reply(400); return
                        content = proof.read_bytes()
                    except OSError:
                        self.reply(404); return
                    if re.fullmatch(rb'[a-f0-9]{64}', content) is None:
                        self.reply(400); return
                    self.reply(200, {'content': content.decode('ascii')}); return
                marker = isinstance(name, str) and re.fullmatch(r'\.openwebide-probe-[A-Za-z0-9_-]{1,128}',name)
                selected = isinstance(name, str) and name in mirror.files.get(body.get('project'), ())
                if folder is None or not (marker or selected):
                    self.reply(400); return
                if self.command=='POST' and selected:
                    try:
                        content = base64.b64decode(body['content'], validate=True)
                        if len(content) > 64 * 1024:
                            self.reply(413); return
                    except (KeyError, TypeError, ValueError):
                        self.reply(400); return
                    (folder/name).write_bytes(content)
                elif self.command=='POST': (folder/name).touch(exist_ok=False)
                else: (folder/name).unlink(missing_ok=True)
                self.reply(204)
            do_POST=mutate
            do_DELETE=mutate
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread=Thread(target=self.server.serve_forever,daemon=True)
        self.thread.start()
    def install(self,browser,project,folder,files=(),native_view=False):
        assert all(isinstance(name, str) and name not in ('', '.', '..')
                   and '/' not in name and '\\' not in name for name in files)
        self.folders[project]=folder
        self.files[project] = set(files)
        if native_view:
            self.native_views.add(project)
        config={'url':f'http://127.0.0.1:{self.server.server_port}', 'token':self.token,'project':project,'root':folder.name,'files':list(files),'nativeView':native_view}
        source='const mirror='+json.dumps(config)+r''';
            async function marker(name,method,content) {
                const response=await fetch(mirror.url,{method,headers:{'content-type':'application/json','x-lsp-probe':mirror.token},body:JSON.stringify({project:mirror.project,name,content})});
                if(!response.ok) throw new Error('Test folder marker mirror failed');
            }
            const getOriginal=FileSystemDirectoryHandle.prototype.getFileHandle;
            FileSystemDirectoryHandle.prototype.getFileHandle=async function(name,options) {
                if(this.name===mirror.root&&/^\.openwebide-proof-[a-f0-9]{32}$/.test(name)&&!options?.create){
                    const response=await fetch(mirror.url,{method:'POST',headers:{'content-type':'application/json','x-lsp-probe':mirror.token},body:JSON.stringify({project:mirror.project,name,action:'proof'})});
                    if(!response.ok)throw new Error('Test native proof correspondence failed');
                    const data=await response.json();
                    if(typeof data.content!=='string'||!/^[a-f0-9]{64}$/.test(data.content))throw new Error('Invalid test proof correspondence');
                    const proof=await getOriginal.call(this,name,{create:true}),writer=await proof.createWritable();
                    await writer.write(data.content);await writer.close();
                }
                const file=await getOriginal.call(this,name,options);
                if(this.name!==mirror.root||(!name.startsWith('.openwebide-probe-')&&!mirror.files.includes(name))) return file;
                const createOriginal=file.createWritable.bind(file);
                file.createWritable=async options=>{const stream=await createOriginal(options),close=stream.close.bind(stream);
                    stream.close=async()=>{await close();let content;
                        if(mirror.files.includes(name)){
                            const bytes=new Uint8Array(await(await file.getFile()).arrayBuffer());
                            if(bytes.length>64*1024)throw new Error('Test mirror file exceeds its limit');
                            content=btoa(bytes.reduce((text,byte)=>text+String.fromCharCode(byte),''));
                        }
                        await marker(name,'POST',content);};return stream;};return file;
            };
            const removeOriginal=FileSystemDirectoryHandle.prototype.removeEntry;
            FileSystemDirectoryHandle.prototype.removeEntry=async function(name,options) {
                await removeOriginal.call(this,name,options);
                if(this.name===mirror.root&&(name.startsWith('.openwebide-probe-')||mirror.files.includes(name))) await marker(name,'DELETE');
            };
            if(mirror.nativeView){
                const nativeFetch=window.fetch;
                async function projectNativeView(){
                    const response=await nativeFetch(mirror.url,{method:'POST',headers:{'content-type':'application/json','x-lsp-probe':mirror.token},body:JSON.stringify({project:mirror.project,action:'snapshot'})});
                    if(!response.ok)throw new Error('Test native filesystem correspondence failed');
                    const data=await response.json(),root=await(await navigator.storage.getDirectory()).getDirectoryHandle(mirror.root);
                    const directories=new Set(data.directories),files=new Set(Object.keys(data.files));
                    const valid=path=>path&&path.split('/').every(part=>part&&part!=='.'&&part!=='..'&&!part.includes('\\')&&!part.startsWith('.openwebide')&&part!=='.git');
                    if([...directories,...files].some(path=>!valid(path)))throw new Error('Invalid test counterpart path');
                    async function prune(parent,prefix=''){
                        for await(const [name,entry] of parent.entries()){
                            if(name==='.git'||name.startsWith('.openwebide'))continue;
                            const path=prefix+name,keep=entry.kind==='directory'?directories.has(path):files.has(path);
                            if(!keep)await removeOriginal.call(parent,name,{recursive:true});
                            else if(entry.kind==='directory')await prune(entry,path+'/');
                        }
                    }
                    await prune(root);
                    async function parentFor(path){
                        const parts=path.split('/');let parent=root;
                        for(const part of parts.slice(0,-1))parent=await parent.getDirectoryHandle(part,{create:true});
                        return [parent,parts.at(-1)];
                    }
                    for(const path of [...directories].sort((a,b)=>a.split('/').length-b.split('/').length)){
                        const [parent,name]=await parentFor(path);await parent.getDirectoryHandle(name,{create:true});
                    }
                    for(const [path,content] of Object.entries(data.files)){
                        const [parent,name]=await parentFor(path),file=await getOriginal.call(parent,name,{create:true});
                        const bytes=Uint8Array.from(atob(content),character=>character.charCodeAt(0));
                        const current=new Uint8Array(await(await file.getFile()).arrayBuffer());
                        if(current.length===bytes.length&&current.every((value,index)=>value===bytes[index]))continue;
                        const writer=await FileSystemFileHandle.prototype.createWritable.call(file);
                        await writer.write(bytes);await writer.close();
                    }
                }
                let projected=Promise.resolve();
                // Interrupted whole native requests can mutate before any
                // browser response. Tests explicitly refresh this counterpart
                // at a known native journal boundary before comparing views.
                window.refreshNativeProbeView=()=>{
                    projected=projected.then(projectNativeView);return projected;
                };
                window.fetch=async function(...args){
                    const response=await nativeFetch.apply(this,args);
                    const path=new URL(String(args[0]?.url??args[0]),location.href).pathname;
                    if(response.ok&&(path==='/workspace/files'||path==='/plugins/languages/command')){
                        await window.refreshNativeProbeView();
                    }
                    return response;
                };
            }
        '''
        if self.script_id is not None:
            browser.call('POST','/goog/cdp/execute',{'cmd':'Page.removeScriptToEvaluateOnNewDocument','params':{'identifier':self.script_id}})
        added=browser.call('POST','/goog/cdp/execute',{'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':source}})
        self.script_id=added['identifier']
    def stop(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=5)
