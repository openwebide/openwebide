#!/usr/bin/env python3
"""Deterministic LSP peer for the production workspace-edit browser contract.

The published HTML plugin selects this executable through the probe's private
PATH. All authorization, LSP normalization and filesystem changes remain in the
actual app. This peer never changes project files.
"""
import json
import os
from pathlib import Path
import sys
import time


def send(value):
    body = json.dumps({'jsonrpc': '2.0', **value}, ensure_ascii=False).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode() + body)
    sys.stdout.buffer.flush()


def record(value):
    with Path(os.environ['OPENWEBIDE_RESOURCE_PROBE_LOG']).open('a') as stream:
        stream.write(json.dumps(value) + '\n')


def position(text):
    rows = text.split('\n')
    return {'line': len(rows) - 1, 'character': len(rows[-1].encode('utf-16-le')) // 2}


def text_change(uri, source, replacement, version=None):
    return {'textDocument': {'uri': uri, 'version': version}, 'edits': [
        {'range': {'start': {'line': 0, 'character': 0}, 'end': position(source)}, 'newText': replacement}
    ]}


documents = {}
root = ''
while True:
    headers = {}
    while line := sys.stdin.buffer.readline():
        if line in (b'\r\n', b'\n'):
            break
        key, value = line.decode().split(':', 1)
        headers[key.lower()] = value.strip()
    if not line:
        break
    message = json.loads(sys.stdin.buffer.read(int(headers['content-length'])))
    method, params = message.get('method'), message.get('params', {})
    if method == 'exit':
        break
    if method == 'initialize':
        root = params['rootUri'].rstrip('/') + '/'
        record({'method': method, 'capabilities': params['capabilities']})
        workspace = params['capabilities']['workspace']
        assert workspace['applyEdit'] is False
        assert workspace['workspaceEdit']['documentChanges'] is True
        assert set(workspace['workspaceEdit']['resourceOperations']) == {'create', 'rename', 'delete'}
        result = {'capabilities': {'positionEncoding': 'utf-16', 'textDocumentSync': 1,
                                  'codeActionProvider': True, 'documentSymbolProvider': True,
                                  'executeCommandProvider': {'commands': ['fixture.execute']}}}
    elif method == 'initialized':
        send({'id': 'unsolicited', 'method': 'workspace/applyEdit', 'params': {'edit': {
            'documentChanges': [{'kind': 'create', 'uri': root + 'unapproved.txt'}]}}})
        continue
    elif method in ('textDocument/didOpen', 'textDocument/didChange'):
        document = params['textDocument']
        text = document['text'] if method.endswith('didOpen') else params['contentChanges'][0]['text']
        documents[document['uri']] = (text, document['version'])
        send({'method': 'textDocument/publishDiagnostics', 'params': {
            'uri': document['uri'], 'version': document['version'], 'diagnostics': []}})
        continue
    elif method == 'textDocument/didClose':
        documents.pop(params['textDocument']['uri'], None)
        continue
    elif method == 'textDocument/documentSymbol':
        result = [{'name': 'Fixture element', 'kind': 12,
                   'range': {'start': position(''), 'end': position('')},
                   'selectionRange': {'start': position(''), 'end': position('')}}]
    elif method == 'textDocument/codeAction':
        uri = params['textDocument']['uri']
        source, version = documents[uri]
        stage = '<section>stage 文😀</section>\r\n'
        final = '<section>final 文😀</section>\r\n'
        changes = [
            text_change(uri, source, stage, version),
            {'kind': 'rename', 'oldUri': uri, 'newUri': root + 'moved.html'},
            text_change(root + 'moved.html', stage, final),
            {'kind': 'create', 'uri': root + 'created.html'},
            text_change(root + 'created.html', '', '<p>new draft</p>\r\n'),
            {'kind': 'delete', 'uri': root + 'assets/gone.txt'},
        ]
        result = [
            {'title': 'Move HTML files with drafts', 'kind': 'refactor.rewrite', 'edit': {'documentChanges': changes}},
            {'title': 'Create interrupted file', 'kind': 'refactor.rewrite', 'edit': {
                'documentChanges': [{'kind': 'create', 'uri': root + 'recover.txt'}]}},
            {'title': 'Reject command with edits', 'kind': 'refactor.rewrite', 'edit': {
                'documentChanges': [{'kind': 'create', 'uri': root + 'unapproved.txt'}]},
             'command': {'title': 'Unapproved command', 'command': 'fixture.write'}},
        ]
        for title, mode, edited in [
            ('Run reviewed command only', 'success', False),
            ('Run reviewed edit and command', 'success', True),
            ('Run command with lost dispatch', 'lost', True),
            ('Run command with delayed outcome', 'block', True),
            ('Run command with uncertain outcome', 'error', True),
            ('Create interrupted command file', 'resource', False),
        ]:
            expected = source + f'\r\n<!-- reviewed {mode} -->\r\n' if edited else source
            action = {'title': title, 'command': {'title': title, 'command': 'fixture.execute',
                'arguments': [{'mode': mode, 'uri': uri, 'expected': expected}]}}
            if edited:
                action['edit'] = {'documentChanges': [text_change(uri, source, expected, version)]}
            if mode == 'resource':
                action['edit'] = {'documentChanges': [{'kind': 'create', 'uri': root + 'command-recover.html'}]}
            result.append(action)
        record({'method': method, 'uri': uri, 'source': source})
    elif method == 'workspace/executeCommand':
        assert params['command'] == 'fixture.execute'
        argument, = params['arguments']
        assert documents[argument['uri']][0] == argument['expected'], 'Command preceded its complete edit'
        record({'method': method, 'root': root, 'mode': argument['mode']})
        if argument['mode'] == 'block':
            time.sleep(60)
        if argument['mode'] == 'error':
            send({'id': message['id'], 'error': {'code': -32801, 'message': 'Command effects occurred before this error'}})
            continue
        result = {'executed': True}
    elif method == 'shutdown':
        result = None
    elif method is None:
        record({'response': message})
        continue
    else:
        result = None
    if 'id' in message:
        send({'id': message['id'], 'result': result})
