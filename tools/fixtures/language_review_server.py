#!/usr/bin/env python3
"""Protocol-only peer for retained reviews with an independently refreshed provider.

The control file is outside every observed project. No peer writes a workspace.
The owning contract exercises the real installed app, authority and workers.
"""
import json
import os
from pathlib import Path
import sys
from threading import Event, Lock, Thread

ROLE, CONTROL, LOG = sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3])
assert ROLE in ('language', 'diagnostics')
output_lock, state_lock, log_lock = Lock(), Lock(), Lock()
finished = Event()
documents, root = {}, ''
generation = 'initial'


def record(value):
    with log_lock, LOG.open('a') as stream:
        stream.write(json.dumps({'role': ROLE, 'pid': os.getpid(), **value}) + '\n')


def send(value):
    body = json.dumps({'jsonrpc': '2.0', **value}, ensure_ascii=False).encode()
    with output_lock:
        sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
        sys.stdout.buffer.flush()


def diagnostic(uri, version, nonce):
    prefix = documents[uri][0].split('\n', 1)[0].rstrip('\r')
    boundaries = [0]
    for character in prefix[:2]:
        boundaries.append(boundaries[-1] + len(character.encode('utf-16-le')) // 2)
    diagnostics = [] if ROLE == 'language' else [{
        'range': {'start': {'line': 0, 'character': boundaries[index]},
                  'end': {'line': 0, 'character': boundaries[index + 1]}},
        'severity': 2, 'source': ROLE,
        'message': 'Independent note ' + nonce + ('' if index == 0 else ' second')}
        for index in range(min(len(boundaries) - 1, 1 if nonce == 'initial' else 2))]
    send({'method': 'textDocument/publishDiagnostics', 'params': {
        'uri': uri, 'version': version, 'diagnostics': diagnostics}})


def refresh():
    global generation
    while not finished.wait(.05):
        try:
            control = json.loads(CONTROL.read_text())
        except (FileNotFoundError, json.JSONDecodeError):
            continue
        with state_lock:
            if control.get('root') != root or control.get('nonce') == generation:
                continue
            nonce = control.get('nonce')
            assert isinstance(nonce, str) and 0 < len(nonce) <= 64
            generation = nonce
            # Serialize against didChange so a refresh cannot publish an older
            # version after acknowledging a newer overlay.
            versions = {uri: document[1] for uri, document in documents.items()}
            for uri, version in versions.items():
                diagnostic(uri, version, nonce)
            record({'event': 'refresh_published', 'root': root, 'nonce': nonce,
                    'versions': versions})


def end(text):
    rows = text.split('\n')
    return {'line': len(rows) - 1, 'character': len(rows[-1].encode('utf-16-le')) // 2}


worker = Thread(target=refresh, daemon=True) if ROLE == 'diagnostics' else None
if worker:
    worker.start()
try:
    while True:
        headers = {}
        while line := sys.stdin.buffer.readline():
            if line in (b'\r\n', b'\n'):
                break
            name, value = line.decode().split(':', 1)
            headers[name.lower()] = value.strip()
        if not line:
            break
        message = json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
        method, params = message.get('method'), message.get('params', {})
        record({'method': method, 'root': root, 'params': params})
        result = None
        if method == 'exit':
            break
        if method == 'initialize':
            with state_lock:
                root = params['rootUri'].rstrip('/') + '/'
            assert params['capabilities']['workspace']['applyEdit'] is False
            result = {'capabilities': {'positionEncoding': 'utf-16', 'textDocumentSync': 1,
                      'codeActionProvider': True, 'documentSymbolProvider': True,
                      'executeCommandProvider': {'commands': ['fixture.execute']}}}
        elif method in ('textDocument/didOpen', 'textDocument/didChange'):
            document = params['textDocument']
            text = document['text'] if method.endswith('didOpen') else params['contentChanges'][0]['text']
            with state_lock:
                documents[document['uri']] = (text, document['version'])
                diagnostic(document['uri'], document['version'], generation)
        elif method == 'textDocument/didClose':
            with state_lock:
                documents.pop(params['textDocument']['uri'], None)
        elif method == 'textDocument/documentSymbol':
            result = []
        elif method == 'textDocument/codeAction':
            uri = params['textDocument']['uri']
            with state_lock:
                text, version = documents[uri]
            edit = {'documentChanges': [{
                'textDocument': {'uri': uri, 'version': version},
                'edits': [{'range': {'start': {'line': 0, 'character': 0}, 'end': end(text)},
                           'newText': 'Reviewed 文😀\r\n'}]},
                {'kind': 'create', 'uri': root + 'created.notes'}]}
            result = [
                {'title': 'Create file with reviewed draft', 'kind': 'refactor.rewrite', 'edit': edit},
                {'title': 'Create file and run reviewed command', 'kind': 'refactor.rewrite', 'edit': edit,
                 'command': {'title': 'Reviewed command', 'command': 'fixture.execute',
                             'arguments': [{'uri': uri, 'expected': 'Reviewed 文😀\r\n'}]}}]
        elif method == 'workspace/executeCommand':
            assert params['command'] == 'fixture.execute'
            argument, = params['arguments']
            with state_lock:
                assert documents[argument['uri']][0] == argument['expected']
            record({'event': 'command_executed', 'root': root})
            result = {'executed': True}
        if 'id' in message:
            send({'id': message['id'], 'result': result})
finally:
    finished.set()
    if worker:
        worker.join(timeout=1)
