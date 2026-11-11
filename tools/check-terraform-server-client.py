#!/usr/bin/env python3
"""Installed Terraform syntax and real LSP workflows in both production editors.

Build the current frontend/backend and the release bridge with compiled defaults
first. Local OPFS/native marker correspondence verifies the paired adapter; it
does not establish physical folder permissions. No infrastructure is initialized.
"""
import base64
import json
import os
from pathlib import Path
import runpy
import secrets
import subprocess
import sys
import tempfile

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, caret, click, close, command, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT/'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
VARIABLES = 'variable "message" {\r\n  type = string\r\n  default = "disk"\r\n}\r\n'
MAIN = '// 文😀\r\nlocals {\r\n  greeting = var.message\r\n}\r\noutput "greeting" {\r\n  value = local.greeting\r\n}\r\n'
DIALECTS = {
    'main.tf': ('terraform', MAIN, 'greeting'),
    'probe.tfvars': ('terraform-vars', 'message = 42\r\n', 'message'),
    'probe.tfcomponent.hcl': ('terraform-stack', 'variable "probe" { type = string }\r\n', 'probe'),
    'probe.tfdeploy.hcl': ('terraform-deploy', 'deployment "probe" {}\r\n', 'probe'),
    'probe.tfquery.hcl': ('terraform-search', 'list "probe" "example" {}\r\n', 'probe'),
    'probe.policy.hcl': ('terraform-policy', 'policy "probe" {}\r\n', 'probe'),
    'probe.policytest.hcl': ('terraform-policytest', 'test "probe" {}\r\n', 'probe'),
}
EVIDENCE = r'''
window.lspEvidence={requests:[],errors:[]};
addEventListener('error',event=>lspEvidence.errors.push(String(event.message)));
addEventListener('unhandledrejection',event=>lspEvidence.errors.push(String(event.reason)));
const originalFetch=window.fetch;
window.fetch=async function(...args){
    const url=String(args[0]?.url??args[0]);
    if(!/\/plugins\/languages\/query$/.test(new URL(url,location.href).pathname))return originalFetch.apply(this,args);
    let body={};try{body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());}catch{}
    const input=body.input??body.selection?.input??body.call?.input;
    const response=await originalFetch.apply(this,args);
    const record={path:input?.path,language:input?.language,operation:input?.query?.operation};
    try{const data=await response.clone().json();record.server=data.server;record.error=data.error;
        record.ready=data.diagnostics_ready;record.messages=(data.diagnostics??[]).map(item=>item.message);}catch{}
    lspEvidence.requests.push(record);return response;
};
'''


def source(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def select(browser, path):
    close(browser)
    assert script(browser, '''
        const tab=[...document.querySelectorAll('[data-editor-tab]')].find(node=>node.dataset.editorTab===arguments[0]);
        if(!tab)return false;tab.click();return true;
    ''', path), 'Missing tab '+path
    wait(path+' writable editor', lambda: script(browser, '''
        const input=document.querySelector('textarea[data-editor-path]');
        return input?.dataset.editorPath===arguments[0]&&!input.readOnly
            &&input.dataset.editorNativePreparing==='false'
            &&input.dataset.editorNativeValueScope===input.dataset.editorScope;
    ''', path))


def symbols(browser, path, expected):
    select(browser, path)
    command(browser, 'Document symbols')
    wait(path+' document symbols', lambda: script(browser, '''
        return [...document.querySelectorAll('[role=dialog] button.ui-dropdown-item')]
            .some(button=>button.textContent.includes(arguments[0]));
    ''', expected))
    close(browser)


def hover(browser, expected):
    select(browser, 'main.tf')
    def ready():
        caret(browser, 'var.message', 4)
        command(browser, 'Symbol information')
        wait('Terraform hover response', lambda: script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes('Symbol information');"))
        return script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes(arguments[0]);", expected)
    wait(expected+' cross-file variable hover', ready)
    close(browser)


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-terraform-client-') as state_path, \
            tempfile.TemporaryDirectory(prefix='terraform-native-', dir=ROOT.parent) as project_path:
        os.environ.update({'CHECKPOINT_DISABLE':'1', 'TF_IN_AUTOMATION':'1', 'OTEL_TRACES_EXPORTER':'none'})
        state, project_root = Path(state_path), Path(project_path)
        runtime = Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host, browser = ExecutionHost(runtime), None
        mirror = ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST', '/api/auth/register', {'username':'terraform-'+secrets.token_hex(6), 'password':secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            installation = next(entry for entry in host.initialize_defaults() if entry['prepared']['manifest']['name']=='terraform')
            manifest = installation['prepared']['manifest']
            assert manifest['version']=='0.1.0' and manifest['compatibility']['pluginApi']==9
            assert {definition['id'] for definition in manifest['contributions']['languages']}=={item[0] for item in DIALECTS.values()}
            browser = Browser(state)
            browser.call('POST', '/url', {'url':runtime.url+'/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie':{'name':cookie.name, 'value':cookie.value, 'domain':'127.0.0.1', 'path':'/', 'httpOnly':True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd':'Page.addScriptToEvaluateOnNewDocument', 'params':{'source':EVIDENCE}})
            for mode in ('remote', 'local'):
                folder = project_root/mode
                folder.mkdir()
                files = {path:text for path,(_,text,_) in DIALECTS.items()}
                files.update({'variables.tf':VARIABLES, 'format.tf':'locals { formatted=42 }\r\n',
                              'completion.tf':'locals {\r\n  completion_value = 0\r\n}\r\n'})
                for path,text in files.items(): (folder/path).write_bytes(text.encode())
                subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name':'Terraform '+mode, 'mode':mode,
                    'path':relative if mode=='remote' else folder.name}, expected=201)
                if mode=='local':
                    local_handle(browser, project['id'], user, folder.name, files)
                    mirror.install(browser, project['id'], folder)
                drafts = []
                for path,text in files.items():
                    encoded = base64.b64encode(text.encode()).decode()
                    drafts.append({'path':path, 'document':{'text':encoded, 'saved':encoded,
                        'selections':[{'anchor':0,'head':0}], 'collapsed':[]}, 'scroll':{'top':0,'left':0}, 'read_only':False})
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision':0,
                    'state':{'format':1, 'root':{'mode':mode, 'path':relative if mode=='remote' else None},
                             'selected':'main.tf', 'files':drafts}})
                for name,value in {'open_tabs':json.dumps([project['id']]), 'active_project':str(project['id'])}.items():
                    runtime.request('PUT', '/api/settings', {'key':name, 'value':value})
                browser.call('POST', '/url', {'url':runtime.url+'/'})
                wait(mode+' Terraform editor', lambda: source(browser)==MAIN.replace('\r\n','\n'))
                for path,(_,_,symbol) in DIALECTS.items():
                    symbols(browser, path, symbol)
                    wait(path+' component-owned syntax', lambda: script(browser, "const input=document.querySelector('textarea[data-editor-path]'),paint=document.querySelector('.editor-highlight-content');return input&&paint?.dataset.editorScope===input.dataset.editorScope&&!!paint.querySelector(':is(.tok-keyword,.tok-attribute,.tok-number,.tok-string)');"))
                requests = script(browser, 'return lspEvidence.requests;')
                for path,(language,_,_) in DIALECTS.items():
                    assert any(item.get('path')==path and item.get('language')==language and item.get('operation')=='symbols' and item.get('server') for item in requests), (path, requests)
                servers = {item['server'] for item in requests if item.get('server')}
                assert len(servers)==1, 'Terraform dialects did not share one service: '+repr(servers)
                hover(browser, 'string')
                select(browser, 'variables.tf')
                caret(browser, 'type = string\n  default = "disk"', select=True)
                insert(browser, 'type = number\n  default = 42')
                hover(browser, 'number')
                caret(browser, 'var.message', 4); command(browser, 'Go to definition')
                wait('Terraform variable definition', lambda: script(browser, "const button=[...document.querySelectorAll('[role=dialog] button')].find(node=>node.textContent.includes('variables.tf:'));if(!button)return false;button.click();return true;"))
                wait('unsaved Terraform definition', lambda: source(browser) and 'type = number' in source(browser))
                caret(browser, 'message'); command(browser, 'Find references')
                wait('Terraform variable references', lambda: script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes('main.tf:');"))
                close(browser)
                select(browser, 'completion.tf')
                caret(browser, 'completion_value = 0', len('completion_value = '), select=True)
                insert(browser, 'var.')
                command(browser, 'Complete at cursor')
                wait('Terraform variable completion', lambda: script(browser, "const button=[...document.querySelectorAll('[role=dialog] button')].find(node=>!node.disabled&&node.textContent.includes('message'));if(!button)return false;button.click();return true;"))
                wait('resolved Terraform completion', lambda: click(browser, 'Insert completion'))
                wait('Terraform completion applied', lambda: 'var.message' in (source(browser) or ''))
                select(browser, 'format.tf')
                unformatted = source(browser)
                command(browser, 'Format document')
                wait('Terraform formatting review', lambda: click(browser, 'Apply formatting'))
                wait('Terraform formatting applied', lambda: 'formatted = 42' in (source(browser) or ''))
                close(browser)
                caret(browser, 'formatted')
                assert script(browser, "return document.querySelector('textarea[data-editor-path]')===document.activeElement;"), 'Terraform undo must target the current editor'
                modifier = '\ue03d' if sys.platform=='darwin' else '\ue009'
                browser.call('POST', '/actions', {'actions':[{'type':'key', 'id':'terraform-history', 'actions':[
                    {'type':'keyDown','value':modifier}, {'type':'keyDown','value':'z'},
                    {'type':'keyUp','value':'z'}, {'type':'keyUp','value':modifier}]}]})
                wait('Terraform formatting undo', lambda: source(browser)==unformatted)
                select(browser, 'main.tf')
                caret(browser, 'var.message', select=True); insert(browser, 'var.missing')
                wait('Terraform current-source diagnostics', lambda: script(browser, "return lspEvidence.requests.some(item=>item.path==='main.tf'&&item.ready&&item.messages?.some(message=>message.includes('missing')))&&!!document.querySelector('.editor-diagnostic');"))
                previous = script(browser, 'return lspEvidence.requests.length;')
                caret(browser, 'var.missing', select=True); insert(browser, 'var.message')
                wait('Terraform fixed-source diagnostics clear', lambda: script(browser, "return lspEvidence.requests.slice(arguments[0]).some(item=>item.path==='main.tf'&&item.ready&&item.messages?.length===0)&&!document.querySelector('.editor-diagnostic');", previous))

                def persisted():
                    recovery = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                    return {file['path']:base64.b64decode(file['document']['text']).decode().replace('\r\n','\n') for file in recovery['state']['files']}

                before = source(browser)
                wait('Terraform draft persistence', lambda: persisted().get('main.tf')==before and 'type = number' in persisted().get('variables.tf',''))
                endpoint = f"/api/projects/{project['id']}/plugins"
                binding = next(entry for entry in runtime.request('GET',endpoint) if entry['prepared']['manifest']['name']=='terraform')
                package = runtime.request('POST','/api/plugins/package',binding['prepared'])
                changed = runtime.request('POST',endpoint,{'action':'disable','id':binding['id'],'revision':binding['revision']})
                disabled = next(entry for entry in changed if entry['id']==binding['id'])
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('disabled Terraform ordinary editing', lambda: source(browser)==before and not script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]')||!!document.querySelector('.editor-highlight .tok-keyword');"))
                script(browser, "const input=document.querySelector('textarea[data-editor-path]');input.focus();input.setSelectionRange(input.value.length,input.value.length);input.dispatchEvent(new Event('select',{bubbles:true}));")
                insert(browser, '\n')
                before += '\n'
                wait('plain typing without Terraform', lambda: source(browser)==before and persisted().get('main.tf')==before)
                runtime.request('POST',endpoint,{'action':'enable','package':package,'installation_revision':installation['revision'],'revision':disabled['revision']})
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('re-enabled Terraform draft', lambda: source(browser)==before and script(browser,"return !!document.querySelector('button[aria-label=\"Code intelligence\"]');"))
                symbols(browser, 'main.tf', 'greeting')
                hover(browser, 'number')
                expected = persisted()
                previous_servers = script(browser, 'return [...new Set(lspEvidence.requests.map(item=>item.server).filter(Boolean))];')
                host.stop_process(); host.start()
                browser.call('POST','/url',{'url':runtime.url+'/'})
                wait('restarted Terraform editor', lambda: bool(source(browser)))
                for path,(_,_,symbol) in DIALECTS.items():
                    symbols(browser, path, symbol)
                    assert source(browser)==expected[path], 'Host restart changed '+path
                hover(browser, 'number')
                restarted = script(browser, 'return [...new Set(lspEvidence.requests.map(item=>item.server).filter(Boolean))];')
                assert restarted and set(previous_servers).isdisjoint(restarted), 'Restart reused stale Terraform service identity'
                assert not script(browser, 'return lspEvidence.errors;')
                for path,text in files.items(): assert (folder/path).read_bytes()==text.encode(), 'Language workflow saved '+path
                assert not (folder/'.terraform').exists() and not (folder/'terraform.tfstate').exists(), 'Language workflow initialized infrastructure'
                print(json.dumps({'mode':mode, 'published_terraform':'0.1.0', 'seven_dialects':True,
                    'component_owned_syntax':True, 'shared_service':True, 'unsaved_cross_file_types':True,
                    'definition_references_completion':True, 'formatting_review_and_undo':True,
                    'diagnostics_and_clear':True, 'disable_enable_preserves_drafts':True,
                    'host_restart_restores_overlays':True, 'source_files_unchanged':True,
                    'workspace_not_initialized':True}), flush=True)
        except BaseException:
            if browser is not None:
                try: print(json.dumps(script(browser,"return {source:document.querySelector('textarea[data-editor-path]')?.value,evidence:window.lspEvidence,dialogs:[...document.querySelectorAll('[role=dialog]')].map(node=>node.textContent.slice(0,1500)),alerts:[...document.querySelectorAll('[role=alert]')].map(node=>node.textContent.slice(0,1000))};")),flush=True)
                except Exception: pass
            for name,log in [('runtime',runtime.state/'runtime.log'),('bridge',host.state/'bridge.log')]:
                if log.exists(): Path('/tmp/openwebide-terraform-client-'+name+'.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None: browser.stop()
            runtime.stop(); host.stop(); mirror.stop()


if __name__=='__main__':
    check()
