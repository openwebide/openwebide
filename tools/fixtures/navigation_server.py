#!/usr/bin/env python3
"""Deterministic public LSP peer for navigation/provider production checks.

This peer supplies protocol responses only. Installation, authority, transport,
normalization, UI navigation, editing and recovery run in the production app.
"""
import json
import os
from pathlib import Path
import sys

ROLE = sys.argv[1]
if ROLE == 'diagnostics' and Path(sys.argv[2]).exists():
    sys.exit('The probe diagnostics provider is deliberately unavailable')


def send(value):
    body = json.dumps({'jsonrpc': '2.0', **value}, ensure_ascii=False).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
    sys.stdout.buffer.flush()


def span(line, start=5, end=9):
    return {'start': {'line': line, 'character': start},
            'end': {'line': line, 'character': end}}


documents = {}
root = ''
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
    with Path(os.environ['OPENWEBIDE_NAVIGATION_PROBE_LOG']).open('a') as log:
        log.write(json.dumps({'role': ROLE, 'method': method, 'params': params}) + '\n')
    result = None
    if method == 'exit':
        break
    if method == 'initialize':
        root = params['rootUri'].rstrip('/') + '/'
        result = {'capabilities': {'positionEncoding': 'utf-16', 'textDocumentSync': 1,
                  'typeDefinitionProvider': True, 'implementationProvider': True,
                  'documentSymbolProvider': True, 'codeActionProvider': True,
                  'workspaceSymbolProvider': {'resolveProvider': True}}}
    elif method in ('textDocument/didOpen', 'textDocument/didChange'):
        document = params['textDocument']
        text = document['text'] if method.endswith('didOpen') else params['contentChanges'][0]['text']
        documents[document['uri']] = (text, document['version'])
        locations = [(2, 'Analyzer note')] if ROLE == 'diagnostics' else [(0, 'First problem'), (3, 'Last problem')]
        send({'method': 'textDocument/publishDiagnostics', 'params': {
            'uri': document['uri'], 'version': document['version'], 'diagnostics': [
                {'range': span(row, 4 if row == 0 else 5, 8 if row == 0 else 9),
                 'severity': 2, 'message': label, 'source': ROLE} for row, label in locations]}})
    elif method == 'textDocument/didClose':
        documents.pop(params['textDocument']['uri'], None)
    elif method in ('textDocument/typeDefinition', 'textDocument/implementation'):
        result = [{'uri': params['textDocument']['uri'], 'range': span(1 if method.endswith('typeDefinition') else 2)}]
    elif method == 'textDocument/documentSymbol':
        result = [{'name': name, 'kind': 5, 'range': span(row), 'selectionRange': span(row)}
                  for name, row in [('Type Item', 1), ('Implementation Item', 2)]]
    elif method == 'workspace/symbol':
        result = [{'name': 'Lazy Item', 'kind': 5, 'location': {'uri': root + 'main.notes'},
                   'data': {'origin': ROLE}}]
    elif method == 'workspaceSymbol/resolve':
        assert params['data']['origin'] == ROLE == 'language'
        result = {**params, 'location': {'uri': root + 'main.notes', 'range': span(3)}}
    elif method == 'textDocument/codeAction':
        assert ROLE == 'diagnostics', 'A diagnostic fix reached the wrong service'
        uri = params['textDocument']['uri']
        result = [{'title': 'Fix analyzer note', 'kind': 'quickfix', 'edit': {'documentChanges': [{
            'textDocument': {'uri': uri, 'version': documents[uri][1]},
            'edits': [{'range': span(2), 'newText': 'Fixed'}]}]}}]
    if 'id' in message:
        send({'id': message['id'], 'result': result})
