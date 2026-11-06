#!/usr/bin/env python3
"""Verify packaged Go, workspace analysis and stdio LSP offline without root."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading


with tempfile.TemporaryDirectory(prefix='openwebide-go-') as directory:
    root = Path(directory).resolve()
    env = {**os.environ, 'GOENV': 'off', 'GOTOOLCHAIN': 'local', 'GOFLAGS': '',
           'GOWORK': str(root / 'go.work'), 'GOPROXY': 'off', 'GOSUMDB': 'off',
           'GOCACHE': str(root / 'cache'), 'GOMODCACHE': str(root / 'modules'),
           'GOPATH': str(root / 'path'), 'GOTELEMETRY': 'off'}
    assert subprocess.check_output(['go', 'version'], env=env, text=True, timeout=10).split()[2] == 'go1.27.3'
    assert subprocess.check_output(['gopls', 'version'], env=env, text=True, timeout=10).strip() == 'golang.org/x/tools/gopls v0.24.0'
    (root / 'lib').mkdir()
    (root / 'app').mkdir()
    (root / 'go.work').write_text('go 1.27.0\n\nuse (\n ./lib\n ./app\n)\n')
    (root / 'lib/go.mod').write_text('module example.test/lib\n\ngo 1.27.0\n')
    (root / 'app/go.mod').write_text('module example.test/app\n\ngo 1.27.0\n\nrequire example.test/lib v0.0.0\n')
    library = 'package lib\n\nfunc Answer() int { return 42 }\n'
    source = 'package app\n\nimport "example.test/lib"\nimport "fmt"\n\nvar Value = lib.Answer()\nvar Text = fmt.Sprint(Value)\n'
    (root / 'lib/answer.go').write_text(library)
    main = root / 'app/main.go'
    main.write_text(source)

    def check(broken):
        result = subprocess.run(['gopls', 'check', 'app/main.go'], cwd=root, env=env,
                                capture_output=True, text=True, timeout=120)
        output = result.stdout + result.stderr
        assert result.returncode == 0, output
        assert ('cannot use' in output) is broken, output

    check(False)
    main.write_text(source.replace('var Value =', 'var Value string ='))
    check(True)
    main.write_text(source)
    unsaved = library.replace('int { return 42 }', 'string { return "unsaved" }')
    with subprocess.Popen(['gopls', 'serve'], cwd=root, env=env,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as server:
        timer = threading.Timer(90, server.kill)
        timer.start()
        try:
            def send(message):
                body = json.dumps(message).encode()
                server.stdin.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
                server.stdin.flush()

            def response(request_id):
                while True:
                    headers = {}
                    while (line := server.stdout.readline()) != b'\r\n':
                        assert line, 'gopls exited before its response'
                        key, value = line.decode().split(':', 1)
                        headers[key.lower()] = value.strip()
                    message = json.loads(server.stdout.read(int(headers['content-length'])))
                    if 'method' in message and 'id' in message:
                        method = message['method']
                        if method == 'workspace/configuration':
                            result = [{} for item in message['params']['items']]
                        elif method == 'workspace/workspaceFolders':
                            result = [{'uri': root.as_uri(), 'name': 'Smoke'}]
                        else:
                            result = None
                        send({'jsonrpc': '2.0', 'id': message['id'], 'result': result})
                    elif message.get('id') == request_id:
                        assert 'result' in message, message
                        return message['result']

            send({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
                'processId': None, 'rootUri': root.as_uri(), 'capabilities': {},
                'workspaceFolders': [{'uri': root.as_uri(), 'name': 'Smoke'}]}})
            initialized = response(1)
            for capability in ['hoverProvider', 'renameProvider', 'documentFormattingProvider']:
                assert initialized['capabilities'][capability], initialized
            send({'jsonrpc': '2.0', 'method': 'initialized', 'params': {}})
            for path, text in [('lib/answer.go', unsaved), ('app/main.go', source)]:
                send({'jsonrpc': '2.0', 'method': 'textDocument/didOpen', 'params': {
                    'textDocument': {'uri': (root / path).as_uri(), 'languageId': 'go', 'version': 1, 'text': text}}})
            send({'jsonrpc': '2.0', 'id': 2, 'method': 'textDocument/hover', 'params': {
                'textDocument': {'uri': main.as_uri()}, 'position': {'line': 5, 'character': 17}}})
            hover = response(2)
            assert 'string' in json.dumps(hover), hover
            send({'jsonrpc': '2.0', 'id': 3, 'method': 'shutdown', 'params': None})
            response(3)
            send({'jsonrpc': '2.0', 'method': 'exit'})
            assert server.wait(timeout=10) == 0
        finally:
            timer.cancel()
            if server.poll() is None:
                server.kill()
                server.wait(timeout=10)
    assert main.read_text() == source and (root / 'lib/answer.go').read_text() == library
    print(json.dumps({'offline': True, 'unprivileged_uid': os.getuid(), 'go': '1.27.3', 'gopls': 'v0.24.0',
                      'go_work_multiple_modules': True, 'standard_library_imports': True, 'type_diagnostics': True,
                      'real_lsp_unsaved_cross_module_hover': True, 'source_files_unchanged': True}))
