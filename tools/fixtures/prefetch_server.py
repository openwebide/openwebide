"""LSP peer that keeps reading while a speculative symbol request is pending."""
import json
from pathlib import Path
import sys
import threading
import time
from urllib.parse import unquote, urlparse

root = Path.cwd()
opened = {}
pending = set()
pending_lock = threading.Lock()
output_lock = threading.Lock()


def send(value):
    body = json.dumps({'jsonrpc': '2.0', **value}).encode()
    with output_lock:
        sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
        sys.stdout.buffer.flush()


def symbols():
    return [{'name': 'Item', 'kind': 13,
        'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}},
        'selectionRange': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 1}}}]


def release_when_marked(request):
    marker = root.parent / (root.name + '.release-symbols')
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        with pending_lock:
            if request not in pending:
                return
            if marker.exists():
                pending.remove(request)
                break
        time.sleep(.005)
    else:
        return
    send({'id': request, 'result': symbols()})


while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line == b'\r\n':
            break
        name, value = line.decode().split(':', 1)
        headers[name.lower()] = value.strip()
    message = json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method = message.get('method')
    params = message.get('params') or {}
    with (root / 'calls').open('a') as stream:
        stream.write(json.dumps({'method': method, 'params': params}) + '\n')
    result = None
    if method == 'initialize':
        result = {'capabilities': {'textDocumentSync': 1, 'hoverProvider': True,
            'documentSymbolProvider': True, 'diagnosticProvider': {'interFileDependencies': True, 'workspaceDiagnostics': False}}}
    elif method in ('textDocument/didOpen', 'textDocument/didChange'):
        document = params['textDocument']
        opened[document['uri']] = document.get('text', params.get('contentChanges', [{}])[0].get('text'))
    elif method == 'textDocument/didClose':
        opened.pop(params['textDocument']['uri'], None)
    elif method == 'textDocument/hover':
        result = {'contents': json.dumps({Path(unquote(urlparse(uri).path)).name: source for uri, source in opened.items()})}
    elif method == 'textDocument/documentSymbol':
        if (root / 'hold-symbols').exists():
            with pending_lock:
                pending.add(message['id'])
            (root / 'symbols-started').write_text('started')
            threading.Thread(target=release_when_marked, args=(message['id'],), daemon=True).start()
            continue
        result = symbols()
    elif method == 'textDocument/diagnostic':
        result = {'kind': 'full', 'items': []}
    elif method == '$/cancelRequest':
        with pending_lock:
            if not (root / 'ignore-cancel').exists():
                pending.discard(params['id'])
        (root / 'symbols-cancelled').write_text('cancelled')
    elif method == 'test/releaseSymbols':
        with pending_lock:
            completed = list(pending)
            pending.clear()
        for request in completed:
            send({'id': request, 'result': symbols()})
    if 'id' in message:
        send({'id': message['id'], 'result': result})
