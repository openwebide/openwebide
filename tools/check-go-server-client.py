#!/usr/bin/env python3
"""Exercise the installed Go component and real server in both production editors.

Build the production frontend, WASI backend and bundled-defaults release bridge
first. Local files use origin-private handles with the shared test marker mirror;
this verifies the paired native transport, not physical folder permissions.
"""
import base64
import json
import os
from itertools import product
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
CLIENTS = ['app/main.go']


def select(browser, path):
    close(browser)
    assert script(browser, '''
        const button=[...document.querySelectorAll('[data-editor-tab]')]
            .find(button=>button.dataset.editorTab===arguments[0]);
        if(!button) return false; button.click(); return true;
    ''', path), 'Missing tab ' + path
    # Restored text can be visible before the file-read lock is released.
    # Do not send the single native input event until this buffer is writable.
    wait(path+' writable native editor', lambda: script(browser, "const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath===arguments[0]&&input.dataset.editorNativeValueScope===input.dataset.editorScope&&input.dataset.editorNativePreparing==='false'&&!input.readOnly;", path))


def hover(browser, path, expected, token='Answer'):
    select(browser, path)
    def ready():
        caret(browser, token)
        command(browser, 'Symbol information')
        wait('hover response', lambda: script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes('Symbol information');"))
        return script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes(arguments[0]);", expected)
    wait(path+' '+expected+' hover', ready)
    close(browser)


def recover(runtime, project):
    record=runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
    return {file['path']: base64.b64decode(file['document']['text']).decode().replace('\r\n','\n') for file in record['state']['files']}


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-go-client-') as state_path, tempfile.TemporaryDirectory(prefix='go-native-', dir=ROOT.parent) as project_path:
        state, project_root=Path(state_path), Path(project_path)
        os.environ.update({'GOENV':'off','GOTOOLCHAIN':'local','GOWORK':'','GOFLAGS':'','GOPROXY':'off','GOSUMDB':'off',
                           'GOCACHE':str(state/'go-cache'),'GOMODCACHE':str(state/'go-modules')})
        runtime=Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host=ExecutionHost(runtime)
        browser=None
        mirror=ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST','/api/auth/register',{'username':'go-'+secrets.token_hex(6),'password':secrets.token_urlsafe(32)},expected=201)
            runtime.acknowledge_release()
            user=runtime.request('GET','/api/auth/me')['user']['id']
            installations=host.initialize_defaults()
            installation=next(entry for entry in installations if entry['prepared']['manifest']['name']=='go')
            assert installation['prepared']['manifest']['version']=='0.3.0'
            assert installation['prepared']['manifest']['compatibility']['pluginApi']==9
            browser=Browser(state)
            browser.call('POST','/url',{'url':runtime.url+'/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST','/cookie',{'cookie':{'name':cookie.name,'value':cookie.value,'domain':'127.0.0.1','path':cookie.path,'httpOnly':True}})
            host.pair_browser(browser)
            browser.call('POST','/goog/cdp/execute',{'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':r'''
                window.lspEvidence={requests:[],errors:[]};
                addEventListener('error',event=>lspEvidence.errors.push(String(event.message)));
                addEventListener('unhandledrejection',event=>lspEvidence.errors.push(String(event.reason)));
                const fetchOriginal=window.fetch;
                window.fetch=function(...args){const url=String(args[0]?.url??args[0]);
                    return fetchOriginal.apply(this,args).then(async response=>{
                        if(/\/plugins\/languages\/(query|authorize|revoke)$/.test(new URL(url,location.href).pathname)){
                            const record={url,status:response.status};
                            try{const data=await response.clone().json();record.server=data.server;record.ready=data.diagnostics_ready;record.diagnostics=data.diagnostics?.length;record.error=data.error;}catch{}
                            lspEvidence.requests.push(record);
                        }return response;
                    });};
            '''}})
            for mode,layout in product(('remote','local'),('module','workspace')):
                folder=project_root/(mode+'-'+layout)
                folder.mkdir()
                module='example.test/lib' if layout=='workspace' else 'example.test/project/lib'
                files={
                    'lib/answer.go':'package lib\r\n\r\nfunc Answer() int { return 42 }\r\n',
                    'app/main.go':f'package app\r\n\r\nimport "{module}"\r\n\r\nvar Note = "文😀"; var Value = lib.Answer()\r\n',
                    'app/scratch.go':'package app\r\n\r\nvar Scratch = 1\r\n',
                    'app/format.go':'package app\r\n\r\nvar Formatted=1\r\n',
                    'app/imports.go':'package app\r\n\r\nfunc Render() {\r\n}\r\n',
                }
                if layout=='workspace':
                    files.update({'go.work':'go 1.27.0\n\nuse (\n ./app\n ./lib\n)\n',
                                  'lib/go.mod':'module example.test/lib\n\ngo 1.27.0\n',
                                  'app/go.mod':'module example.test/app\n\ngo 1.27.0\n\nrequire example.test/lib v0.0.0\n'})
                else: files['go.mod']='module example.test/project\n\ngo 1.27.0\n'
                for path,value in files.items():
                    (folder/path).parent.mkdir(parents=True,exist_ok=True)
                    (folder/path).write_bytes(value.encode())
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative=folder.relative_to(ROOT.parent.parent).as_posix()
                project=runtime.request('POST','/api/projects',{'name':'Go '+mode+' '+layout,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,files)
                    mirror.install(browser,project['id'],folder)
                drafts=[]
                for path in ['lib/answer.go','app/scratch.go','app/format.go','app/imports.go',*CLIENTS]:
                    encoded=base64.b64encode(files[path].encode()).decode()
                    drafts.append({'path':path,'document':{'text':encoded,'saved':encoded,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':'app/main.go','files':drafts}})
                for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                    runtime.request('PUT','/api/settings',{'key':key,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('Answer');"))
                wait(mode+' server',lambda:script(browser,'return lspEvidence.requests.some(request=>request.server);'),90)
                for path in CLIENTS: hover(browser,path,'int')
                # Change a real buffer without saving. Every open document must
                # receive its own document identity while sharing the native server.
                select(browser,'lib/answer.go')
                caret(browser,'int { return 42 }',select=True)
                insert(browser,'string { return "unsaved" }')
                wait('unsaved library edit',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('unsaved');"))
                for path in CLIENTS: hover(browser,path,'string')
                servers=script(browser,'return [...new Set(lspEvidence.requests.map(request=>request.server).filter(Boolean))];')
                assert len(servers)==1, 'Tab switches replaced the warm server: '+repr(servers)
                warm=servers[0]
                select(browser,'app/imports.go')
                caret(browser,'func Render() {',len('func Render() {'))
                insert(browser,'\n    fmt.Spri')
                caret(browser,'fmt.Spri',len('fmt.Spri'))
                command(browser,'Complete at cursor')
                wait('unimported fmt.Sprint completion',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>/^Sprint(?:\\(|$)/.test(button.textContent.trim()));if(!button)return false;button.click();return true;"))
                wait('resolved completion',lambda:click(browser,'Insert completion'))
                wait('completion insertion and import',lambda:script(browser,"""const value=document.querySelector('textarea[data-editor-path]')?.value;return value?.includes('fmt.Sprint')&&value.includes('"fmt"');"""))
                caret(browser,'fmt.Sprint',len('fmt.'))
                command(browser,'Go to definition')
                wait('standard-library definition',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('print.go:'));if(!button)return false;button.click();return true;"))
                wait('read-only Go standard library',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return dialog?.textContent.includes('External source · read-only')&&dialog.querySelector('pre')?.textContent.includes('func Sprint(');"))
                assert script(browser,"return !document.querySelector('[role=dialog] textarea:not([readonly])');")
                Path('/tmp/openwebide-go-lsp-stdlib-'+mode+'-'+layout+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                close(browser)
                select(browser,'app/format.go')
                command(browser,'Format document')
                wait('formatting review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                assert click(browser,'Apply formatting')
                wait('formatting staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('var Formatted = 1');"))
                select(browser,'app/main.go')
                caret(browser,'var Value = lib.Answer()',len('var Value = lib.Answer()'))
                broken='\nvar Broken int = "bad"'
                insert(browser,broken)
                wait('current-source Go diagnostics',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                Path('/tmp/openwebide-go-lsp-diagnostics-'+mode+'-'+layout+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                caret(browser,broken,select=True)
                previous=script(browser,'return lspEvidence.requests.length;')
                insert(browser,'\nvar Broken int = 1')
                wait('fixed-source diagnostics clear',lambda:script(browser,'return lspEvidence.requests.slice(arguments[0]).some(request=>request.ready&&request.diagnostics===0)&&!document.querySelector(".editor-diagnostic");',previous))
                caret(browser,'Answer'); command(browser,'Go to definition')
                wait('definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('lib/answer.go:'));if(!button)return false;button.click();return true;"))
                wait('unsaved definition navigation',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath==='lib/answer.go'&&input.value.includes('unsaved');"))
                caret(browser,'Answer'); command(browser,'Document symbols')
                wait('document symbols',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(button=>button.textContent.trim()==='Answer');"))
                close(browser); caret(browser,'Answer'); command(browser,'Find references')
                wait('cross-file references',lambda:script(browser,"const text=document.querySelector('[role=dialog]')?.textContent;return arguments[0].every(path=>text?.includes(path+':'));",['lib/answer.go',*CLIENTS]))
                close(browser); caret(browser,'Answer'); command(browser,'Rename symbol…')
                wait('rename input',lambda:script(browser,"const input=document.querySelector('[role=dialog] input');if(!input)return false;input.value='RenamedAnswer';input.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('two-file rename review',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return arguments[0].every(path=>dialog?.textContent.includes(path))&&[...dialog.querySelectorAll('button')].some(button=>button.textContent.trim()==='Apply to editor tabs'&&!button.disabled);",['lib/answer.go',*CLIENTS]))
                assert script(browser, '''
                    const diffs=[...document.querySelectorAll('[role=dialog] .editor-diff')];
                    return diffs.length===2&&diffs.every(diff=>diff.clientHeight>=diff.scrollHeight-1);
                '''), 'Rename previews collapsed instead of allowing dialog scrolling'
                Path('/tmp/openwebide-go-lsp-review-'+mode+'-'+layout+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                wait('rename published before closing its review',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('RenamedAnswer');"))
                for path in ['lib/answer.go',*CLIENTS]:
                    select(browser,path)
                    wait(path+' staged rename',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('RenamedAnswer');"))
                wait('rename recovery persisted',lambda:all('RenamedAnswer' in recover(runtime,project).get(path,'') for path in ['lib/answer.go',*CLIENTS]))
                for path,text in files.items(): assert (folder/path).read_bytes()==text.encode(), 'Review wrote behind Save: '+path
                if mode=='local':
                    assert browser.call('POST','/execute/async',{'script': '''
                        const [folder,files,done]=arguments;
                        (async()=>{const root=await (await navigator.storage.getDirectory()).getDirectoryHandle(folder);
                            for(const [path,text] of Object.entries(files)){let parent=root;const parts=path.split('/');const leaf=parts.pop();for(const part of parts)parent=await parent.getDirectoryHandle(part);if(await (await parent.getFileHandle(leaf)).getFile().then(file=>file.text())!==text)return false;}
                            return true;})().then(done,error=>done(String(error)));
                    ''','args':[folder.name,files]}) is True, 'Local review wrote behind Save'
                select(browser,'app/main.go')
                assert script(browser,"const button=[...document.querySelectorAll('button')].find(button=>button.getAttribute('aria-label')==='Close app/scratch.go');if(!button)return false;button.click();return true;")
                hover(browser,'app/main.go','string','RenamedAnswer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')==warm
                evidence=script(browser,'return lspEvidence;')
                assert not evidence['errors'], evidence['errors']
                expected=(host.url if mode=='local' else runtime.url+'/api')+'/plugins/languages/query'
                assert any(request['url']==expected and request['server'] for request in evidence['requests']), evidence
                # Disable the actual installed component, then re-enable it using
                # the same ordinary reviewed binding, retaining unsaved recovery.
                endpoint=f"/api/projects/{project['id']}/plugins"
                binding=next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']=='go')
                package=runtime.request('POST','/api/plugins/package',binding['prepared'])
                changed=runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                disabled=next(entry for entry in changed if entry['id']==binding['id'])
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('disabled package plain editing',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('RenamedAnswer')&&!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':installation['revision'],'revision':disabled['revision']})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('re-enabled editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'app/main.go','string','RenamedAnswer')
                reenabled=script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')
                host.stop_process(); host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'app/main.go','string','RenamedAnswer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')!=reenabled
                print(json.dumps({'mode':mode,'layout':layout,'installed_plugin_api':installation['prepared']['manifest']['compatibility']['pluginApi'],'real_go_lsp':True,'module_workspace_resolution':True,'unsaved_cross_file_hover':True,'completion_import_and_insertion':True,'formatting_review_and_staging':True,'diagnostics_and_clear':True,'definition_navigation':True,'standard_library_read_only':True,'symbols_and_references':True,'two_file_rename_review_and_staging':True,'source_unchanged_on_disk':True,'recovery_persisted':True,'close_preserves_warm_service':True,'disable_reenable_preserves_buffers':True,'host_restart_resynchronizes_unsaved_buffers':True}),flush=True)

        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser,"const input=document.querySelector('textarea[data-editor-path]');return {editor:input?.value,native:input&&{readOnly:input.readOnly,...input.dataset},evidence:window.lspEvidence&&{...lspEvidence,requestCount:lspEvidence.requests.length,requests:lspEvidence.requests.slice(-12)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(dialog=>dialog.textContent.slice(0,3000)),alerts:[...document.querySelectorAll('[role=alert]')].map(alert=>alert.textContent.slice(0,2000))};")),flush=True)
                except Exception:
                    pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists(): Path('/tmp/openwebide-go-lsp-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None: browser.stop()
            runtime.stop(); host.stop(); mirror.stop()


if __name__=='__main__':
    check()
