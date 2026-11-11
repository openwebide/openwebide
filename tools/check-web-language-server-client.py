#!/usr/bin/env python3
"""Published HTML/CSS/JSON packages in both production editors.

Uses real servers, trusted native input, and ordinary installed packages. Local
OPFS/native marker correspondence is test transport setup, not a permission test.
"""
import base64
import json
from pathlib import Path
import runpy
import secrets
import subprocess
import tempfile

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, caret, click, close, command, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
CASES = {
    'main.html': ('html', '<section><h1>文😀</h1></section>\r\n'),
    'style.css': ('css', '.card{color:red;background-color:white;}\r\n'),
    'theme.scss': ('css', '$theme:red;\r\n.card{color:$theme;}\r\n'),
    'settings.json': ('json', '{"$schema":"./schema.json","count":1}\r\n'),
    'settings.jsonc': ('json', '// 文😀\r\n{"$schema":"./schema.json","count":1}\r\n'),
}
EVIDENCE = r'''
window.webLanguageEvidence={requests:[],errors:[]};
addEventListener('error',event=>webLanguageEvidence.errors.push(String(event.message)));
addEventListener('unhandledrejection',event=>webLanguageEvidence.errors.push(String(event.reason)));
const originalFetch=window.fetch;
window.fetch=async function(...args){
    const url=String(args[0]?.url??args[0]);
    if(!/\/plugins\/languages\/query$/.test(new URL(url,location.href).pathname))return originalFetch.apply(this,args);
    let body={};try{body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());}catch{}
    const input=body.input??body.selection?.input;
    const response=await originalFetch.apply(this,args);
    const record={path:input?.path,operation:input?.query?.operation};
    try{const data=await response.clone().json();record.server=data.server;record.error=data.error;record.ready=data.diagnostics_ready;record.messages=(data.diagnostics??[]).map(item=>item.message);}catch{}
    webLanguageEvidence.requests.push(record);return response;
};
'''


def key(browser, *keys):
    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'web-languages', 'actions':
        [{'type': 'keyDown', 'value': key} for key in keys]
        + [{'type': 'keyUp', 'value': key} for key in reversed(keys)]}]})


def source(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def select(browser, path):
    close(browser)
    assert script(browser, '''
        const tab=[...document.querySelectorAll('[data-editor-tab]')].find(node=>node.dataset.editorTab===arguments[0]);
        if(!tab)return false;tab.click();return true;
    ''', path), 'Missing tab ' + path
    wait(path+' editor', lambda: script(browser, "return document.querySelector('textarea[data-editor-path]')?.dataset.editorPath===arguments[0];", path))


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-web-client-') as state_path, \
            tempfile.TemporaryDirectory(prefix='web-language-project-', dir=ROOT.parent) as project_path:
        state, project_root = Path(state_path), Path(project_path)
        runtime = Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host, browser = ExecutionHost(runtime), None
        mirror = ProbeMirror(runtime.url)
        try:
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username':'web-'+secrets.token_hex(6), 'password':secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            installations = host.initialize_defaults()
            packages = {name:next(entry for entry in installations if entry['prepared']['manifest']['name']==name) for name in ('html','css','json')}
            for package in packages.values():
                assert package['prepared']['manifest']['version']=='0.2.0'
                assert package['prepared']['manifest']['compatibility']['pluginApi']==7
            browser = Browser(state)
            browser.call('POST','/url',{'url':runtime.url+'/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST','/cookie',{'cookie':{'name':cookie.name,'value':cookie.value,'domain':'127.0.0.1','path':'/','httpOnly':True}})
            host.pair_browser(browser)
            browser.call('POST','/goog/cdp/execute',{'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':EVIDENCE}})
            for mode in ('remote','local'):
                folder = project_root/mode
                folder.mkdir()
                files = {path:text for path,(_,text) in CASES.items()}
                files.update({'package.json':'{}', 'schema.json':json.dumps({'type':'object','properties':{'count':{'type':'integer'}},'required':['count']})})
                for path,text in files.items():
                    (folder/path).write_bytes(text.encode())
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST','/api/projects',{'name':'Web '+mode,'mode':mode,'path':relative if mode=='remote' else folder.name},expected=201)
                if mode=='local':
                    local_handle(browser,project['id'],user,folder.name,files)
                    mirror.install(browser,project['id'],folder)
                drafts=[]
                for path in CASES:
                    text=base64.b64encode(files[path].encode()).decode()
                    drafts.append({'path':path,'document':{'text':text,'saved':text,'selections':[{'anchor':0,'head':0}],'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},'selected':'main.html','files':drafts}})
                for name,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                    runtime.request('PUT','/api/settings',{'key':name,'value':value})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait(mode+' editor',lambda:bool(source(browser)))
                for path in CASES:
                    select(browser,path)
                    command(browser,'Document symbols')
                    wait(path+' symbols',lambda:script(browser,"return !!document.querySelector('[role=dialog] button.ui-dropdown-item');"))
                    close(browser)
                    script(browser,"const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(0,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
                    unformatted=source(browser)
                    command(browser,'Format selection')
                    wait(path+' range formatting review',lambda:click(browser,'Apply formatting'))
                    wait(path+' formatting staged',lambda:script(browser,"const input=document.querySelector('textarea[data-editor-path]');return input?.dataset.editorPath===arguments[1]&&input.value!==arguments[0]&&input.value.length>0&&input.dataset.editorNativeValueScope===input.dataset.editorScope&&input.dataset.editorNativePreparing==='false';",unformatted,path))
                    close(browser)
                for paths in [('style.css','theme.scss'),('settings.json','settings.jsonc')]:
                    servers=script(browser,"return [...new Set(webLanguageEvidence.requests.filter(item=>arguments[0].includes(item.path)&&['symbols','range_formatting'].includes(item.operation)).map(item=>item.server).filter(Boolean))];",list(paths))
                    assert len(servers)==1, 'Related dialects did not share their service: '+repr(servers)
                # The automatic acceptance path must enter the snippet field,
                # not leave the caret after the property's trailing semicolon.
                select(browser,'style.css')
                script(browser,"const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(0,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
                insert(browser,'.card { col }')
                caret(browser,'col',3)
                key(browser,'\ue009',' ')
                wait('CSS property suggestion',lambda:script(browser,"return [...document.querySelectorAll('.editor-completion-option > span')].some(node=>node.textContent==='color');"))
                script(browser,"[...document.querySelectorAll('.editor-completion-option')].find(node=>node.querySelector('span')?.textContent==='color').click();")
                wait('CSS snippet insertion',lambda:'color:' in source(browser))
                insert(browser,'red')
                wait('typed snippet value',lambda:'color: red' in source(browser))
                key(browser,'\ue004')
                assert '${' not in source(browser)
                # Real schema diagnostics and provider-labelled Problems UI.
                select(browser,'settings.json')
                caret(browser,'"count": 1',len('"count": '),select=True)
                insert(browser,'"bad"')
                wait('JSON schema diagnostics',lambda:script(browser,"return webLanguageEvidence.requests.some(item=>item.path==='settings.json'&&item.ready&&item.messages.some(message=>message.includes('integer')));"))
                command(browser,'Problems (1)')
                wait('JSON Problems attribution',lambda:script(browser,"return document.querySelector('[role=dialog]')?.textContent.includes('JSON')&&document.querySelector('[role=dialog]')?.textContent.includes('integer');"))
                close(browser)
                # Ordinary binding controls retain current drafts and keep plain
                # editing available, independently for each server family.
                endpoint=f"/api/projects/{project['id']}/plugins"
                for name,path in [('html','main.html'),('css','style.css'),('json','settings.json')]:
                    select(browser,path)
                    before=source(browser)
                    def persisted():
                        recovery=runtime.request('GET',f"/api/projects/{project['id']}/editor-recovery")
                        entry=next((file for file in recovery['state']['files'] if file['path']==path),None)
                        return recovery['state']['selected']==path and entry and base64.b64decode(entry['document']['text']).decode().replace('\r\n','\n')==before
                    wait(name+' unsaved recovery before disable',persisted)
                    binding=next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']==name)
                    package=runtime.request('POST','/api/plugins/package',binding['prepared'])
                    changed=runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                    disabled=next(entry for entry in changed if entry['id']==binding['id'])
                    browser.call('POST','/url',{'url':runtime.url+'/'})
                    wait(name+' disabled editing',lambda:source(browser)==before and not script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                    script(browser,"const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(input.value.length,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
                    insert(browser,'\n')
                    before+='\n'
                    wait(name+' plain typing without provider',lambda:source(browser)==before)
                    wait(name+' disabled draft persists',persisted)
                    runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':packages[name]['revision'],'revision':disabled['revision']})
                    browser.call('POST','/url',{'url':runtime.url+'/'})
                    wait(name+' re-enabled exact draft',lambda:source(browser)==before and script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                    command(browser,'Document symbols')
                    wait(name+' re-enabled symbols',lambda:script(browser,"return !!document.querySelector('[role=dialog] button.ui-dropdown-item');"))
                    close(browser)
                expected={}
                for path in CASES:
                    select(browser,path)
                    expected[path]=source(browser)
                previous=script(browser,'return [...new Set(webLanguageEvidence.requests.map(item=>item.server).filter(Boolean))];')
                host.stop_process()
                host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted editor',lambda:bool(source(browser)))
                for path,text in expected.items():
                    select(browser,path)
                    wait(path+' restored exact draft after host restart',lambda:source(browser)==text)
                    command(browser,'Document symbols')
                    wait(path+' restarted symbols',lambda:script(browser,"return !!document.querySelector('[role=dialog] button.ui-dropdown-item');"))
                    close(browser)
                restarted=script(browser,'return [...new Set(webLanguageEvidence.requests.map(item=>item.server).filter(Boolean))];')
                assert restarted and set(previous).isdisjoint(restarted), 'Host restart reused a stale service identity'
                for path,text in files.items():
                    assert (folder/path).read_bytes()==text.encode(), 'Queries saved '+path
                assert not script(browser,'return webLanguageEvidence.errors;')
                print(json.dumps({'mode':mode,'published_packages':list(packages),'five_dialects':True,'symbols':True,'selection_formatting':True,'automatic_snippet_field_input':True,'json_schema_diagnostics':True,'source_files_unchanged':True,'related_dialects_share_service':True,'independent_disable_reenable_preserves_drafts':True,'plain_typing_without_providers':True,'host_restart_resynchronizes_all_drafts':True}),flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "return {evidence:window.webLanguageEvidence&&{...webLanguageEvidence,requests:webLanguageEvidence.requests.slice(-16)},dialogs:[...document.querySelectorAll('[role=dialog]')].map(node=>node.textContent.slice(0,1500)),alerts:[...document.querySelectorAll('[role=alert]')].map(node=>node.textContent.slice(0,1000))};")),flush=True)
                except Exception:
                    pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists():
                    Path('/tmp/openwebide-web-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None:
                browser.stop()
            mirror.stop()
            runtime.stop()
            host.stop()


if __name__=='__main__':
    check()
