#!/usr/bin/env python3
"""Exercise the production Rust LSP editor through both execution-host adapters.

Build the production frontend, WASI backend and bundled-defaults release bridge
first. Local checks use real origin-private File System Access handles mirroring
an isolated native project; they do not claim physical folder permission checks.
All projects, accounts, native caches and browser data are temporary.
"""
import base64
import json
from pathlib import Path
import runpy
import secrets
import subprocess
import tempfile

from language_host_support import ExecutionHost
from language_server_client_support import wait, script, click, close, command, caret, insert, local_handle, ProbeMirror

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']



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
            installations = host.initialize_defaults()
            installation = next(entry for entry in installations if entry['prepared']['manifest']['name']=='rust')
            assert installation['prepared']['manifest']['version']=='0.3.0'
            assert installation['prepared']['manifest']['compatibility']['pluginApi']==9
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
                    const raw=args[1]?.body;
                    let payload=Promise.resolve('{}');
                    try{payload=typeof raw==='string'?Promise.resolve(raw):raw?.text?raw.text():raw?Promise.resolve(new TextDecoder().decode(raw)):args[0]?.clone?args[0].clone().text():payload;}catch{}
                    payload=payload.catch(()=>'{}');
                    return fetchOriginal.apply(this,args).then(async response=> {
                        let body={};try{body=JSON.parse(await payload);}catch{}
                        const input=body.input??body.call?.input??body.selection?.input;
                        const marker=typeof body.name==='string'&&body.name.startsWith('.openwebide-probe-');
                        if(marker||/\/plugins\/languages\/(query|authorize|revoke)$/.test(new URL(url,location.href).pathname)) {
                            const record={time:Date.now(),url,status:response.status,query:input?.query,document:input?.document,
                                client:input?.client,request:input?.request,revision:input?.revision,source_digest:input?.source_digest};
                            if(marker)record.marker={name:body.name,method:args[1]?.method};
                            try{const data=await response.clone().json();record.server=data.server;record.ready=data.diagnostics_ready;record.diagnostics=data.diagnostics?.length;record.error=data.error;
                                if(Array.isArray(data.result))record.completions=data.result.filter(item=>item.label?.startsWith('answer')).map(item=>({id:item.id,label:item.label}));}catch{}
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
                for path,text in files.items():
                    (folder/path).parent.mkdir(parents=True,exist_ok=True)
                    (folder/path).write_bytes(text.encode())
                subprocess.run(['cargo','generate-lockfile','--offline','--manifest-path',str(folder/'Cargo.toml')],cwd=ROOT,check=True)
                files['Cargo.lock']=(folder/'Cargo.lock').read_text()
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
                for path in ('Cargo.toml','Cargo.lock','src/main.rs','src/lib.rs'): assert (folder/path).read_bytes()==files[path].encode(), 'Language workflow changed '+path+' on disk'
                assert not (folder/'target').exists(), 'The installed Rust package wrote Cargo output inside the project'
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
