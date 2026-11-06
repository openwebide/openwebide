#!/usr/bin/env python3
"""Verify shipped Python tools offline as an arbitrary unprivileged user."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading


with tempfile.TemporaryDirectory(prefix='openwebide-python-') as directory:
    root = Path(directory)
    subprocess.run(['python3', '-m', 'venv', '--without-pip', str(root / '.venv')], check=True, timeout=30)
    site = subprocess.check_output([
        str(root / '.venv/bin/python'), '-c', "import sysconfig; print(sysconfig.get_path('purelib'))"
    ], text=True, timeout=10).strip()
    (Path(site) / 'env_only.py').write_text("def answer() -> str:\n    return 'environment'\n")
    (root / 'pyproject.toml').write_text("[tool.pyright]\nvenvPath = '.'\nvenv = '.venv'\ntypeCheckingMode = 'standard'\n")
    source = root / 'main.py'
    source.write_text('from env_only import answer\nvalue: str = answer()\n')
    env = {**os.environ, 'PYRIGHT_TMPDIR': directory}

    def check(expected):
        result = subprocess.run(['pyright', '--outputjson', 'main.py'], cwd=root, env=env,
                                capture_output=True, text=True, timeout=45)
        data = json.loads(result.stdout)
        assert data['version'] == '1.1.414', data
        assert data['summary']['errorCount'] == expected, data
        assert result.returncode == (1 if expected else 0), result
        return data

    check(0)
    source.write_text('from env_only import answer\nvalue: int = answer()\n')
    broken = check(1)
    assert broken['generalDiagnostics'][0]['rule'] == 'reportAssignmentType', broken
    with subprocess.Popen(['pyright-langserver','--stdio'], cwd=root, env=env,
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as server:
        timer = threading.Timer(45, server.kill)
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
                        assert line, 'Language server exited before response'
                        key, value = line.decode().split(':', 1)
                        headers[key.lower()] = value.strip()
                    message = json.loads(server.stdout.read(int(headers['content-length'])))
                    if message.get('id') == request_id:
                        assert 'result' in message, message
                        return message['result']

            send({'jsonrpc':'2.0','id':1,'method':'initialize','params':{
                'processId':None,'rootUri':root.as_uri(),'capabilities':{},
                'workspaceFolders':[{'uri':root.as_uri(),'name':'Smoke'}]}})
            initialized = response(1)
            assert initialized['capabilities']['hoverProvider'], initialized
            assert initialized['capabilities']['renameProvider'], initialized
            send({'jsonrpc':'2.0','method':'initialized','params':{}})
            send({'jsonrpc':'2.0','id':2,'method':'shutdown','params':None})
            response(2)
            send({'jsonrpc':'2.0','method':'exit'})
            assert server.wait(timeout=10) == 0
        finally:
            timer.cancel()
            if server.poll() is None:
                server.kill()
                server.wait(timeout=10)
    print(json.dumps({'offline':True,'unprivileged_uid':os.getuid(),'pyright':'1.1.414',
                      'project_venv_imports':True,'type_diagnostics':True,'real_lsp_initialize_shutdown':True}))
