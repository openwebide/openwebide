#!/usr/bin/env python3
"""Trusted input and pointer contracts for automatic intelligence in both modes.

Uses the built production frontend and an ordinarily installed TypeScript plugin
on a real execution host. Origin-private local handles verify transport parity;
they do not substitute for physical folder-permission or assistive-device checks.
"""
import base64
import json
from pathlib import Path
import runpy
import secrets
import subprocess
import tempfile
import time

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, caret, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']


def key(browser, *keys):
    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'intelligence', 'actions':
        [{'type': 'keyDown', 'value': value} for value in keys]
        + [{'type': 'keyUp', 'value': value} for value in reversed(keys)]}]})


def editor(browser):
    return script(browser, "return document.querySelector('textarea[data-editor-path]')?.value;")


def options(browser):
    return script(browser, "return [...document.querySelectorAll('.editor-completion-option > span')].map(node=>node.textContent);")


def point(browser, token):
    return wait('painted symbol point', lambda: script(browser, r'''
        const input=document.querySelector('textarea[data-editor-path]');
        if(!input||!document.querySelector('.highlight-ready'))return null;
        const at=input.value.indexOf(arguments[0]);if(at<0)return null;
        const before=input.value.slice(0,at),line=before.split('\n').length;
        const column=before.length-(before.lastIndexOf('\n')+1);
        const row=document.querySelector(`.editor-source-line[data-line="${line}"]`);
        if(!row)return null;
        const walker=document.createTreeWalker(row,NodeFilter.SHOW_TEXT);
        let node,offset=column;while((node=walker.nextNode())){if(offset<node.length)break;offset-=node.length;}
        if(!node)return null;
        const range=document.createRange();range.setStart(node,offset);range.setEnd(node,offset+1);
        const rect=range.getBoundingClientRect();if(!rect.height||!rect.width)return null;
        return {x:rect.left+rect.width/2,y:rect.top+rect.height/2};
    ''', token))


def move(browser, coordinates):
    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.dispatchMouseEvent', 'params':
        {'type': 'mouseMoved', 'x': coordinates['x'], 'y': coordinates['y']}})


def bounded(browser, selector):
    assert script(browser, '''
        const node=document.querySelector(arguments[0]);if(!node)return false;
        const r=node.getBoundingClientRect();return r.width>0&&r.height>0&&r.left>=0&&r.top>=0&&r.right<=innerWidth+1&&r.bottom<=innerHeight+1;
    ''', selector), 'Popup escaped the viewport'


EVIDENCE = r'''
window.autoEvidence={requests:[],errors:[],delay:0,hold:false,holdOperation:"completion",fail:false};
window.autoReleases=[];
addEventListener('error',event=>autoEvidence.errors.push(String(event.message)+' '+String(event.error?.stack??'')));
const originalError=console.error;console.error=function(...args){autoEvidence.errors.push(args.map(String).join(' '));return originalError.apply(this,args);};
addEventListener('unhandledrejection',event=>autoEvidence.errors.push(String(event.reason)));
const originalFetch=window.fetch;
window.fetch=async function(...args){
    const url=String(args[0]?.url??args[0]);
    if(!/\/plugins\/languages\/query$/.test(new URL(url,location.href).pathname))return originalFetch.apply(this,args);
    let body={};try{body=JSON.parse(typeof args[1]?.body==='string'?args[1].body:await args[0].clone().text());}catch{}
    const input=body.input??body.selection?.input;
    const record={operation:input?.query?.operation,path:input?.path,digest:input?.source_digest,start:performance.now()};
    autoEvidence.requests.push(record);
    const delay=record.operation==='completion'?autoEvidence.delay:0;
    const response=await originalFetch.apply(this,args);
    try{const data=await response.clone().json();record.server=data.server;record.error=data.error;record.ready=data.diagnostics_ready;record.messages=(data.diagnostics??[]).map(item=>item.message);}catch{}
    if(record.operation===autoEvidence.holdOperation&&autoEvidence.hold){record.held=true;await new Promise(resolve=>autoReleases.push(resolve));record.held=false;}
    if(record.operation==='completion'&&autoEvidence.fail){record.end=performance.now();record.failed=true;throw new Error('Fixture completion transport failure');}
    if(delay)await new Promise(resolve=>setTimeout(resolve,delay));
    record.end=performance.now();return response;
};
'''


def check():
    with tempfile.TemporaryDirectory(prefix='openwebide-automatic-client-') as directory, \
            tempfile.TemporaryDirectory(prefix='automatic-language-', dir=ROOT.parent) as folder:
        state, project_root = Path(directory), Path(folder)
        runtime = Runtime(state / 'spin', frontend=True)
        runtime.state.mkdir()
        host, browser = ExecutionHost(runtime), None
        mirror = ProbeMirror(runtime.url)
        try:
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'automatic-' + secrets.token_hex(6), 'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            installations = host.initialize_defaults()
            installation = next(entry for entry in installations if entry['prepared']['manifest']['name'] == 'javascript-typescript')
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value, 'domain': '127.0.0.1', 'path': cookie.path, 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument', 'params': {'source': EVIDENCE}})
            for mode in ('remote', 'local'):
                directory = project_root / mode
                directory.mkdir()
                files = {
                    'tsconfig.json': json.dumps({'compilerOptions': {'target': 'ES2022', 'module': 'ESNext', 'moduleResolution': 'bundler', 'strict': True, 'noEmit': True, 'types': []}, 'include': ['*.ts']}),
                    'package.json': '{"type":"module"}',
                    'extra.ts': 'export function extraAnswer(): number { return 42; }\r\n',
                    'readonly.ts': 'export const locked: number = 42;\r\n',
                    'main.ts': '// 文😀\r\nconst person = { alpha: 42, alpine: 43 };\r\nconst answer = person.alpha;\r\n',
                }
                for path, text in files.items():
                    (directory / path).write_bytes(text.encode())
                subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(directory)],check=True)
                relative = directory.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name': 'Automatic ' + mode, 'mode': mode, 'path': relative if mode == 'remote' else directory.name}, expected=201)
                if mode == 'local':
                    local_handle(browser, project['id'], user, directory.name, files)
                    mirror.install(browser, project['id'], directory)
                drafts = [{'path': path, 'document': {'text': base64.b64encode(files[path].encode()).decode(), 'saved': base64.b64encode(files[path].encode()).decode(), 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []}, 'scroll': {'top': 0, 'left': 0}, 'read_only': path == 'readonly.ts'} for path in ['main.ts', 'readonly.ts']]
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': {'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None}, 'selected': 'main.ts', 'files': drafts}})
                for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id']), 'theme': 'light' if mode == 'local' else 'dark', 'editor_preferences': json.dumps({'word_wrap': True})}.items():
                    runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait('editor', lambda: editor(browser) and 'const answer' in editor(browser))
                wait('real warm native server', lambda: script(browser, 'return autoEvidence.requests.some(request=>request.server);'), 90)
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nextraAn')
                wait('automatic import suggestion', lambda: 'extraAnswer' in options(browser))
                assert not script(browser, 'return !!document.querySelector("[role=dialog]");')
                bounded(browser, '.editor-completion-popup')
                Path('/tmp/openwebide-completion-popup-' + mode + '.png').write_bytes(base64.b64decode(browser.call('GET', '/screenshot')))
                assert script(browser, '''
                    const input=document.querySelector('textarea[data-editor-path]');
                    return document.activeElement===input&&input.getAttribute('aria-autocomplete')==='list'
                        &&!!document.getElementById(input.getAttribute('aria-activedescendant'));
                ''')
                tree = browser.call('POST', '/goog/cdp/execute', {'cmd': 'Accessibility.getFullAXTree', 'params': {}})['nodes']
                textboxes = [node for node in tree if node.get('role', {}).get('value') == 'textbox' and node.get('name', {}).get('value') == 'Code editor: main.ts']
                assert len(textboxes) == 1
                assert any(prop['name'] == 'autocomplete' and prop['value'].get('value') == 'list' for prop in textboxes[0].get('properties', [])), 'Autocomplete missing from accessibility tree'
                before = editor(browser)
                key(browser, '\ue007')  # Enter resolves and inserts in one gesture.
                wait('completion including import', lambda: 'import { extraAnswer }' in editor(browser) and '\nextraAnswer' in editor(browser))
                key(browser, '\ue009', 'z')
                wait('one undo reverses all completion edits', lambda: editor(browser) == before)
                key(browser, '\ue009', '\ue008', 'z')
                wait('redo restores import', lambda: 'import { extraAnswer }' in editor(browser))

                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nperson.al')
                wait('filtered member suggestions', lambda: options(browser) == ['alpha', 'alpine'])
                key(browser, '\ue015')  # ArrowDown must not move the source caret.
                key(browser, '\ue004')  # Tab accepts while indentation owns Tab.
                wait('keyboard member acceptance', lambda: '\nperson.alpine' in editor(browser))

                caret(browser, 'person.alpha;', len('person.alpha;'))
                script(browser, 'autoEvidence.hold=true;autoEvidence.holdOperation="completion";autoEvidence.requests=[];')
                insert(browser, '\nperson.')
                wait('slow completion started', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='completion'&&request.held);"))
                for text in ['a', 'l']:
                    insert(browser, text)
                    time.sleep(.25)
                assert script(browser, "return autoEvidence.requests.filter(request=>request.operation==='completion').length;") == 1
                script(browser, 'autoEvidence.hold=false;autoReleases.splice(0).forEach(release=>release());')
                wait('latest coalesced completion', lambda: options(browser) == ['alpha', 'alpine'])
                count = script(browser, "return autoEvidence.requests.filter(request=>request.operation==='completion').length;")
                assert count == 2, f'Automatic typing accumulated {count} host requests'
                key(browser, '\ue00c')

                script(browser, 'autoEvidence.requests=[];')
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nqzxNonexistentIdentifier')
                wait('unmatched completion response', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='completion'&&request.end!=null);"))
                browser.call('POST', '/execute/async', {'script':'const done=arguments[0];requestAnimationFrame(()=>requestAnimationFrame(()=>done(true)));','args':[]})
                assert not options(browser)
                selection = script(browser, 'const input=document.querySelector("textarea[data-editor-path]");return [input.selectionStart,input.selectionEnd];')
                move(browser, point(browser, 'person ='))
                wait('pointer symbol information', lambda: script(browser, "return document.querySelector('[data-editor-hover]')?.textContent.includes('alpha');"))
                bounded(browser, '[data-editor-hover]')
                Path('/tmp/openwebide-hover-popup-' + mode + '.png').write_bytes(base64.b64decode(browser.call('GET', '/screenshot')))
                assert script(browser, 'const input=document.querySelector("textarea[data-editor-path]");return [input.selectionStart,input.selectionEnd];') == selection
                card = script(browser, "const r=document.querySelector('[data-editor-hover]').getBoundingClientRect();return {x:r.right-8,y:r.top+8};")
                move(browser, card)
                time.sleep(.3)
                assert script(browser, "return !!document.querySelector('[data-editor-hover]');"), 'Hover closed while moving into its content'
                key(browser, '\ue00c')
                wait('Escape dismisses hover', lambda: not script(browser, "return !!document.querySelector('[data-editor-hover]');"))

                script(browser, 'autoEvidence.hold=true;autoEvidence.holdOperation="hover";autoEvidence.requests=[];')
                move(browser, point(browser, 'person ='))
                wait('held pointer hover', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='hover'&&request.held);"))
                move(browser, point(browser, 'answer ='))
                time.sleep(.5)
                assert not script(browser, "return !!document.querySelector('[data-editor-hover]');"), 'Stale pointer information remained visible'
                script(browser, 'autoEvidence.hold=false;autoReleases.splice(0).forEach(release=>release());')
                wait('latest pointer hover', lambda: script(browser, "return document.querySelector('[data-editor-hover]')?.textContent.includes('answer');"))
                key(browser, '\ue00c')

                script(browser, 'autoEvidence.fail=true;autoEvidence.requests=[];')
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nperson.a')
                wait('automatic transport failure', lambda: script(browser, 'return autoEvidence.requests.some(request=>request.failed);'))
                wait('failed popup closes', lambda: not options(browser))
                assert not script(browser, 'return !!document.querySelector("[role=dialog]");')
                script(browser, 'autoEvidence.fail=false;')
                insert(browser, 'l')
                wait('typing recovers after provider error', lambda: options(browser) == ['alpha', 'alpine'])
                key(browser, '\ue00c')

                script(browser, 'autoEvidence.hold=true;autoEvidence.holdOperation="completion";autoEvidence.requests=[];')
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nperson.')
                wait('completion held before a non-completion edit', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='completion'&&request.held);"))
                insert(browser, '\nconst problem: number = "bad";')
                time.sleep(.55)
                script(browser, 'autoEvidence.hold=false;autoReleases.splice(0).forEach(release=>release());')
                wait('latest diagnostics after abandoned completion', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='synchronize'&&request.ready&&request.messages?.some(message=>message.includes('not assignable')))&&!!document.querySelector('.editor-diagnostic-error');"))
                assert not options(browser)
                script(browser, """document.querySelector('button[aria-label="Code intelligence"]').click();""")
                wait('Problems menu', lambda: script(browser, "const button=[...document.querySelectorAll('[role=menuitem]')].find(node=>node.textContent.startsWith('Problems ('));if(!button)return false;button.click();return true;"))
                wait('current diagnostics reach the UI', lambda: script(browser, "return document.querySelector('[role=dialog]')?.textContent.includes('not assignable');"))
                script(browser, "document.querySelector('[aria-label=\"Close dialog\"]').click();")

                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nperson.')
                wait('member list before composition', lambda: bool(options(browser)))
                key(browser, '\ue00c')
                baseline = editor(browser)
                before = script(browser, "return autoEvidence.requests.filter(request=>request.operation==='completion').length;")
                browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.imeSetComposition', 'params': {'text': 'al', 'selectionStart': 2, 'selectionEnd': 2}})
                time.sleep(.35)
                assert not options(browser), 'Completion opened during composition'
                assert script(browser, "return autoEvidence.requests.filter(request=>request.operation==='completion').length;") == before
                browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.imeSetComposition', 'params': {'text': '', 'selectionStart': 0, 'selectionEnd': 0}})
                wait('cancelled composition restores source', lambda: editor(browser) == baseline)
                assert not options(browser)
                browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.imeSetComposition', 'params': {'text': 'al', 'selectionStart': 2, 'selectionEnd': 2}})
                browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': 'al'}})
                wait('completion after committed composition', lambda: options(browser) == ['alpha', 'alpine'])
                key(browser, '\ue009', 'm')
                key(browser, '\ue009', ' ')
                wait('manual popup in Tab navigation mode', lambda: bool(options(browser)))
                unchanged = editor(browser)
                key(browser, '\ue004')
                assert editor(browser) == unchanged, 'Tab navigation accepted a completion'
                assert script(browser, 'return document.activeElement!==document.querySelector("textarea[data-editor-path]");')
                script(browser, 'document.querySelector("textarea[data-editor-path]").focus();')
                key(browser, '\ue009', 'm')
                key(browser, '\ue009', ' ')
                wait('pointer completion list', lambda: options(browser) == ['alpha', 'alpine'])
                expected = script(browser, 'const input=document.querySelector("textarea[data-editor-path]");return input.value.slice(0,input.selectionStart-2)+"alpine"+input.value.slice(input.selectionEnd);')
                position = script(browser, "const node=[...document.querySelectorAll('.editor-completion-option')].find(node=>node.querySelector('span').textContent==='alpine');const r=node.getBoundingClientRect();return {x:r.left+8,y:r.top+8};")
                for kind in ['mouseMoved', 'mousePressed', 'mouseReleased']:
                    params = {'type': kind, **position}
                    if kind != 'mouseMoved':
                        params.update(button='left', clickCount=1)
                    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.dispatchMouseEvent', 'params': params})
                wait('pointer accepts without losing focus', lambda: editor(browser) == expected and script(browser, 'return document.activeElement===document.querySelector("textarea[data-editor-path]");'))

                script(browser, 'autoEvidence.hold=true;autoEvidence.holdOperation="completion";autoEvidence.requests=[];')
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nperson.a')
                wait('delayed completion requested', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='completion'&&request.held);"))
                caret(browser, 'answer')
                script(browser, 'autoEvidence.hold=false;autoReleases.splice(0).forEach(release=>release());')
                wait('stale response finished', lambda: script(browser, "return autoEvidence.requests.some(request=>request.operation==='completion'&&request.end!=null);"))
                browser.call('POST', '/execute/async', {'script':'const done=arguments[0];requestAnimationFrame(()=>requestAnimationFrame(()=>done(true)));','args':[]})
                assert not options(browser), 'A late completion reopened after the caret moved'
                script(browser, 'autoEvidence.delay=0;')
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\n' * 45 + 'const nearBottom = person.a')
                wait('long source extent', lambda: script(browser, "const scroll=document.querySelector('.editor-scroll-surface');return scroll.scrollHeight>scroll.clientHeight;"))
                script(browser, "const scroll=document.querySelector('.editor-scroll-surface');scroll.scrollTop=scroll.scrollHeight;")
                wait('native caret is visible', lambda: bool(point(browser, 'nearBottom =')))
                insert(browser, 'l')
                wait('wrapped viewport completion', lambda: options(browser) == ['alpha', 'alpine'])
                bounded(browser, '.editor-completion-popup')
                assert script(browser, 'return document.querySelector("textarea[data-editor-path]").wrap=== "soft";')
                expected = script(browser, 'const input=document.querySelector("textarea[data-editor-path]");return input.value.slice(0,input.selectionStart)+"\\n"+input.value.slice(input.selectionEnd);')
                script(browser, "document.querySelector('.editor-scroll-surface').scrollTop=0;")
                wait('offscreen caret hides completion choices', lambda: not options(browser))
                key(browser, '\ue007')
                wait('hidden suggestions preserve ordinary Enter', lambda: editor(browser) == expected)
                caret(browser, 'nearBottom = person.al', len('nearBottom = person.al'))
                script(browser, "const scroll=document.querySelector('.editor-scroll-surface');scroll.scrollTop=scroll.scrollHeight;")
                wait('caret visible before resize check', lambda: bool(point(browser, 'nearBottom =')))
                original_rect = browser.call('GET', '/window/rect')
                key(browser, '\ue009', ' ')
                wait('popup before resize', lambda: bool(options(browser)))
                browser.call('POST', '/window/rect', {'width': original_rect['width'], 'height': max(500, original_rect['height'] - 100)})
                wait('resize dismisses stale placement', lambda: not options(browser))
                browser.call('POST', '/window/rect', original_rect)

                endpoint = f"/api/projects/{project['id']}/plugins"
                binding = next(entry for entry in runtime.request('GET', endpoint) if entry['prepared']['manifest']['name'] == 'javascript-typescript')
                package = runtime.request('POST', '/api/plugins/package', binding['prepared'])
                changed = runtime.request('POST', endpoint, {'action': 'disable', 'id': binding['id'], 'revision': binding['revision']})
                disabled = next(entry for entry in changed if entry['id'] == binding['id'])
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait('disabled provider leaves editing available', lambda: editor(browser) and not script(browser, """return !!document.querySelector('button[aria-label="Code intelligence"]');"""))
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nmissingProviderName')
                time.sleep(.3)
                assert 'missingProviderName' in editor(browser) and not options(browser)
                assert not script(browser, 'return autoEvidence.requests.some(request=>request.operation==="completion");')
                runtime.request('POST', endpoint, {'action': 'enable', 'package': package, 'installation_revision': installation['revision'], 'revision': disabled['revision']})
                browser.call('POST', '/url', {'url': runtime.url + '/'})
                wait('re-enabled provider', lambda: script(browser, """return !!document.querySelector('button[aria-label="Code intelligence"]');"""))
                caret(browser, 'person.alpha;', len('person.alpha;'))
                insert(browser, '\nperson.al')
                wait('automatic completion after re-enabling', lambda: options(browser) == ['alpha', 'alpine'])
                key(browser, '\ue00c')

                assert script(browser, 'const tab=[...document.querySelectorAll("[data-editor-tab]")].find(tab=>tab.dataset.editorTab==="readonly.ts");if(!tab)return false;tab.click();return true;')
                wait('read-only native editor', lambda: script(browser, 'const input=document.querySelector("textarea[data-editor-path]");return input?.dataset.editorPath==="readonly.ts"&&input.readOnly;'))
                wait('read-only hover paint', lambda: bool(point(browser, 'locked')))
                locked = editor(browser)
                script(browser, 'document.querySelector("textarea[data-editor-path]").focus();')
                key(browser, '\ue009', ' ')
                time.sleep(.3)
                assert not options(browser) and editor(browser) == locked
                script(browser, """document.querySelector('button[aria-label="Code intelligence"]').focus();""")
                move(browser, point(browser, 'locked'))
                wait('read-only pointer information', lambda: script(browser, "return document.querySelector('[data-editor-hover]')?.textContent.includes('locked');"))
                key(browser, '\ue00c')
                wait('global Escape dismisses read-only hover', lambda: not script(browser, "return !!document.querySelector('[data-editor-hover]');"))

                assert not script(browser, 'return autoEvidence.errors;'), 'Unhandled browser errors'
                for path, text in files.items():
                    assert (directory / path).read_bytes() == text.encode(), 'Automatic intelligence saved a source file'

                print(json.dumps({'mode': mode, 'real_typescript_lsp': True, 'automatic_completion_popup': True, 'keyboard_selection_and_acceptance': True, 'atomic_import_completion_undo_redo': True, 'bounded_latest_typing_requests': True, 'diagnostics_after_abandoned_completion': True, 'pointer_hover_without_caret_move': True, 'hover_persists_over_content': True, 'hover_after_no_matching_completion': True, 'escape_dismissal': True, 'stale_caret_reply_rejected': True, 'stale_pointer_reply_rejected': True, 'editing_survives_provider_failure': True, 'viewport_bounded': True, 'accessible_active_option': True, 'source_unchanged_on_disk': True, 'pointer_completion_acceptance': True, 'ime_commit_cancel_and_suppression': True, 'tab_focus_navigation_preserved': True, 'wrapped_viewport_and_resize': True, 'hidden_suggestions_preserve_enter': True, 'disabled_provider_plain_editing_and_reenable': True, 'read_only_hover_without_completion': True, 'native_textbox_accessibility': True}), flush=True)
        except BaseException:
            if browser is not None:
                try:
                    print(json.dumps(script(browser, "return {source:document.querySelector('textarea[data-editor-path]')?.value,options:[...document.querySelectorAll('.editor-completion-option')].map(node=>node.textContent),hover:document.querySelector('[data-editor-hover]')?.textContent,evidence:window.autoEvidence,alerts:[...document.querySelectorAll('[role=alert]')].map(node=>node.textContent)};")), flush=True)
                    Path('/tmp/openwebide-completion-popup-failure.png').write_bytes(base64.b64decode(browser.call('GET', '/screenshot')))
                except Exception:
                    pass
            for name, log in [('runtime', runtime.state / 'runtime.log'), ('bridge', host.state / 'bridge.log')]:
                if log.exists():
                    Path('/tmp/openwebide-completion-popup-' + name + '.log').write_bytes(log.read_bytes())
            raise
        finally:
            if browser is not None:
                browser.stop()
            host.stop()
            runtime.stop()
            mirror.stop()


if __name__ == '__main__':
    check()
