#!/usr/bin/env python3
"""Exercise the production Rust LSP editor through both execution-host adapters.

Build the production frontend, WASI backend and bundled-defaults release bridge
first. Local checks use real origin-private File System Access handles mirroring
an isolated native project; they do not claim physical folder permission checks.
All projects, accounts, native caches and browser data are temporary.
"""
import base64
import json
import re
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from pathlib import Path
import runpy
import secrets
import subprocess
import tempfile
import time

from language_host_support import ExecutionHost

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']


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
    assert script(browser, '''
        const button = document.querySelector('button[aria-label="Code intelligence"]');
        if (!button) return false; button.click(); return true;
    '''), 'Missing Code intelligence control'
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
    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': text}})


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


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-lsp-client-') as state_path, tempfile.TemporaryDirectory(prefix='lsp-native-', dir=ROOT.parent) as project_path:
        state, project_root = Path(state_path), Path(project_path)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        host = ExecutionHost(runtime)
        browser = None
        mirror = ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST', '/api/auth/register', {'username':'lsp-'+secrets.token_hex(6),'password':secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            host.initialize_defaults()
            browser = Browser(state)
            browser.call('POST', '/url', {'url':runtime.url+'/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie':{'name':cookie.name,'value':cookie.value,'domain':'127.0.0.1','path':cookie.path,'httpOnly':True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':r'''
                window.lspEvidence={requests:[],errors:[]};
                addEventListener('error',e=>lspEvidence.errors.push(String(e.message)));
                addEventListener('unhandledrejection',e=>lspEvidence.errors.push(String(e.reason)));
                const fetchOriginal=window.fetch;
                window.fetch=function(...args){const url=String(args[0]?.url??args[0]);
                    return fetchOriginal.apply(this,args).then(async response=> {
                        if(/\/plugins\/languages\/(query|authorize|revoke)$/.test(new URL(url,location.href).pathname)) {
                            const record={url,status:response.status};
                            try{const data=await response.clone().json();record.server=data.server;record.ready=data.diagnostics_ready;record.diagnostics=data.diagnostics?.length;record.error=data.error;}catch{}
                            lspEvidence.requests.push(record);
                        } return response;
                    });};
            '''}})
            for mode in ('remote', 'local'):
                folder=project_root/mode
                (folder/'src').mkdir(parents=True)
                crate='rust_lsp_'+mode
                files={'Cargo.toml':f'[package]\nname="{crate}"\nversion="0.1.0"\nedition="2021"\n',
                       'src/lib.rs':'pub fn answer() -> u32 { 42 }\r\n',
                       'src/main.rs':f'// 文😀\r\nfn main() {{ let value = {crate}::answer(); println!("{{value}}"); }}\r\n'}
                for path,text in files.items(): (folder/path).write_bytes(text.encode())
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative=folder.relative_to(ROOT.parent.parent).as_posix()
                project=runtime.request('POST','/api/projects',{'name':'LSP '+mode,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,files)
                    mirror.install(browser,project['id'],folder)
                encoded=base64.b64encode(files['src/main.rs'].encode()).decode()
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{
                    'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':'src/main.rs',
                    'files':[{'path':'src/main.rs','document':{'text':encoded,'saved':encoded,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False}]}})
                for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                    runtime.request('PUT','/api/settings',{'key':key,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('answer');"))
                wait(mode+' service',lambda:script(browser,"return lspEvidence.requests.some(request=>request.server);"),90)
                def hover():
                    caret(browser,'answer'); command(browser,'Symbol information')
                    wait('hover response',lambda:script(browser,"return document.querySelector('[role=dialog]')?.textContent.includes('Symbol information');"))
                    return script(browser,"return document.querySelector('[role=dialog]')?.textContent.includes('u32');")
                wait(mode+' Rust hover',hover)
                close(browser)
                # Exercise completion, formatting and diagnostics through the editor,
                # retaining all changes in buffers until the later rename review.
                caret(browser,'answer',select=True)
                insert(browser,'ans')
                wait('completion prefix edit',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('::ans()');"))
                caret(browser,'ans',3)
                command(browser,'Complete at cursor')
                wait('completion choice',lambda:script(browser,"const b=[...document.querySelectorAll('[role=dialog] button')].find(b=>b.textContent.trim().startsWith('answer'));if(!b)return false;b.click();return true;"))
                wait('resolved completion',lambda:click(browser,'Insert completion'))
                wait('completion inserted',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('::answer()');"))
                command(browser,'Format document')
                wait('formatting review',lambda:script(browser,"return document.querySelector('[role=dialog] .diff-line.add')!==null;"))
                assert click(browser,'Apply formatting')
                wait('formatting applied',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('\\n    let value');"))
                caret(browser,'{',1)
                insert(browser,'\n    let broken: = ;')
                wait('current-source diagnostic marks',lambda:script(browser,"const mark=document.querySelector('.editor-diagnostic'),editor=document.querySelector('.editor-code');if(!mark||!editor)return false;const r=mark.getBoundingClientRect(),e=editor.getBoundingClientRect();return r.width>0&&r.height>=2&&r.left>=e.left&&r.top>=e.top&&r.bottom<=e.bottom;"))
                Path('/tmp/openwebide-rust-lsp-diagnostics-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                caret(browser,'\n    let broken: = ;',select=True)
                previous=script(browser,'return lspEvidence.requests.length;')
                insert(browser,' ')
                wait('corrected-source diagnostics',lambda:script(browser,"return lspEvidence.requests.slice(arguments[0]).some(r=>r.ready&&r.diagnostics===0)&&!document.querySelector('.editor-diagnostic');",previous))
                close(browser); caret(browser,'answer'); command(browser,'Go to definition')
                wait('definition target',lambda:script(browser,"const b=[...document.querySelectorAll('[role=dialog] button')].find(b=>b.textContent.includes('src/lib.rs:'));if(!b)return false;b.click();return true;"))
                wait('cross-file navigation',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.getAttribute('data-editor-path')==='src/lib.rs'&&input.value.startsWith('pub fn answer');"))
                caret(browser,'answer'); command(browser,'Document symbols')
                wait('document symbol',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(b=>b.textContent.trim()==='answer');"))
                close(browser); caret(browser,'answer'); command(browser,'Find references')
                wait('cross-file references',lambda:script(browser,"return document.querySelector('[role=dialog]')?.textContent.includes('src/main.rs:');"))
                close(browser); caret(browser,'answer'); command(browser,'Rename symbol…')
                wait('rename input',lambda:script(browser,"const i=document.querySelector('[role=dialog] input');if(!i)return false;i.value='renamed_answer';i.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('multi-file preview',lambda:script(browser,"const d=document.querySelector('[role=dialog]');return d?.textContent.includes('src/main.rs')&&d?.textContent.includes('src/lib.rs')&&[...d.querySelectorAll('button')].some(b=>b.textContent.trim()==='Apply to editor tabs'&&!b.disabled);"))
                Path('/tmp/openwebide-rust-lsp-review-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                wait('rename applied',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamed_answer');"))
                for path in ('src/main.rs','src/lib.rs'): assert (folder/path).read_bytes()==files[path].encode(), 'Review wrote behind Save'
                evidence=script(browser,'return lspEvidence;')
                assert not evidence['errors'], evidence['errors']
                expected=(host.url if mode=='local' else runtime.url+'/api')+'/plugins/languages/query'
                assert any(request['url']==expected and request['server'] for request in evidence['requests']), evidence
                print(json.dumps({'mode':mode,'real_rust_lsp':True,'completion_insertion':True,'formatting_review_and_apply':True,'current_diagnostic_marks_and_clear':True,'hover':True,'definition_navigation':True,'symbols':True,'references':True,'rename_review_and_staging':True,'source_unchanged_on_disk':True}),flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "return {editor:document.querySelector('textarea[data-editor-path]')?.value,evidence:window.lspEvidence,dialogs:[...document.querySelectorAll('[role=dialog]')].map(d=>d.textContent.slice(0,3000)),alerts:[...document.querySelectorAll('[role=alert]')].map(d=>d.textContent.slice(0,2000))};")),flush=True)
                except Exception:
                    pass
            for name, log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists(): Path('/tmp/openwebide-rust-lsp-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None: browser.stop()
            runtime.stop(); host.stop(); mirror.stop()


if __name__=='__main__':
    check()
