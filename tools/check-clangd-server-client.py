#!/usr/bin/env python3
"""Exercise the installed C/C++ component and real server in both production editors.

Build the production frontend, WASI backend and bundled-defaults release bridge
first. Local files use origin-private handles with the shared test marker mirror;
this verifies the paired native transport, not physical folder permissions.
"""
import base64
import json
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
CLIENTS = ['main.c', 'main.cpp']


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
    with tempfile.TemporaryDirectory(prefix='openwebide-clangd-client-') as state_path, tempfile.TemporaryDirectory(prefix='clangd-native-', dir=ROOT.parent) as project_path:
        state, project_root=Path(state_path), Path(project_path).resolve()
        runtime=Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host=ExecutionHost(runtime)
        browser=None
        mirror=ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST','/api/auth/register',{'username':'clangd-'+secrets.token_hex(6),'password':secrets.token_urlsafe(32)},expected=201)
            runtime.acknowledge_release()
            user=runtime.request('GET','/api/auth/me')['user']['id']
            installations=host.initialize_defaults()
            installation=next(entry for entry in installations if entry['prepared']['manifest']['name']=='c-cpp')
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
            for mode,layout in product(('remote','local'),('database','config')):
                folder=project_root/(mode+'-'+layout)
                folder.mkdir()
                header='#pragma once\r\n#ifndef BUILD_VALUE\r\n#error Missing project flags\r\n#endif\r\nstatic inline int answer(void) { return BUILD_VALUE; }\r\n'
                files={
                    'include/api.hpp':header,
                    'include/c_api.h':header,
                    'main.c':'#include "c_api.h" // IWYU pragma: keep\r\nconst char *note = "文😀"; long c_value(void) { return answer(); }\r\n',
                    'main.cpp':'#include "api.hpp" // IWYU pragma: keep\r\nconst char *note = "文😀"; auto value = answer();\r\n',
                    'scratch.cpp':'int scratch = 1;\r\n',
                    'format.c':'int c_formatted(void){return 1;}\r\n',
                    'completion.c':'struct Item { int member; };\r\nint c_render(void) { struct Item item; return 0; }\r\n',
                    'format.cpp':'int formatted(){return 1;}\r\n',
                    'completion.cpp':'struct Item { int member; };\r\nint render() { Item item; return 0; }\r\n',
                }
                if layout=='database':
                    files['compile_commands.json']=json.dumps([
                        {'directory':str(folder),'file':path,'arguments':[compiler,standard,'-Iinclude','-DBUILD_VALUE=42','-c',path]}
                        for path,compiler,standard in [('main.c','clang','-std=c17'),('main.cpp','clang++','-std=c++20')]])
                else:
                    files['.clangd']='CompileFlags:\n  Add: [-Iinclude, -DBUILD_VALUE=42]\n---\nIf:\n  PathMatch: .*\\.(c|h)$\nCompileFlags:\n  Add: [-xc, -std=c17]\n---\nIf:\n  PathMatch: .*\\.(cpp|hpp)$\nCompileFlags:\n  Add: [-xc++, -std=c++20]\n'
                for path,value in files.items():
                    (folder/path).parent.mkdir(parents=True,exist_ok=True)
                    (folder/path).write_bytes(value.encode())
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative=folder.relative_to(ROOT.parent.parent).as_posix()
                project=runtime.request('POST','/api/projects',{'name':'C/C++ '+mode+' '+layout,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,files)
                    mirror.install(browser,project['id'],folder)
                drafts=[]
                for path in ['include/c_api.h','include/api.hpp','scratch.cpp','format.c','completion.c','format.cpp','completion.cpp',*CLIENTS]:
                    encoded=base64.b64encode(files[path].encode()).decode()
                    drafts.append({'path':path,'document':{'text':encoded,'saved':encoded,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':'main.cpp','files':drafts}})
                for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                    runtime.request('PUT','/api/settings',{'key':key,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('answer');"))
                wait(mode+' server',lambda:script(browser,'return lspEvidence.requests.some(request=>request.server);'),90)
                for path in CLIENTS: hover(browser,path,'int')
                # Change a real buffer without saving. Every open document must
                # receive its own document identity while sharing the native server.
                for path in ['include/c_api.h','include/api.hpp']:
                    select(browser,path)
                    caret(browser,'inline int answer',select=True)
                    insert(browser,'inline long answer')
                    wait('unsaved header edit',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('inline long');"))
                for path in CLIENTS: hover(browser,path,'long')
                servers=script(browser,'return [...new Set(lspEvidence.requests.map(request=>request.server).filter(Boolean))];')
                assert len(servers)==1, 'Tab switches replaced the warm server: '+repr(servers)
                warm=servers[0]
                # Exercise C's full feature surface as well as C++ below.
                select(browser,'completion.c')
                caret(browser,'return 0',len('return '),select=True)
                insert(browser,'item.mem')
                caret(browser,'item.mem',len('item.mem'))
                command(browser,'Complete at cursor')
                wait('C member completion',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('member'));if(!button)return false;button.click();return true;"))
                wait('C resolved completion',lambda:click(browser,'Insert completion'))
                wait('C completion insertion',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('item.member');"))
                select(browser,'format.c')
                command(browser,'Format document')
                wait('C formatting review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                assert click(browser,'Apply formatting')
                wait('C formatting staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('int c_formatted(void) {');"))
                select(browser,'main.c')
                caret(browser,'return answer(); }',len('return answer(); }'))
                c_broken='\nint c_broken = "bad";'
                insert(browser,c_broken)
                wait('C diagnostics',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                caret(browser,c_broken,select=True)
                previous=script(browser,'return lspEvidence.requests.length;')
                insert(browser,'\nint c_broken = 1;')
                wait('C diagnostics clear',lambda:script(browser,'return lspEvidence.requests.slice(arguments[0]).some(request=>request.ready&&request.diagnostics===0)&&!document.querySelector(".editor-diagnostic");',previous))
                caret(browser,'answer'); command(browser,'Go to definition')
                wait('C definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('include/c_api.h:'));if(!button)return false;button.click();return true;"))
                wait('C unsaved definition navigation',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath==='include/c_api.h'&&input.value.includes('inline long');"))
                caret(browser,'answer'); command(browser,'Document symbols')
                wait('C document symbols',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(button=>button.textContent.trim()==='answer');"))
                close(browser); caret(browser,'answer'); command(browser,'Find references')
                wait('C cross-file references',lambda:script(browser,"const text=document.querySelector('[role=dialog]')?.textContent;return arguments[0].every(path=>text?.includes(path+':'));",['include/c_api.h','main.c']))
                close(browser); caret(browser,'answer'); command(browser,'Rename symbol…')
                wait('C rename input',lambda:script(browser,"const input=document.querySelector('[role=dialog] input');if(!input)return false;input.value='c_answer';input.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('C header/source rename review',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return arguments[0].every(path=>dialog?.textContent.includes(path))&&[...dialog.querySelectorAll('button')].some(button=>button.textContent.trim()==='Apply to editor tabs'&&!button.disabled);",['include/c_api.h','main.c']))
                Path('/tmp/openwebide-clangd-lsp-c-review-'+mode+'-'+layout+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                for path in ['include/c_api.h','main.c']:
                    select(browser,path)
                    wait(path+' C rename staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('c_answer');"))
                wait('C rename recovery',lambda:all('c_answer' in recover(runtime,project).get(path,'') for path in ['include/c_api.h','main.c']))
                select(browser,'completion.cpp')
                caret(browser,'return 0',len('return '),select=True)
                insert(browser,'item.mem')
                caret(browser,'item.mem',len('item.mem'))
                command(browser,'Complete at cursor')
                wait('member completion',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('member'));if(!button)return false;button.click();return true;"))
                wait('resolved completion',lambda:click(browser,'Insert completion'))
                wait('completion insertion',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('item.member');"))
                select(browser,'format.cpp')
                command(browser,'Format document')
                wait('formatting review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                assert click(browser,'Apply formatting')
                wait('formatting staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('int formatted() {');"))
                select(browser,'main.cpp')
                caret(browser,'auto value = answer();',len('auto value = answer();'))
                broken='\nint broken = "bad";'
                insert(browser,broken)
                wait('current-source C++ diagnostics',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                Path('/tmp/openwebide-clangd-lsp-diagnostics-'+mode+'-'+layout+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                caret(browser,broken,select=True)
                previous=script(browser,'return lspEvidence.requests.length;')
                insert(browser,'\nint broken = 1;')
                wait('fixed-source diagnostics clear',lambda:script(browser,'return lspEvidence.requests.slice(arguments[0]).some(request=>request.ready&&request.diagnostics===0)&&!document.querySelector(".editor-diagnostic");',previous))
                caret(browser,'answer'); command(browser,'Go to definition')
                wait('definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('include/api.hpp:'));if(!button)return false;button.click();return true;"))
                wait('unsaved definition navigation',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath==='include/api.hpp'&&input.value.includes('inline long');"))
                caret(browser,'answer'); command(browser,'Document symbols')
                wait('document symbols',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(button=>button.textContent.trim()==='answer');"))
                close(browser); caret(browser,'answer'); command(browser,'Find references')
                wait('cross-file references',lambda:script(browser,"const text=document.querySelector('[role=dialog]')?.textContent;return arguments[0].every(path=>text?.includes(path+':'));",['include/api.hpp','main.cpp']))
                close(browser); caret(browser,'answer'); command(browser,'Rename symbol…')
                wait('rename input',lambda:script(browser,"const input=document.querySelector('[role=dialog] input');if(!input)return false;input.value='renamed_answer';input.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('header and source rename review',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return arguments[0].every(path=>dialog?.textContent.includes(path))&&[...dialog.querySelectorAll('button')].some(button=>button.textContent.trim()==='Apply to editor tabs'&&!button.disabled);",['include/api.hpp','main.cpp']))
                assert script(browser, '''
                    const diffs=[...document.querySelectorAll('[role=dialog] .editor-diff')];
                    return diffs.length>=2&&diffs.every(diff=>diff.clientHeight>=diff.scrollHeight-1);
                '''), 'Rename previews collapsed instead of allowing dialog scrolling'
                Path('/tmp/openwebide-clangd-lsp-review-'+mode+'-'+layout+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                for path in ['include/api.hpp','main.cpp']:
                    select(browser,path)
                    wait(path+' staged rename',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamed_answer');"))
                wait('rename recovery persisted',lambda:all('renamed_answer' in recover(runtime,project).get(path,'') for path in ['include/api.hpp','main.cpp']))
                for path,text in files.items(): assert (folder/path).read_bytes()==text.encode(), 'Review wrote behind Save: '+path
                if mode=='local':
                    assert browser.call('POST','/execute/async',{'script': '''
                        const [folder,files,done]=arguments;
                        (async()=>{const root=await (await navigator.storage.getDirectory()).getDirectoryHandle(folder);
                            for(const [path,text] of Object.entries(files)){let parent=root;const parts=path.split('/');const leaf=parts.pop();for(const part of parts)parent=await parent.getDirectoryHandle(part);if(await (await parent.getFileHandle(leaf)).getFile().then(file=>file.text())!==text)return false;}
                            return true;})().then(done,error=>done(String(error)));
                    ''','args':[folder.name,files]}) is True, 'Local review wrote behind Save'
                select(browser,'main.cpp')
                assert script(browser,"const button=[...document.querySelectorAll('button')].find(button=>button.getAttribute('aria-label')==='Close scratch.cpp');if(!button)return false;button.click();return true;")
                hover(browser,'main.cpp','long','renamed_answer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')==warm
                evidence=script(browser,'return lspEvidence;')
                assert not evidence['errors'], evidence['errors']
                expected=(host.url if mode=='local' else runtime.url+'/api')+'/plugins/languages/query'
                assert any(request['url']==expected and request['server'] for request in evidence['requests']), evidence
                # Disable the actual installed component, then re-enable it using
                # the same ordinary reviewed binding, retaining unsaved recovery.
                endpoint=f"/api/projects/{project['id']}/plugins"
                binding=next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']=='c-cpp')
                package=runtime.request('POST','/api/plugins/package',binding['prepared'])
                changed=runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                disabled=next(entry for entry in changed if entry['id']==binding['id'])
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('disabled package plain editing',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('renamed_answer')&&!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':installation['revision'],'revision':disabled['revision']})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('re-enabled editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'main.cpp','long','renamed_answer')
                reenabled=script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')
                host.stop_process(); host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,'main.cpp','long','renamed_answer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')!=reenabled
                print(json.dumps({'mode':mode,'layout':layout,'installed_api_seven_component':True,'real_clangd_lsp':True,'project_compilation_flags':True,'c_cpp_one_service':True,'c_code_intelligence':True,'cpp_code_intelligence':True,'unsaved_cross_file_hover':True,'member_completion_and_insertion':True,'formatting_review_and_staging':True,'diagnostics_and_clear':True,'definition_navigation':True,'symbols_and_references':True,'header_source_rename_review_and_staging':True,'source_unchanged_on_disk':True,'recovery_persisted':True,'close_preserves_warm_service':True,'disable_reenable_preserves_buffers':True,'host_restart_resynchronizes_unsaved_buffers':True}),flush=True)

        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser,"const input=document.querySelector('textarea[data-editor-path]');return {editor:input?.value,native:input&&{readOnly:input.readOnly,...input.dataset},evidence:window.lspEvidence&&{...lspEvidence,requestCount:lspEvidence.requests.length,requests:lspEvidence.requests.slice(-12)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(dialog=>dialog.textContent.slice(0,3000)),alerts:[...document.querySelectorAll('[role=alert]')].map(alert=>alert.textContent.slice(0,2000))};")),flush=True)
                except Exception:
                    pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists(): Path('/tmp/openwebide-clangd-lsp-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None: browser.stop()
            runtime.stop(); host.stop(); mirror.stop()


if __name__=='__main__':
    check()
