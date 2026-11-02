"""Collect all heaps in the fresh Chrome owned by the editor measurement."""
import base64
import hashlib
import json
import os
from pathlib import Path
import socket
import struct


class ScopedScriptResults:
    """Keep repeated WebDriver results in releasable object groups.

    ChromeDriver omits its result group when a script has no element arguments.
    An owned root element activates that group; the wrapper removes it before
    running the original script. Use only after navigation in an owned browser.
    """
    def __init__(self, browser):
        self.browser = browser
        self.anchor = browser.call("POST", "/element", {"using": "css selector", "value": "html"})

    def __getattr__(self, name):
        return getattr(self.browser, name)

    def call(self, method, path, body=None, **kwargs):
        if method == "POST" and path == "/execute/sync":
            body = dict(body)
            body["args"] = list(body.get("args", [])) + [self.anchor]
            body["script"] = (
                "const values = Array.from(arguments); values.pop();"
                "return (function(){" + body["script"] + "\n}).apply(null, values);"
            )
        return self.browser.call(method, path, body, **kwargs)

    def script(self, source):
        return self.call("POST", "/execute/sync", {"script": source, "args": []})


class Cdp:
    def __init__(self, directory):
        port, path = (Path(directory) / 'chrome/DevToolsActivePort').read_text().splitlines()[:2]
        self.socket = socket.create_connection(('127.0.0.1', int(port)), timeout=30)
        self.buffer = b''
        self.sequence = 0
        try:
            key = base64.b64encode(os.urandom(16)).decode()
            self.socket.sendall((f'GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n').encode())
            while b'\r\n\r\n' not in self.buffer:
                chunk = self.socket.recv(4096)
                assert chunk, "Chrome closed its debugger handshake"
                self.buffer += chunk
                assert len(self.buffer) <= 65536, "Oversized debugger handshake"
            header, self.buffer = self.buffer.split(b'\r\n\r\n', 1)
            assert header.startswith(b'HTTP/1.1 101'), header[:100]
            accept = base64.b64encode(hashlib.sha1((key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
            fields = {name.lower(): value.strip() for name, value in (line.split(b":", 1) for line in header.split(b"\r\n")[1:])}
            assert fields.get(b"sec-websocket-accept") == accept
        except BaseException:
            self.socket.close()
            raise

    def read(self, count):
        while len(self.buffer) < count:
            chunk = self.socket.recv(max(4096, count - len(self.buffer)))
            assert chunk, 'CDP websocket closed'
            self.buffer += chunk
        value, self.buffer = self.buffer[:count], self.buffer[count:]
        return value

    def send(self, payload, opcode=1):
        mask = os.urandom(4)
        count = len(payload)
        size = bytes([0x80 | count]) if count < 126 else (b'\xfe' + struct.pack('!H', count) if count < 65536 else b'\xff' + struct.pack('!Q', count))
        self.socket.sendall(bytes([0x80 | opcode]) + size + mask + bytes(value ^ mask[index % 4] for index, value in enumerate(payload)))

    def receive(self):
        fragments = []
        message_size = 0
        while True:
            first, second = self.read(2)
            opcode = first & 15
            size = second & 127
            if size == 126:
                size = struct.unpack('!H', self.read(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.read(8))[0]
            assert size <= 64 * 1024 * 1024
            mask = self.read(4) if second & 128 else None
            payload = self.read(size)
            if mask:
                payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
            if opcode == 9:
                self.send(payload, 10)
                continue
            if opcode == 10:
                continue
            assert opcode in (0, 1), 'CDP websocket ended or sent non-JSON data'
            message_size += len(payload)
            assert message_size <= 64 * 1024 * 1024
            fragments.append(payload)
            if first & 128:
                return json.loads(b''.join(fragments))

    def call(self, method, params=None, session_id=None):
        self.sequence += 1
        request = {'id': self.sequence, 'method': method, 'params': params or {}}
        if session_id:
            request['sessionId'] = session_id
        self.send(json.dumps(request).encode())
        while True:
            message = self.receive()
            if message.get('id') == self.sequence:
                assert 'error' not in message, message.get('error')
                return message.get('result', {})


def collect_garbage(browser):
    client = Cdp(Path(browser.log.name).parent)
    try:
        targets = client.call('Target.getTargets')['targetInfos']
        heaps = []
        for target in targets:
            if target['type'] not in ('page', 'worker', 'service_worker', 'shared_worker', 'iframe'):
                continue
            session = client.call('Target.attachToTarget', {'targetId': target['targetId'], 'flatten': True})['sessionId']
            try:
                client.call('HeapProfiler.collectGarbage', session_id=session)
                heaps.append({'targetType': target['type'], 'heap': client.call('Runtime.getHeapUsage', session_id=session)})
            finally:
                client.call('Target.detachFromTarget', {'sessionId': session})
        assert any(heap['targetType'] == 'page' for heap in heaps)
        return heaps
    finally:
        client.socket.close()
