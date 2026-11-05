#!/usr/bin/env python3
"""Exercise the installed JS/TS component and real server in both production editors.

Build the production frontend, WASI backend and bundled-defaults release bridge
first. Local files use origin-private handles with the shared test marker mirror;
this verifies the paired native transport, not physical folder permissions.
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
DIALECTS = {'main.ts': 'typescript', 'plain.js': 'javascript', 'view.tsx': 'tsx', 'widget.jsx': 'jsx'}


def select(browser, path):
    close(browser)
    assert script(browser, '''
        const button=[...document.querySelectorAll('[data-editor-tab]')]
            .find(button=>button.dataset.editorTab===arguments[0]);
        if(!button) return false; button.click(); return true;
    ''', path), 'Missing tab ' + path
    wait(path+' editor', lambda: script(browser, "return document.querySelector('textarea[data-editor-path]')?.dataset.editorPath===arguments[0];", path))


def hover(browser, path, expected, token='answer'):
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
    with tempfile.TemporaryDirectory(prefix='openwebide-typescript-client-') as state_path, tempfile.TemporaryDirectory(prefix='typescript-native-', dir=ROOT.parent) as project_path:
        state, project_root=Path(state_path), Path(project_path)
        runtime=Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host=ExecutionHost(runtime)
        browser=None
        mirror=ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST','/api/auth/register',{'username':'typescript-'+secrets.token_hex(6),'password':secrets.token_urlsafe(32)},expected=201)
            runtime.acknowledge_release()
            user=runtime.request('GET','/api/auth/me')['user']['id']
            installations=host.initialize_defaults()
            installation=next(entry for entry in installations if entry['prepared']['manifest']['name']=='javascript-typescript')
            assert installation['prepared']['manifest']['version']=='0.2.0'
            assert installation['prepared']['manifest']['compatibility']['pluginApi']==7
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
            for mode in ('remote','local'):
                folder=project_root/mode
                folder.mkdir()
                files={
                    'tsconfig.json':json.dumps({'compilerOptions':{'target':'ES2022','module':'ESNext','moduleResolution':'bundler','jsx':'preserve','allowJs':True,'checkJs':True,'strict':True,'noEmit':True,'types':[]},'include':['*.ts','*.tsx','*.js','*.jsx']}),
                    'package.json':'{"type":"module"}',
                    'types.d.ts':'declare namespace JSX { interface IntrinsicElements { div: any; } }\n',
                    'extra.ts':'export function extraAnswer(): number { return 42; }\nexport function extraAction(): number { return 7; }\n',
                    'lib.ts':'export function answer(): number { return 42; }\r\n',
                    'main.ts':'// 文😀\r\nimport { answer } from "./lib";\r\nexport const value=answer();\r\n',
                    'plain.js':'import { answer } from "./lib";\r\nexport const value=answer();\r\n',
                    'view.tsx':'import { answer } from "./lib";\r\nexport const view=<div>{answer()}</div>;\r\n',
                    'widget.jsx':'import { answer } from "./lib";\r\nexport const widget=<div>{answer()}</div>;\r\n',
                }
                for path,text in files.items(): (folder/path).write_bytes(text.encode())
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative=folder.relative_to(ROOT.parent.parent).as_posix()
                project=runtime.request('POST','/api/projects',{'name':'TypeScript '+mode,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,files)
                    mirror.install(browser,project['id'],folder)
                drafts=[]
                for path in ['lib.ts',*DIALECTS]:
                    encoded=base64.b64encode(files[path].encode()).decode()
                    drafts.append({'path':path,'document':{'text':encoded,'saved':encoded,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':'main.ts','files':drafts}})
                for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                    runtime.request('PUT','/api/settings',{'key':key,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('answer');"))
                wait(mode+' server',lambda:script(browser,'return lspEvidence.requests.some(request=>request.server);'),90)
                for path in DIALECTS: hover(browser,path,'number')
                # Change a real buffer without saving. Every related dialect must
                # receive its own document identity while sharing the native server.
                select(browser,'lib.ts')
                caret(browser,': number { return 42; }',select=True)
                insert(browser,': string { return "unsaved"; }')
                wait('unsaved library edit',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('unsaved');"))
                for path in DIALECTS: hover(browser,path,'string')
                servers=script(browser,'return [...new Set(lspEvidence.requests.map(request=>request.server).filter(Boolean))];')
                assert len(servers)==1, 'Dialect switches replaced the warm server: '+repr(servers)
                warm=servers[0]
                select(browser,'main.ts')
                caret(browser,'answer();',len('answer();'))
                insert(browser,'\nextraAn')
                caret(browser,'extraAn',len('extraAn'))
                command(browser,'Complete at cursor')
                wait('auto-import completion',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.trim().startsWith('extraAnswer'));if(!button)return false;button.click();return true;"))
                wait('resolved auto-import completion',lambda:click(browser,'Insert completion'))
                wait('completion text and import insertion',lambda:script(browser,"const text=document.querySelector('textarea[data-editor-path]')?.value;return text?.includes('import { extraAnswer }')&&text.includes('\\nextraAnswer');"))
                command(browser,'Format document')
                wait('formatting review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                assert click(browser,'Apply formatting')
                wait('formatting applied',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('value = answer()');"))
                caret(browser,'value = answer();',len('value = answer();'))
                broken='\nconst broken: number = "bad";'
                insert(browser,broken)
                wait('current-source TypeScript diagnostics',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                Path('/tmp/openwebide-typescript-lsp-diagnostics-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                caret(browser,broken,select=True)
                previous=script(browser,'return lspEvidence.requests.length;')
                insert(browser,' ')
                wait('fixed-source diagnostics clear',lambda:script(browser,'return lspEvidence.requests.slice(arguments[0]).some(request=>request.ready&&request.diagnostics===0)&&!document.querySelector(".editor-diagnostic");',previous))
                caret(browser,'value = answer();',len('value = answer();'))
                insert(browser,'\nextraAction();')
                wait('missing-import diagnostic',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                caret(browser,'extraAction',select=True)
                command(browser,'Code actions')
                wait('import code action',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>/^(?:Add|Update) import from /.test(button.textContent.trim())&&!button.disabled);if(!button)return false;button.click();return true;"))
                wait('import action review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                assert click(browser,'Apply to editor tabs')
                wait('code action import staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.split('\\n').some(line=>line.includes('import')&&line.includes('extraAction'));"))
                caret(browser,'answer'); command(browser,'Go to definition')
                wait('definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('lib.ts:'));if(!button)return false;button.click();return true;"))
                wait('unsaved definition navigation',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath==='lib.ts'&&input.value.includes('unsaved');"))
                caret(browser,'answer'); command(browser,'Document symbols')
                wait('document symbols',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(button=>button.textContent.trim()==='answer');"))
                close(browser); caret(browser,'answer'); command(browser,'Find references')
                wait('all-dialect references',lambda:script(browser,"const text=document.querySelector('[role=dialog]')?.textContent;return arguments[0].every(path=>text?.includes(path+':'));",list(DIALECTS)))
                close(browser); caret(browser,'answer'); command(browser,'Rename symbol…')
                wait('rename input',lambda:script(browser,"const input=document.querySelector('[role=dialog] input');if(!input)return false;input.value='renamedAnswer';input.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('five-file rename review',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return arguments[0].every(path=>dialog?.textContent.includes(path))&&[...dialog.querySelectorAll('button')].some(button=>button.textContent.trim()==='Apply to editor tabs'&&!button.disabled);",['lib.ts',*DIALECTS]))
                assert script(browser, '''
                    const diffs=[...document.querySelectorAll('[role=dialog] .editor-diff')];
                    return diffs.length===5&&diffs.every(diff=>diff.clientHeight>=diff.scrollHeight-1);
                '''), 'Rename previews collapsed instead of allowing dialog scrolling'
                Path('/tmp/openwebide-typescript-lsp-review-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                for path in ['lib.ts',*DIALECTS]:
                    select(browser,path)
                    wait(path+' staged rename',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamedAnswer');"))
                wait('rename recovery persisted',lambda:all('renamedAnswer' in recover(runtime,project).get(path,'') for path in ['lib.ts',*DIALECTS]))
                for path,text in files.items(): assert (folder/path).read_bytes()==text.encode(), 'Review wrote behind Save: '+path
                if mode=='local':
                    assert browser.call('POST','/execute/async',{'script': '''
                        const [folder,files,done]=arguments;
                        (async()=>{const root=await (await navigator.storage.getDirectory()).getDirectoryHandle(folder);
                            for(const [path,text] of Object.entries(files))if(await (await root.getFileHandle(path)).getFile().then(file=>file.text())!==text)return false;
                            return true;})().then(done,error=>done(String(error)));
                    ''','args':[folder.name,files]}) is True, 'Local review wrote behind Save'
                select(browser,'main.ts')
                assert script(browser,"const button=[...document.querySelectorAll('button')].find(button=>button.getAttribute('aria-label')==='Close widget.jsx');if(!button)return false;button.click();return true;")
                hover(browser,'main.ts','string','renamedAnswer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')==warm
                evidence=script(browser,'return lspEvidence;')
                assert not evidence['errors'], evidence['errors']
                expected=(host.url if mode=='local' else runtime.url+'/api')+'/plugins/languages/query'
                assert any(request['url']==expected and request['server'] for request in evidence['requests']), evidence
                # Disable the actual installed component, then re-enable it using
                # the same ordinary reviewed binding, retaining unsaved recovery.
                endpoint=f"/api/projects/{project['id']}/plugins"
                binding=next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']=='javascript-typescript')
                package=runtime.request('POST','/api/plugins/package',binding['prepared'])
                changed=runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                disabled=next(entry for entry in changed if entry['id']==binding['id'])
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('disabled package plain editing',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamedAnswer')&&!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':installation['revision'],'revision':disabled['revision']})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('re-enabled editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'main.ts','string','renamedAnswer')
                host.stop_process(); host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'main.ts','string','renamedAnswer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')!=warm
                print(json.dumps({'mode':mode,'real_typescript_lsp':True,'installed_api_seven_component':True,'four_dialects_one_server':True,'unsaved_cross_dialect_hover':True,'completion_with_auto_import':True,'formatting_review_and_apply':True,'diagnostics_and_clear':True,'import_code_action_review_and_staging':True,'definition_navigation':True,'symbols_and_all_dialect_references':True,'five_file_rename_review_and_staging':True,'readable_multi_file_previews':True,'source_unchanged_on_disk':True,'close_dialect_preserves_server':True,'disable_reenable_preserves_buffers':True,'host_restart_resynchronizes_unsaved_buffers':True}),flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser,"return {editor:document.querySelector('textarea[data-editor-path]')?.value,evidence:window.lspEvidence&&{...lspEvidence,requestCount:lspEvidence.requests.length,requests:lspEvidence.requests.slice(-12)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(dialog=>dialog.textContent.slice(0,3000)),alerts:[...document.querySelectorAll('[role=alert]')].map(alert=>alert.textContent.slice(0,2000))};")),flush=True)
                except Exception:
                    pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists(): Path('/tmp/openwebide-typescript-lsp-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None: browser.stop()
            runtime.stop(); host.stop(); mirror.stop()


if __name__=='__main__':
    check()
