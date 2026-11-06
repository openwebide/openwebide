"""Browser primitives for real language-server editor checks in both modes."""
import json
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
    assert script(browser, '''
        const input = document.querySelector('textarea[data-editor-path]');
        const start = input?.value.indexOf(arguments[0]);
        if (start == null || start < 0) return false;
        input.focus(); input.setSelectionRange(start + arguments[1], arguments[2] ? start + arguments[0].length : start + arguments[1]);
        input.dispatchEvent(new Event('select', {bubbles:true})); return true;
    ''', token, delta, select), 'Missing cursor token ' + token


def insert(browser, text):
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
    """Mirror discovery markers between the test OPFS and native project roots.

    OPFS cannot address native files. Only this test correspondence is supplied;
    host selection, grants, LSP requests and source results stay unchanged.
    """
    def __init__(self, origin):
        self.origin, self.token, self.folders = origin, secrets.token_urlsafe(32), {}
        mirror = self
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_OPTIONS(self):
                self.reply(204)
            def reply(self, status):
                self.send_response(status)
                self.send_header('Access-Control-Allow-Origin', mirror.origin)
                self.send_header('Access-Control-Allow-Methods', 'POST, DELETE, OPTIONS')
                self.send_header('Access-Control-Allow-Headers', 'content-type, x-lsp-probe')
                self.send_header('Access-Control-Allow-Private-Network', 'true')
                self.end_headers()
            def mutate(self):
                if self.headers.get('Origin') != mirror.origin or self.headers.get('x-lsp-probe') != mirror.token:
                    self.reply(403); return
                body=json.loads(self.rfile.read(min(int(self.headers.get('Content-Length','0')),4096)))
                folder=mirror.folders.get(body.get('project'))
                name=body.get('name','')
                if folder is None or not re.fullmatch(r'\.openwebide-probe-[A-Za-z0-9_-]{1,128}',name):
                    self.reply(400); return
                if self.command=='POST': (folder/name).touch(exist_ok=False)
                else: (folder/name).unlink(missing_ok=True)
                self.reply(204)
            do_POST=mutate
            do_DELETE=mutate
        self.server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
        self.thread=Thread(target=self.server.serve_forever,daemon=True)
        self.thread.start()
    def install(self,browser,project,folder):
        self.folders[project]=folder
        config={'url':f'http://127.0.0.1:{self.server.server_port}', 'token':self.token,'project':project,'root':folder.name}
        source='const mirror='+json.dumps(config)+r''';
            async function marker(name,method) {
                const response=await fetch(mirror.url,{method,headers:{'content-type':'application/json','x-lsp-probe':mirror.token},body:JSON.stringify({project:mirror.project,name})});
                if(!response.ok) throw new Error('Test folder marker mirror failed');
            }
            const getOriginal=FileSystemDirectoryHandle.prototype.getFileHandle;
            FileSystemDirectoryHandle.prototype.getFileHandle=async function(name,options) {
                const file=await getOriginal.call(this,name,options);
                if(this.name!==mirror.root||!name.startsWith('.openwebide-probe-')) return file;
                const createOriginal=file.createWritable.bind(file);
                file.createWritable=async options=>{const stream=await createOriginal(options),close=stream.close.bind(stream);
                    stream.close=async()=>{await close();await marker(name,'POST');};return stream;};return file;
            };
            const removeOriginal=FileSystemDirectoryHandle.prototype.removeEntry;
            FileSystemDirectoryHandle.prototype.removeEntry=async function(name,options) {
                await removeOriginal.call(this,name,options);
                if(this.name===mirror.root&&name.startsWith('.openwebide-probe-')) await marker(name,'DELETE');
            };
        '''
        browser.call('POST','/goog/cdp/execute',{'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':source}})
    def stop(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(timeout=5)
