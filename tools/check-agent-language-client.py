#!/usr/bin/env python3
"""Production agent language queries with real installed TypeScript services.

Only the model is scripted. The production browser, native/Spin agent loops,
installed package selection, authority, VFS and stdio language server are real.
OPFS discovery-marker correspondence is not a physical folder-permission test.
"""
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import json
from pathlib import Path
import runpy
import secrets
import subprocess
import tempfile
from threading import Lock, Thread
import time
import urllib.request

from language_host_support import ExecutionHost
from language_server_client_support import ProbeMirror, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
SOURCE = '// 文😀\r\nexport const answer = 42;\r\nexport function read() { return answer; }\r\n'
DRAFT = SOURCE.replace('answer = 42', 'answer: string = 42')
OPS = ['symbols', 'definitions', 'references', 'diagnostics']


class Model:
    def __init__(self):
        self.lock, self.results, self.requests = Lock(), {}, {}
        model = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def reply(self, value, stream=False, status=200):
                data = value.encode() if stream else json.dumps(value).encode()
                self.send_response(status)
                self.send_header('Content-Type', 'text/event-stream' if stream else 'application/json')
                self.send_header('Content-Length', str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                if self.path == '/props':
                    self.reply({'default_generation_settings': {'n_ctx': 32768},
                                'chat_template': 'fixture',
                                'chat_template_caps': {'supports_tool_calls': True}})
                elif self.path == '/v1/models':
                    self.reply({'data': [{'id': 'agent-fixture', 'object': 'model'}]})
                else:
                    self.reply({'error': 'Unavailable fixture metadata'}, status=404)

            def do_POST(self):
                if self.headers.get('Transfer-Encoding', '').lower() == 'chunked':
                    parts, size = [], 0
                    while True:
                        length = int(self.rfile.readline().split(b';', 1)[0].strip(), 16)
                        if not length:
                            while self.rfile.readline().strip():
                                pass
                            break
                        size += length
                        if size > 4 * 1024 * 1024:
                            raise ValueError('Model fixture request exceeds its byte limit')
                        parts.append(self.rfile.read(length))
                        assert self.rfile.read(2) == b'\r\n'
                    raw = b''.join(parts)
                else:
                    raw = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                body = json.loads(raw)
                if self.path == '/apply-template':
                    self.reply({'prompt': json.dumps(body.get('messages', []))})
                    return
                if self.path == '/tokenize':
                    self.reply({'tokens': [1] * max(1, len(str(body.get('content', ''))) // 3)})
                    return
                if self.path != '/v1/chat/completions':
                    self.reply({'error': 'Unknown fixture operation'}, status=404)
                    return
                messages = body.get('messages', [])
                last_user = next((index for index in reversed(range(len(messages)))
                                  if messages[index].get('role') == 'user'), -1)
                user = messages[last_user].get('content', '') if last_user >= 0 else ''
                marker = 'AGENT_PROBE_CASE='
                case = user.split(marker, 1)[-1].split()[0] if isinstance(user, str) and marker in user else None
                tools = {tool['function']['name'] for tool in body.get('tools', [])}
                outputs = []
                for message in messages[last_user + 1:]:
                    if message.get('role') == 'tool':
                        try:
                            outputs.append(json.loads(message['content']))
                        except (ValueError, TypeError):
                            outputs.append({'error': message.get('content', '')})
                if case:
                    with model.lock:
                        model.results[case] = outputs
                        model.requests[case] = model.requests.get(case, 0) + 1
                index = len(outputs)
                # Push diagnostics may finish after the first synchronization.
                pending = outputs and outputs[-1].get('operation') == 'diagnostics' and not outputs[-1].get('ready')
                failed = any('error' in output for output in outputs)
                if case and 'code_intelligence' in tools and not failed and (index < len(OPS) or pending and index < 12):
                    operation = OPS[min(index, len(OPS) - 1)]
                    arguments = {'operation': operation, 'path': 'main.ts'}
                    if operation in ('definitions', 'references'):
                        prefix = SOURCE[:SOURCE.rfind('answer')]
                        arguments.update(line=prefix.count('\n') + 1,
                                         column=len(prefix.rsplit('\n', 1)[-1].encode('utf-16-le')) // 2 + 1)
                    if pending:
                        time.sleep(.15)
                    message = {'role': 'assistant', 'tool_calls': [{
                        'id': f'language-{index}', 'type': 'function',
                        'function': {'name': 'code_intelligence', 'arguments': json.dumps(arguments)}}]}
                    finish = 'tool_calls'
                else:
                    message = {'role': 'assistant', 'content': 'AGENT_PROBE_DONE ' + (case or 'metadata')}
                    finish = 'stop'
                if body.get('stream'):
                    delta = dict(message)
                    if 'tool_calls' in delta:
                        delta['tool_calls'] = [dict(value, index=index) for index, value in enumerate(delta['tool_calls'])]
                    chunks = [{'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
                              {'choices': [{'index': 0, 'delta': {}, 'finish_reason': finish}]}]
                    self.reply(''.join('data: ' + json.dumps(chunk) + '\n\n' for chunk in chunks) + 'data: [DONE]\n\n', stream=True)
                else:
                    self.reply({'choices': [{'index': 0, 'message': message, 'finish_reason': finish}],
                                'usage': {'prompt_tokens': 100, 'completion_tokens': 20}})

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.url = 'http://127.0.0.1:' + str(self.server.server_port)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)

    def verify(self, case, disabled=False):
        with self.lock:
            outputs = list(self.results.get(case, []))
        assert outputs, case + ': no tool results reached the model'
        if disabled:
            assert 'No enabled language server' in outputs[0].get('error', ''), case + ': disabled provider did not fail explicitly'
            return
        assert {output.get('operation') for output in outputs} == set(OPS), case + ': incomplete agent operations'
        assert all(output.get('source_digest') == hashlib.sha256(SOURCE.encode()).hexdigest()
                   and output.get('source') == 'workspace_files' and not output.get('truncated')
                   for output in outputs), case + ': wrong source or truncated result'
        for operation in OPS[:3]:
            output = next(output for output in outputs if output['operation'] == operation)
            assert len(output['records']) > 1, case + ': missing ' + operation + ' locations'
        assert outputs[-1]['ready'], case + ': diagnostics stayed pending'
        assert not any(record.get('kind') == 'diagnostic' for record in outputs[-1]['records']), case + ': editor draft leaked into agent diagnostics'


def main():
    with tempfile.TemporaryDirectory(prefix='agent-language-client-') as state_path, \
            tempfile.TemporaryDirectory(prefix='agent-language-project-', dir=ROOT.parent) as project_path:
        state, project_root = Path(state_path), Path(project_path)
        runtime = Runtime(state/'spin', frontend=True)
        runtime.state.mkdir()
        host, model, browser = ExecutionHost(runtime), Model(), None
        mirror = ProbeMirror(runtime.url)
        try:
            host.start()
            runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'agent-'+secrets.token_hex(6), 'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            host.initialize_defaults()
            print('Production host and installed defaults ready', flush=True)
            connection = runtime.request('POST', '/api/connections', {'name': 'Agent fixture', 'kind': 'llamacpp', 'base_url': model.url, 'model': 'agent-fixture', 'context_limit': 32768}, expected=201)
            connection['tool_selection'] = {'selected': ['code_intelligence']}
            runtime.request('PUT', '/api/connections/'+str(connection['id']), connection)
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url+'/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value, 'domain': '127.0.0.1', 'path': '/', 'httpOnly': True}})
            host.pair_browser(browser)
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument', 'params': {'source': '''
                window.agentRunEvidence=[];
                const send=WebSocket.prototype.send;
                WebSocket.prototype.send=function(value) {
                    try { const data=JSON.parse(value);
                        if(data.type==='run_start') agentRunEvidence.push({type:data.type,session:data.session_id});
                    } catch {}
                    return send.call(this,value);
                };
            '''}})
            for mode in ('remote', 'local'):
                folder = project_root/mode
                folder.mkdir()
                files = {'main.ts': SOURCE, 'tsconfig.json': '{"compilerOptions":{"strict":true},"include":["*.ts"]}', 'package.json': '{}'}
                for path, text in files.items():
                    (folder/path).write_bytes(text.encode())
                subprocess.run(['git', 'init', '--quiet', '--initial-branch=main', '--template=', str(folder)], check=True)
                relative = folder.relative_to(ROOT.parent.parent).as_posix()
                project = runtime.request('POST', '/api/projects', {'name': 'Agent '+mode, 'mode': mode, 'path': relative if mode == 'remote' else folder.name}, expected=201)
                session = runtime.request('POST', '/api/sessions', {'name': 'Agent '+mode, 'project_id': project['id'], 'connection_id': connection['id'], 'auto_title': False}, expected=201)
                if mode == 'local':
                    local_handle(browser, project['id'], user, folder.name, files)
                    mirror.install(browser, project['id'], folder)
                text = lambda value: base64.b64encode(value.encode()).decode()
                runtime.request('PUT', f"/api/projects/{project['id']}/editor-recovery", {'revision': 0, 'state': {'format': 1, 'root': {'mode': mode, 'path': relative if mode == 'remote' else None}, 'selected': 'main.ts', 'files': [{'path': 'main.ts', 'document': {'text': text(DRAFT), 'saved': text(SOURCE), 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []}, 'scroll': {'top': 0, 'left': 0}, 'read_only': False}]}})
                for name, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id']), 'last_session_'+str(project['id']): str(session['id'])}.items():
                    runtime.request('PUT', '/api/settings', {'key': name, 'value': value})
                browser.call('POST', '/url', {'url': runtime.url+'/'})
                wait(mode+' editor draft', lambda: script(browser, "return document.querySelector('textarea[data-editor-path]')?.value===arguments[0];", DRAFT.replace("\r\n", "\n")))
                print(mode+': restored separate editor draft', flush=True)
                for disabled in (False, True):
                    case = mode + ('-disabled' if disabled else '-agent')
                    if disabled:
                        bindings = runtime.request('GET', f"/api/projects/{project['id']}/plugins")
                        binding = next(binding for binding in bindings if any(language['id'] == 'typescript' for language in binding['prepared']['manifest']['contributions'].get('languages', [])))
                        runtime.request('POST', f"/api/projects/{project['id']}/plugins", {'action': 'disable', 'id': binding['id'], 'revision': binding['revision']})
                    wait(case+' composer', lambda: script(browser, "const input=document.querySelector('.composer-input');if(!input||input.disabled||!input.getBoundingClientRect().height)return false;input.focus();return document.activeElement===input;"))
                    browser.call('POST', '/goog/cdp/execute', {'cmd': 'Input.insertText', 'params': {'text': 'AGENT_PROBE_CASE='+case}})
                    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'agent-submit', 'actions': [{'type': 'keyDown', 'value': '\ue007'}, {'type': 'keyUp', 'value': '\ue007'}]}]})
                    wait(case+' complete agent run', lambda: script(browser, "return document.body.textContent.includes(arguments[0]);", 'AGENT_PROBE_DONE '+case), seconds=120)
                    model.verify(case, disabled)
                    assert script(browser, "return document.querySelector('textarea[data-editor-path]')?.value===arguments[0];", DRAFT.replace("\r\n", "\n")), case+': agent changed editor draft'
                    recovery = runtime.request('GET', f"/api/projects/{project['id']}/editor-recovery")
                    draft = next(file for file in recovery['state']['files'] if file['path'] == 'main.ts')
                    assert base64.b64decode(draft['document']['text']).decode() == DRAFT, case+': draft recovery changed'
                starts = script(browser, 'return window.agentRunEvidence;')
                if mode == 'local':
                    assert not starts, 'Local project bypassed its browser agent executor'
                if mode == 'remote':
                    # New project keeps this fallback independent of disablement.
                    fallback_folder = project_root/'spin'
                    fallback_folder.mkdir()
                    for path, value in files.items():
                        (fallback_folder/path).write_bytes(value.encode())
                    fallback_path = fallback_folder.relative_to(ROOT.parent.parent).as_posix()
                    fallback = runtime.request('POST', '/api/projects', {'name': 'Spin agent', 'mode': 'remote', 'path': fallback_path}, expected=201)
                    fallback_session = runtime.request('POST', '/api/sessions', {'name': 'Spin agent', 'project_id': fallback['id'], 'connection_id': connection['id'], 'auto_title': False}, expected=201)
                    native_session = runtime.request('POST', '/api/sessions', {'name': 'Native agent', 'project_id': fallback['id'], 'connection_id': connection['id'], 'auto_title': False}, expected=201)
                    token = runtime.request('POST', '/api/bridge/token', {})['token']
                    browser.call('POST', '/timeouts', {'script': 180000})
                    native = browser.call('POST', '/execute/async', {'script': '''
                        const [url,token,session,done]=arguments;
                        const socket=new WebSocket(url);
                        let finished=false;
                        const finish=value=>{if(finished)return;finished=true;socket.close();done(value);};
                        socket.onopen=()=>socket.send(JSON.stringify({type:'hello',token}));
                        socket.onerror=()=>finish({error:'Native agent WebSocket failed'});
                        socket.onclose=()=>finish({error:'Native agent socket closed before completion'});
                        socket.onmessage=event=>{
                            const data=JSON.parse(event.data);
                            if(data.type==='hello_ok') {
                                if(!data.runs){finish({error:'Native server did not advertise agent runs'});return;}
                                socket.send(JSON.stringify({type:'run_start',run_id:'agent-language-probe',session_id:session,content:'AGENT_PROBE_CASE=native-agent',model:null,editor_context:null}));
                            }
                            if(data.type==='hello_error'||data.type==='run_rejected') finish({error:data.message});
                            if(data.type==='run_event') {
                                if(['error','cancelled','permission_request'].includes(data.event.kind)) finish({error:data.event.kind});
                                if(data.event.kind==='done') finish({complete:data.event.message.content==='AGENT_PROBE_DONE native-agent'});
                            }
                        };
                        setTimeout(()=>finish({error:'Native agent run timed out'}),150000);
                    ''', 'args': [host.url.replace('http:', 'ws:', 1), token, native_session['id']]}, timeout=180)
                    assert native == {'complete': True}, 'Native bridge agent failed: '+str(native)
                    model.verify('native-agent')
                    request = urllib.request.Request(runtime.url+f"/api/sessions/{fallback_session['id']}/messages", data=json.dumps({'content': 'AGENT_PROBE_CASE=spin-agent'}).encode(), headers={'x-openwebide': '1', 'Content-Type': 'application/json'}, method='POST')
                    with runtime.client.open(request, timeout=180) as response:
                        assert b'AGENT_PROBE_DONE spin-agent' in response.read(), 'Spin fallback did not finish'
                    model.verify('spin-agent')
                    assert all((fallback_folder/path).read_bytes() == value.encode() for path, value in files.items()), 'Spin agent changed workspace files'
                assert all((folder/path).read_bytes() == value.encode() for path, value in files.items()), mode+': read-only agent changed workspace files'
                print(mode+': actual agent queries, separate editor drafts and disabled provider verified', flush=True)
            print('Native bridge and Spin fallback agent queries verified', flush=True)
        except BaseException:
            with model.lock:
                print(json.dumps({'model_tool_results': model.results}), flush=True)
            for name, log in [('runtime', runtime.state/'runtime.log'), ('bridge', host.state/'bridge.log')]:
                if log.exists():
                    Path('/tmp/openwebide-agent-client-'+name+'.log').write_bytes(log.read_bytes())
            if browser:
                try:
                    print(json.dumps(script(browser, "return {editors:[...document.querySelectorAll('textarea[data-editor-path]')].map(node=>({path:node.dataset.editorPath,value:node.value})),tail:document.body.textContent.slice(-3500),alerts:[...document.querySelectorAll('[role=alert]')].map(node=>node.textContent.slice(0,1000)),dialogs:[...document.querySelectorAll('[role=dialog]')].map(node=>node.textContent.slice(0,1000))};")), flush=True)
                except Exception:
                    pass
            raise
        finally:
            if browser:
                browser.stop()
            mirror.stop()
            runtime.stop()
            host.stop()
            model.close()


if __name__ == '__main__':
    main()
