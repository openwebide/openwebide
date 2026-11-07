#!/usr/bin/env python3
"""Verify packaged C/C++ analysis and unsaved headers offline without root."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

with tempfile.TemporaryDirectory(prefix='openwebide-clangd-') as directory:
    root = Path(directory).resolve()
    version = subprocess.check_output(['clangd', '--version'], text=True, timeout=10)
    assert 'clangd version 23.1.6' in version, version
    (root / 'include').mkdir()
    header = '#pragma once\nstatic inline int answer(void) { return BUILD_VALUE; }\n'
    c = '#include "api.h"\n#include <stddef.h>\nlong c_value(void) { return answer(); }\n'
    cpp = '#include "api.h"\n#include <vector>\nstd::vector<int> values;\nauto value = answer();\n'
    files = {'include/api.h': header, 'main.c': c, 'main.cpp': cpp}
    for path, text in files.items():
        (root / path).write_text(text)
    commands = [{'directory': str(root), 'file': name, 'arguments': [compiler, standard, '-Iinclude', '-DBUILD_VALUE=42', '-c', name]}
                for name, compiler, standard in [('main.c', 'cc', '-std=c17'), ('main.cpp', 'c++', '-std=c++20')]]
    (root / 'compile_commands.json').write_text(json.dumps(commands))
    for name in ['main.c', 'main.cpp']:
        result = subprocess.run(['clangd', '--check=' + name, '--log=error'], cwd=root, capture_output=True, text=True, timeout=90)
        assert result.returncode == 0, result.stdout + result.stderr
    broken = root / 'broken.cpp'
    broken.write_text('int invalid = "bad";\n')
    result = subprocess.run(['clangd', '--check=broken.cpp', '--log=error'], cwd=root, capture_output=True, text=True, timeout=30)
    assert result.returncode != 0 and 'cannot initialize' in result.stderr, result.stderr
    with subprocess.Popen(['clangd', '--background-index', '--use-dirty-headers', '--pch-storage=memory', '--log=error'], cwd=root,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as server:
        timer = threading.Timer(90, server.kill)
        timer.start()
        try:
            def send(message):
                body = json.dumps(message).encode()
                server.stdin.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
                server.stdin.flush()

            def response(identifier):
                while True:
                    headers = {}
                    while (line := server.stdout.readline()) != b'\r\n':
                        assert line, 'clangd exited before responding'
                        key, value = line.decode().split(':', 1)
                        headers[key.lower()] = value.strip()
                    message = json.loads(server.stdout.read(int(headers['content-length'])))
                    if 'method' in message and 'id' in message:
                        send({'jsonrpc': '2.0', 'id': message['id'], 'result': None})
                    elif message.get('id') == identifier:
                        assert 'result' in message, message
                        return message['result']

            send({'jsonrpc': '2.0', 'id': 1, 'method': 'initialize', 'params': {
                'processId': None, 'rootUri': root.as_uri(), 'capabilities': {},
                'workspaceFolders': [{'uri': root.as_uri(), 'name': 'Smoke'}]}})
            initialized = response(1)
            assert all(initialized['capabilities'][key] for key in ['hoverProvider', 'renameProvider', 'documentFormattingProvider'])
            send({'jsonrpc': '2.0', 'method': 'initialized', 'params': {}})
            for path, text, language in [('include/api.h', header.replace('inline int', 'inline long'), 'cpp'), ('main.c', c, 'c'), ('main.cpp', cpp, 'cpp')]:
                send({'jsonrpc': '2.0', 'method': 'textDocument/didOpen', 'params': {
                    'textDocument': {'uri': (root / path).as_uri(), 'languageId': language, 'version': 1, 'text': text}}})
            for identifier, path, line, character in [(2, 'main.c', 2, 29), (3, 'main.cpp', 3, 16)]:
                send({'jsonrpc': '2.0', 'id': identifier, 'method': 'textDocument/hover', 'params': {
                    'textDocument': {'uri': (root / path).as_uri()}, 'position': {'line': line, 'character': character}}})
                hover = response(identifier)
                assert 'long' in json.dumps(hover), hover
            send({'jsonrpc': '2.0', 'id': 4, 'method': 'shutdown', 'params': None})
            response(4)
            send({'jsonrpc': '2.0', 'method': 'exit'})
            assert server.wait(timeout=10) == 0
        finally:
            timer.cancel()
            if server.poll() is None:
                server.kill()
                server.wait(timeout=10)
    for path, text in files.items():
        assert (root / path).read_text() == text
    print(json.dumps({'offline': True, 'unprivileged_uid': os.getuid(), 'clangd': '23.1.6',
                      'compilation_database_flags': True, 'c_cpp_standard_library_headers': True,
                      'type_diagnostics': True, 'real_lsp_unsaved_header_hover_both_languages': True,
                      'source_files_unchanged': True}))
