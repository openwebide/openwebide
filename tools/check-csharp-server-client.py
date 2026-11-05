#!/usr/bin/env python3
"""Exercise the installed C# component and real server in both production editors.

Local files use real origin-private handles with the shared native marker mirror.
This checks the paired transport; physical folder permissions remain device checks.
"""
import base64
import json
import os
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
LIB = 'Library/Answers.cs'
CLIENT = 'Client/Consumer.cs'
EXTRA = 'Library/Other.cs'


def select(browser, path):
    close(browser)
    assert script(browser, '''
        const tab=[...document.querySelectorAll('[data-editor-tab]')]
            .find(tab=>tab.dataset.editorTab===arguments[0]);
        if(!tab)return false;tab.click();return true;
    ''', path), 'Missing tab '+path
    wait(path+' native editor', lambda: script(browser, '''
        const input=document.querySelector('textarea[data-editor-path]');
        return input?.dataset.editorPath===arguments[0]
            &&input.dataset.editorNativeValueScope===input.dataset.editorScope
            &&input.dataset.editorNativePreparing==='false';
    ''', path))


def hover(browser, path, expected, token='GetAnswer'):
    select(browser, path)
    def ready():
        caret(browser, token)
        command(browser, 'Symbol information')
        wait('C# hover', lambda: script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes('Symbol information');"))
        return script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes(arguments[0]);", expected)
    wait(path+' '+expected+' hover', ready)
    close(browser)


def replace(browser, token, text):
    caret(browser, token, select=True)
    assert script(browser, '''
        const input=document.querySelector('textarea[data-editor-path]');
        return input===document.activeElement&&input.value.slice(input.selectionStart,input.selectionEnd)===arguments[0];
    ''', token), 'Editor lost the selected replacement'
    insert(browser, text)
    wait('C# native edit', lambda: script(browser, "return document.querySelector('textarea[data-editor-path]')?.value.includes(arguments[0]);", text))


def recover(runtime, project):
    record=runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
    return {entry['path']:base64.b64decode(entry['document']['text']).decode().replace('\r\n','\n') for entry in record['state']['files']}


def files():
    sdk=json.loads((ROOT/'tools/csharp-language-server/versions.json').read_text())['sdk']
    return {
        'Sample.slnx':'<Solution><Project Path="Library/Library.csproj"/><Project Path="Client/Client.csproj"/></Solution>',
        'global.json':json.dumps({'sdk':{'version':sdk,'rollForward':'latestPatch'}}),
        'NuGet.Config':'<configuration><packageSources><clear/></packageSources></configuration>',
        '.gitignore':'**/obj/\n**/bin/\n.cli/\n',
        'Library/Library.csproj':'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup></Project>',
        'Client/Client.csproj':'<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup><TargetFramework>net10.0</TargetFramework></PropertyGroup><ItemGroup><ProjectReference Include="../Library/Library.csproj"/></ItemGroup></Project>',
        LIB:'namespace Library;public static class Answers{public static int GetAnswer()=>42;}\r\n',
        CLIENT:'// 文😀\r\nnamespace Client;public class Consumer{public string Label="😀";public int Value=>Library.Answers.GetAnswer();public string Tag=>nameof(Library.Answers.GetAnswer);}\r\n',
        EXTRA:'namespace Library;public static class Other{public const int Constant=7;}\r\n',
    }


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-csharp-client-') as state_path, tempfile.TemporaryDirectory(prefix='csharp-native-',dir=ROOT.parent) as project_path:
        state, project_root=Path(state_path),Path(project_path)
        runtime=Runtime(state/'spin',frontend=True)
        runtime.state.mkdir()
        host=ExecutionHost(runtime)
        browser=None
        mirror=ProbeMirror(runtime.url)
        try:
            host.start();runtime.start()
            runtime.request('POST','/api/auth/register',{'username':'csharp-'+secrets.token_hex(6),'password':secrets.token_urlsafe(32)},expected=201)
            runtime.acknowledge_release()
            user=runtime.request('GET','/api/auth/me')['user']['id']
            installations=host.initialize_defaults()
            installation=next(entry for entry in installations if entry['prepared']['manifest']['name']=='csharp')
            assert installation['prepared']['manifest']['version']=='0.2.0'
            assert installation['prepared']['manifest']['compatibility']['pluginApi']==8
            browser=Browser(state)
            browser.call('POST','/url',{'url':runtime.url+'/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST','/cookie',{'cookie':{'name':cookie.name,'value':cookie.value,'domain':'127.0.0.1','path':cookie.path,'httpOnly':True}})
            host.pair_browser(browser)
            browser.call('POST','/goog/cdp/execute',{'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':r'''
                window.lspEvidence={requests:[],errors:[]};
                addEventListener('error',e=>lspEvidence.errors.push(String(e.message)));
                addEventListener('unhandledrejection',e=>lspEvidence.errors.push(String(e.reason)));
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
                original=files()
                for path,text in original.items():
                    destination=folder/path;destination.parent.mkdir(parents=True,exist_ok=True);destination.write_bytes(text.encode())
                subprocess.run(['dotnet','restore','Sample.slnx','--configfile','NuGet.Config','--disable-build-servers'],cwd=folder,env={**os.environ,'DOTNET_CLI_HOME':str(folder/'.cli'),'DOTNET_CLI_TELEMETRY_OPTOUT':'1','DOTNET_GENERATE_ASPNET_CERTIFICATE':'false','DOTNET_NOLOGO':'1'},check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative=folder.relative_to(ROOT.parent.parent).as_posix()
                project=runtime.request('POST','/api/projects',{'name':'C# '+mode,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,original)
                    mirror.install(browser,project['id'],folder)
                drafts=[]
                for path in [LIB,CLIENT,EXTRA]:
                    encoded=base64.b64encode(original[path].encode()).decode()
                    drafts.append({'path':path,'document':{'text':encoded,'saved':encoded,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':CLIENT,'files':drafts}})
                for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():runtime.request('PUT','/api/settings',{'key':key,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' C# editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('GetAnswer');"))
                wait(mode+' C# server',lambda:script(browser,'return lspEvidence.requests.some(request=>request.server);'),90)
                hover(browser,CLIENT,'int')
                warm=script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')
                select(browser,LIB)
                replace(browser,'int GetAnswer()=>42','string GetAnswer()=>"unsaved"')
                hover(browser,CLIENT,'string')
                wait('cross-project type diagnostic',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                previous=script(browser,'return lspEvidence.requests.length;')
                replace(browser,'public int Value','public string Value')
                wait('C# diagnostic clear',lambda:script(browser,'return lspEvidence.requests.slice(arguments[0]).some(request=>request.ready&&request.diagnostics===0)&&!document.querySelector(".editor-diagnostic");',previous))
                replace(browser,'GetAnswer()','GetAn()')
                caret(browser,'GetAn',len('GetAn'))
                command(browser,'Complete at cursor')
                wait('C# member completion',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.trim().startsWith('GetAnswer'));if(!button)return false;button.click();return true;"))
                wait('resolved member completion',lambda:click(browser,'Insert completion'))
                wait('completion inserted',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('GetAnswer()');"))
                command(browser,'Format document')
                wait('C# formatting review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                assert click(browser,'Apply formatting')
                wait('C# formatting staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('Value =>');"))
                caret(browser,'string Label');command(browser,'Go to definition')
                wait('framework definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('String.cs:'));if(!button)return false;button.click();return true;"))
                wait('read-only .NET framework source',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return dialog?.textContent.includes('External source · read-only')&&dialog.querySelector('pre')?.textContent.includes('String');"))
                assert script(browser,"return !document.querySelector('[role=dialog] textarea:not([readonly])');")
                Path('/tmp/openwebide-csharp-lsp-framework-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                close(browser)
                caret(browser,'GetAnswer');command(browser,'Go to definition')
                wait('C# definition target',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.includes('Library/Answers.cs:'));if(!button)return false;button.click();return true;"))
                wait('unsaved C# definition',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath===arguments[0]&&input.value.includes('unsaved');",LIB))
                caret(browser,'GetAnswer');command(browser,'Document symbols')
                wait('C# symbols',lambda:script(browser,"return [...document.querySelectorAll('[role=dialog] button')].some(button=>button.textContent.includes('GetAnswer'));"))
                close(browser);caret(browser,'GetAnswer');command(browser,'Find references')
                wait('cross-project references',lambda:script(browser,"const text=document.querySelector('[role=dialog]')?.textContent;return arguments[0].every(path=>text?.includes(path+':'));",[LIB,CLIENT]))
                close(browser);caret(browser,'GetAnswer');command(browser,'Rename symbol…')
                wait('C# rename input',lambda:script(browser,"const input=document.querySelector('[role=dialog] input');if(!input)return false;input.value='RenamedAnswer';input.dispatchEvent(new Event('input',{bubbles:true}));return true;"))
                assert click(browser,'Review rename')
                wait('two-project rename review',lambda:script(browser,"const dialog=document.querySelector('[role=dialog]');return arguments[0].every(path=>dialog?.textContent.includes(path))&&[...dialog.querySelectorAll('button')].some(button=>button.textContent.trim()==='Apply to editor tabs'&&!button.disabled);",[LIB,CLIENT]))
                assert script(browser,"const diffs=[...document.querySelectorAll('[role=dialog] .editor-diff')];return diffs.length===2&&diffs.every(diff=>diff.clientHeight>=diff.scrollHeight-1);")
                Path('/tmp/openwebide-csharp-lsp-review-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                for path in [LIB,CLIENT]:
                    select(browser,path)
                    wait('staged C# rename',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('RenamedAnswer');"))
                select(browser,CLIENT)
                replace(browser,'public string Value','public StringBuilder Value')
                replace(browser,'Library.Answers.RenamedAnswer()','new StringBuilder()')
                wait('missing C# namespace diagnostic',lambda:script(browser,"return !!document.querySelector('.editor-diagnostic');"))
                caret(browser,'StringBuilder',select=True);command(browser,'Code actions')
                wait('C# using action',lambda:script(browser,"const button=[...document.querySelectorAll('[role=dialog] button')].find(button=>button.textContent.trim().startsWith('using System.Text;')&&!button.disabled);if(!button)return false;button.click();return true;"))
                wait('resolved C# import review',lambda:script(browser,"return !!document.querySelector('[role=dialog] .diff-line.add');"))
                Path('/tmp/openwebide-csharp-lsp-import-'+mode+'.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                assert click(browser,'Apply to editor tabs')
                wait('using directive staged',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('using System.Text;');"))
                wait('C# recovery persisted',lambda:all('RenamedAnswer' in recover(runtime,project).get(path,'') for path in [LIB,CLIENT]) and 'using System.Text;' in recover(runtime,project).get(CLIENT,''))
                for path,text in original.items():assert (folder/path).read_bytes()==text.encode(),'C# changes wrote behind Save: '+path
                if mode=='local':
                    assert browser.call('POST','/execute/async',{'script': '''
                        const [folder,files,done]=arguments;
                        (async()=>{const root=await (await navigator.storage.getDirectory()).getDirectoryHandle(folder);
                            for(const [path,text] of Object.entries(files)){
                                let parent=root;const parts=path.split('/'),leaf=parts.pop();
                                for(const part of parts)parent=await parent.getDirectoryHandle(part);
                                if(await (await parent.getFileHandle(leaf)).getFile().then(file=>file.text())!==text)return false;
                            }return true;})().then(done,error=>done(String(error)));
                    ''','args':[folder.name,original]}) is True,'Local C# review wrote behind Save'
                assert script(browser,"const button=[...document.querySelectorAll('button')].find(button=>button.getAttribute('aria-label')==='Close Library/Other.cs');if(!button)return false;button.click();return true;")
                hover(browser,CLIENT,'string','RenamedAnswer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')==warm
                evidence=script(browser,'return lspEvidence;')
                assert not evidence['errors'],evidence['errors']
                expected=(host.url if mode=='local' else runtime.url+'/api')+'/plugins/languages/query'
                assert any(request['url']==expected and request['server'] for request in evidence['requests'])
                endpoint=f"/api/projects/{project['id']}/plugins"
                binding=next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']=='csharp')
                package=runtime.request('POST','/api/plugins/package',binding['prepared'])
                changed=runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                disabled=next(entry for entry in changed if entry['id']==binding['id'])
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('disabled C# preserves drafts',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.value.includes('RenamedAnswer')&&!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':installation['revision'],'revision':disabled['revision']})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('re-enabled C# editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,CLIENT,'string','RenamedAnswer')
                host.stop_process();host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted C# editor',lambda:script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                hover(browser,CLIENT,'string','RenamedAnswer')
                assert script(browser,'return lspEvidence.requests.filter(request=>request.server).at(-1).server;')!=warm
                print(json.dumps({'mode':mode,'installed_api_eight_component':True,'real_csharp_lsp':True,'multi_project_slnx':True,'unsaved_cross_project_hover':True,'diagnostics_and_clear':True,'member_completion_and_insertion':True,'formatting_review_and_staging':True,'definition_navigation':True,'read_only_framework_definition':True,'symbols_and_cross_project_references':True,'two_project_rename_review_and_staging':True,'deferred_using_action_review_and_staging':True,'source_unchanged_on_disk':True,'recovery_persisted':True,'close_preserves_warm_solution':True,'disable_reenable_preserves_buffers':True,'host_restart_resynchronizes_unsaved_buffers':True}),flush=True)
        except BaseException:
            if browser is not None:
                try:
                    Path('/tmp/openwebide-csharp-lsp-failure.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                    print(json.dumps(script(browser,"const input=document.querySelector('textarea[data-editor-path]');return {editor:input?.value,native:input&&{...input.dataset},focused:input===document.activeElement,evidence:window.lspEvidence&&{...lspEvidence,requestCount:lspEvidence.requests.length,requests:lspEvidence.requests.slice(-12)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(dialog=>dialog.textContent.slice(0,3000)),alerts:[...document.querySelectorAll('[role=alert]')].map(alert=>alert.textContent.slice(0,2000))};")),flush=True)
                except Exception:pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists():Path('/tmp/openwebide-csharp-lsp-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None:browser.stop()
            runtime.stop();host.stop();mirror.stop()


if __name__=='__main__':check()
