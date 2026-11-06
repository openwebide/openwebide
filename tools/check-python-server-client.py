#!/usr/bin/env python3
"""Exercise the installed Python component and real server in both production editors.

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
CLIENTS = ['main.py']


def select(browser, path):
    close(browser)
    assert script(browser, '''
        const button=[...document.querySelectorAll('[data-editor-tab]')]
            .find(button=>button.dataset.editorTab===arguments[0]);
        if(!button) return false; button.click(); return true;
    ''', path), 'Missing tab ' + path
    wait(path+' native editor', lambda: script(browser, "const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath===arguments[0]&&input.dataset.editorNativeValueScope===input.dataset.editorScope&&input.dataset.editorNativePreparing==='false';", path))


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
    with tempfile.TemporaryDirectory(prefix='openwebide-python-client-') as state_path, tempfile.TemporaryDirectory(prefix='python-native-', dir=ROOT.parent) as project_path:
        state, project_root=Path(state_path), Path(project_path)
        runtime=Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host=ExecutionHost(runtime)
        browser=None
        mirror=ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST','/api/auth/register',{'username':'python-'+secrets.token_hex(6),'password':secrets.token_urlsafe(32)},expected=201)
            runtime.acknowledge_release()
            user=runtime.request('GET','/api/auth/me')['user']['id']
            installations=host.initialize_defaults()
            installation=next(entry for entry in installations if entry['prepared']['manifest']['name']=='python')
            assert installation['prepared']['manifest']['version']=='0.2.0'
            assert installation['prepared']['manifest']['compatibility']['pluginApi']==6
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
                    'pyproject.toml':"[tool.pyright]\nvenvPath = '.'\nvenv = '.venv'\ntypeCheckingMode = 'standard'\n",
                    'lib.py':"def answer() -> int:\r\n    return 42\r\n",
                    'main.py':'# 文😀\r\nfrom lib import answer\r\nfrom env_only import environment_value\r\nvalue = answer()\r\nenvironment = environment_value()\r\n',
                    'scratch.py':'scratch = 1\r\n',
                    'stub.pyi':'def stub_answer() -> str: ...\r\n',
                }
                for path,text in files.items(): (folder/path).write_bytes(text.encode())
                subprocess.run(['python3','-m','venv','--without-pip',str(folder/'.venv')],check=True)
                site=subprocess.check_output([str(folder/'.venv/bin/python'),'-c',"import sysconfig; print(sysconfig.get_path('purelib'))"],text=True).strip()
                (Path(site)/'env_only.py').write_text("def environment_value() -> bytes:\n    return b'venv'\n")
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative=folder.relative_to(ROOT.parent.parent).as_posix()
                project=runtime.request('POST','/api/projects',{'name':'Python '+mode,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,files)
                    mirror.install(browser,project['id'],folder)
                drafts=[]
                for path in ['lib.py','scratch.py','stub.pyi',*CLIENTS]:
                    encoded=base64.b64encode(files[path].encode()).decode()
                    drafts.append({'path':path,'document':{'text':encoded,'saved':encoded,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':'main.py','files':drafts}})
                for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                    runtime.request('PUT','/api/settings',{'key':key,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('answer');"))
                wait(mode+' server',lambda:script(browser,'return lspEvidence.requests.some(request=>request.server);'),90)
                for path in CLIENTS: hover(browser,path,'int')
                hover(browser,'main.py','bytes','environment_value')
                hover(browser,'stub.pyi','str','stub_answer')
                # Change a real buffer without saving. Every open document must
                # receive its own document identity while sharing the native server.
                select(browser,'lib.py')
                caret(browser,"int:\n    return 42",select=True)
                insert(browser,"str:\n    return 'unsaved'")
                wait('unsaved library edit',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('unsaved');"))
                for path in CLIENTS: hover(browser,path,'str')
                servers=script(browser,'return [...new Set(lspEvidence.requests.map(request=>request.server).filter(Boolean))];')
                assert len(servers)==1, 'Tab switches replaced the warm server: '+repr(servers)
                warm=servers[0]
                select(browser,'main.py')
                caret(browser,'value = answer()',len('value = answer()'))
                insert(browser,'.upp')
                caret(browser,'.upp',len('.upp'))
                command(browser,'Complete at cursor')
                wait('string member completion',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.trim().startsWith('upper'));if(!button)return false;button.click();return true;"))
                wait('resolved completion',lambda:click(browser,'Insert completion'))
                wait('member completion insertion',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('answer().upper');"))
                caret(browser,'.upper',1)
                command(browser,'Go to definition')
                wait('standard-library definition',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('builtins.pyi:'));if(!button)return false;button.click();return true;"))
                wait('read-only Python standard library',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return dialog?.textContent.includes('External source · read-only')&&dialog.querySelector('pre')?.textContent.includes('def upper');"))
                assert script(browser,"return !document.querySelector('[role=dialog] textarea:not([readonly])');")
                Path('/tmp/openwebide-python-lsp-stdlib-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                close(browser)
                command(browser,'Format document')
                wait('unsupported formatter message',lambda:script(browser,"return [...document.querySelectorAll('[role=alert]')].some(alert=>alert.textContent.includes('does not support'));"))
                close(browser)
                caret(browser,'environment = environment_value()',len('environment = environment_value()'))
                broken='\nbroken: int = "bad"'
                insert(browser,broken)
                wait('current-source Python diagnostics',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                Path('/tmp/openwebide-python-lsp-diagnostics-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                caret(browser,broken,select=True)
                previous=script(browser,'return lspEvidence.requests.length;')
                insert(browser,'\nbroken: int = 1')
                wait('fixed-source diagnostics clear',lambda:script(browser,'return lspEvidence.requests.slice(arguments[0]).some(request=>request.ready&&request.diagnostics===0)&&!document.querySelector(".editor-diagnostic");',previous))
                caret(browser,'answer'); command(browser,'Go to definition')
                wait('definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('lib.py:'));if(!button)return false;button.click();return true;"))
                wait('unsaved definition navigation',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath==='lib.py'&&input.value.includes('unsaved');"))
                caret(browser,'answer'); command(browser,'Document symbols')
                wait('document symbols',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(button=>button.textContent.trim()==='answer');"))
                close(browser); caret(browser,'answer'); command(browser,'Find references')
                wait('cross-file references',lambda:script(browser,"const text=document.querySelector('[role=dialog]')?.textContent;return arguments[0].every(path=>text?.includes(path+':'));",['lib.py',*CLIENTS]))
                close(browser); caret(browser,'answer'); command(browser,'Rename symbol…')
                wait('rename input',lambda:script(browser,"const input=document.querySelector('[role=dialog] input');if(!input)return false;input.value='renamed_answer';input.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('two-file rename review',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return arguments[0].every(path=>dialog?.textContent.includes(path))&&[...dialog.querySelectorAll('button')].some(button=>button.textContent.trim()==='Apply to editor tabs'&&!button.disabled);",['lib.py',*CLIENTS]))
                assert script(browser, '''
                    const diffs=[...document.querySelectorAll('[role=dialog] .editor-diff')];
                    return diffs.length===2&&diffs.every(diff=>diff.clientHeight>=diff.scrollHeight-1);
                '''), 'Rename previews collapsed instead of allowing dialog scrolling'
                Path('/tmp/openwebide-python-lsp-review-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                for path in ['lib.py',*CLIENTS]:
                    select(browser,path)
                    wait(path+' staged rename',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamed_answer');"))
                wait('rename recovery persisted',lambda:all('renamed_answer' in recover(runtime,project).get(path,'') for path in ['lib.py',*CLIENTS]))
                for path,text in files.items(): assert (folder/path).read_bytes()==text.encode(), 'Review wrote behind Save: '+path
                if mode=='local':
                    assert browser.call('POST','/execute/async',{'script': '''
                        const [folder,files,done]=arguments;
                        (async()=>{const root=await (await navigator.storage.getDirectory()).getDirectoryHandle(folder);
                            for(const [path,text] of Object.entries(files))if(await (await root.getFileHandle(path)).getFile().then(file=>file.text())!==text)return false;
                            return true;})().then(done,error=>done(String(error)));
                    ''','args':[folder.name,files]}) is True, 'Local review wrote behind Save'
                select(browser,'main.py')
                assert script(browser,"const button=[...document.querySelectorAll('button')].find(button=>button.getAttribute('aria-label')==='Close scratch.py');if(!button)return false;button.click();return true;")
                hover(browser,'main.py','str','renamed_answer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')==warm
                evidence=script(browser,'return lspEvidence;')
                assert not evidence['errors'], evidence['errors']
                expected=(host.url if mode=='local' else runtime.url+'/api')+'/plugins/languages/query'
                assert any(request['url']==expected and request['server'] for request in evidence['requests']), evidence
                # Disable the actual installed component, then re-enable it using
                # the same ordinary reviewed binding, retaining unsaved recovery.
                endpoint=f"/api/projects/{project['id']}/plugins"
                binding=next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']=='python')
                package=runtime.request('POST','/api/plugins/package',binding['prepared'])
                changed=runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                disabled=next(entry for entry in changed if entry['id']==binding['id'])
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('disabled package plain editing',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamed_answer')&&!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':installation['revision'],'revision':disabled['revision']})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('re-enabled editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'main.py','str','renamed_answer')
                host.stop_process(); host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'main.py','str','renamed_answer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')!=warm
                print(json.dumps({'mode':mode,'installed_api_six_component':True,'real_python_lsp':True,'pyproject_venv_imports':True,'stub_language_service':True,'unsaved_cross_file_hover':True,'member_completion_and_insertion':True,'diagnostics_and_clear':True,'definition_navigation':True,'standard_library_read_only':True,'symbols_and_references':True,'two_file_rename_review_and_staging':True,'source_unchanged_on_disk':True,'recovery_persisted':True,'close_preserves_warm_service':True,'disable_reenable_preserves_buffers':True,'host_restart_resynchronizes_unsaved_buffers':True}),flush=True)

        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser,"return {editor:document.querySelector('textarea[data-editor-path]')?.value,evidence:window.lspEvidence&&{...lspEvidence,requestCount:lspEvidence.requests.length,requests:lspEvidence.requests.slice(-12)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(dialog=>dialog.textContent.slice(0,3000)),alerts:[...document.querySelectorAll('[role=alert]')].map(alert=>alert.textContent.slice(0,2000))};")),flush=True)
                except Exception:
                    pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists(): Path('/tmp/openwebide-python-lsp-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None: browser.stop()
            runtime.stop(); host.stop(); mirror.stop()


if __name__=='__main__':
    check()
