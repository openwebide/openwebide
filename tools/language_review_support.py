"""Owned transport latch before the real initial native worker admission.

It forwards the original authenticated/delegated request and response unchanged.
No worker, validation result or journal acknowledgement is manufactured.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from threading import Event, Lock, Thread
import time
import urllib.error
import urllib.request

from language_server_client_support import script


class AdmissionProxy:
    def __init__(self, runtime):
        self.runtime, self.secret, self.armed = runtime, None, False
        self.lock, self.paused, self.release, self.replied = Lock(), Event(), Event(), Event()
        self.release.set()
        self.rows = []
        proxy = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def relay(self):
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                data = json.loads(body) if body else {}
                operation = data.get('operation', {})
                remote = (proxy.secret is not None
                          and self.headers.get('Authorization') == 'Bearer ' + proxy.secret
                          and self.path == '/api/workspace-transactions/service'
                          and operation.get('command') == 'claim_worker')
                paired = (self.path == '/api/workspace-transactions/delegated'
                          and isinstance(data.get('delegation'), dict)
                          and isinstance(data.get('worker'), dict)
                          and operation.get('command') == 'admit')
                with proxy.lock:
                    pause = proxy.armed and (remote or paired)
                    if pause:
                        proxy.armed = False
                        scope = operation['scope'] if remote else data['delegation']['scope']
                        row = {'mode': 'remote' if remote else 'local', 'id': scope['id'],
                               'generation': scope['generation'], 'operation': operation['command'],
                               'original_review_digest': operation.get('review_digest'),
                               'has_manual_grant': isinstance(operation.get('language_grant'), str)
                                   and len(operation['language_grant']) == 32,
                               'original_scope': scope, 'paused_monotonic': time.monotonic(),
                               'forwarded': False}
                        proxy.rows.append(row)
                if pause:
                    proxy.paused.set()
                    released = proxy.release.wait(45)
                else:
                    released = True
                if not released:
                    status, response, content_type = 503, b'{"error":"Owned admission latch expired"}', 'application/json'
                    row['latch_expired'] = True
                else:
                    headers = {name: value for name, value in self.headers.items()
                               if name.lower() not in ('host', 'content-length', 'connection')}
                    request = urllib.request.Request(runtime.url + self.path, data=body or None,
                                                     headers=headers, method=self.command)
                    if pause:
                        row['forwarded'] = True
                        row['forwarded_monotonic'] = time.monotonic()
                    try:
                        reply = urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=30)
                    except urllib.error.HTTPError as error:
                        reply = error
                    except urllib.error.URLError:
                        reply = None
                    if reply is None:
                        status, response, content_type = 503, b'{"error":"Owned admission relay unavailable"}', 'application/json'
                    else:
                        with reply:
                            status, response, content_type = reply.code, reply.read(), reply.headers.get('Content-Type', 'application/json')
                if pause:
                    row['status'], row['reply_monotonic'] = status, time.monotonic()
                    try:
                        decoded = json.loads(response)
                        row['error'] = decoded.get('error')
                        row['result'] = decoded.get('result')
                    except (json.JSONDecodeError, AttributeError):
                        row['invalid_json'] = True
                    proxy.replied.set()
                try:
                    self.send_response(status)
                    self.send_header('Content-Type', content_type)
                    self.send_header('Content-Length', str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            do_GET = do_POST = do_PUT = do_DELETE = relay

        self.server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.daemon_threads = True
        self.url = f'http://127.0.0.1:{self.server.server_port}/api'
        self.thread = Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def arm(self):
        with self.lock:
            assert not self.armed
            self.paused.clear()
            self.replied.clear()
            self.release.clear()
            self.armed = True

    def stop(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def assert_review_session(browser, mode, checkpoint, has_command=False):
    timeline = script(browser, 'return resourceProbe.timeline.slice(arguments[0]);', checkpoint)
    language = [row for row in timeline if row['kind'] == 'language'
                and row['query']['operation'] not in ('synchronize', 'prepare_action', 'synchronize_action')]
    assert [row['query']['operation'] for row in language] == ['code_actions', 'review_action', 'validate_analysis'], language
    assert all(row.get('status') == 200 for row in language), language
    command_queries = [row for row in timeline if row['kind'] == 'language'
                       and row['query']['operation'] in ('prepare_action', 'synchronize_action')]
    if has_command:
        prepared = [row for row in command_queries if row['query']['operation'] == 'prepare_action']
        synchronized = [row for row in command_queries if row['query']['operation'] == 'synchronize_action']
        assert len(prepared) == 1 and synchronized, command_queries
        assert language[-1]['completed'] < prepared[0]['began'], timeline
        assert all(prepared[0]['completed'] < row['began'] for row in synchronized), timeline
        assert all(row.get('status') == 200 and row.get('server') == language[0]['server']
                   for row in command_queries), command_queries
    else:
        assert not command_queries, command_queries

    assert language[0]['server'] == language[1]['server'] == language[2]['server'], language
    begins = [row for row in timeline if row['kind'] == 'transaction' and row['operation'] == 'begin']
    assert len(begins) == 1 and begins[0].get('status') == 200, begins
    begin = begins[0]
    review = begin['body']['language_review']
    assert review['input'] == language[1]['input'], review
    assert review['context'] == language[1]['analysisContext'], review
    assert review['result'] == language[1]['analysisResult'], review
    assert review['binding_revision'] > 0 and len(review['transaction_digest']) == 64
    context = review['context']
    assert context['files'][0] == '' and context['identities'][0].startswith('u1.')
    assert 0 < len(context['files']) == len(context['identities']) == len(context['revisions'])
    assert all(isinstance(value, str) and value for value in context['revisions'])
    assert language[-1]['completed'] < begin['began'], timeline
    folders = [row for row in timeline if row['kind'] == 'folder']
    executed = [row for row in timeline if row['kind'] in ('folder', 'transaction') and row.get('operation') == 'execute']
    assert len(executed) == 1 and executed[0]['hasGrant'], executed
    grants = [row for row in timeline if row['kind'] == 'grant' and row.get('status') == 200
              and begin['completed'] < row['began'] < row['completed'] < executed[0]['began']]
    assert any(row['context'].get('user_action') is True and row['context'].get('session_id') is None
               and row['context'].get('project_id') is not None
               and row['packages'] == [review['prepared']['digest']]
               and row['grantDigests'] == [review['prepared']['digest']] for row in grants), grants
    if mode == 'remote':
        assert not folders, folders
        return
    opened = [row for row in folders if row['operation'] == 'open']
    proved = [row for row in folders if row['operation'] == 'prove']
    assert len(opened) == len(proved) == 1, folders
    assert opened[0]['result'] == 'opened' and proved[0]['result'] == 'ready', folders
    assert opened[0]['completed'] < proved[0]['began'] < proved[0]['completed'] < language[0]['began'], timeline
    session = opened[0]['openedSession']
    # Dropping the preceding rejected review can finish its asynchronous close
    # after this checkpoint. It cannot contribute reads, proof or mutations.
    assert session and all(row.get('session') == session or row['operation'] in ('open', 'close') for row in folders), folders
    original = [row for row in folders if row.get('session') == session]
    assert {'file', 'execute', 'close'} <= {row['operation'] for row in original}, folders
    assert any(row.get('fileOperation') == 'bind' for row in original), folders
    executed = [row for row in original if row['operation'] == 'execute']
    assert len(executed) == 1 and language[-1]['completed'] < executed[0]['began'], timeline
    assert not any(row['operation'] in ('admit', 'advance') or row.get('fileOperation') in ('create', 'transfer', 'remove_created', 'remove_retained') for row in original), folders
    assert all(row.get('status') == 200 for row in [opened[0], *original]), folders
