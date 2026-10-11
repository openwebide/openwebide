#!/usr/bin/env python3
"""Drive Chromium IME and keyboard input through the built Rust/WASM editor.

Uses disposable Spin/SQLite state and recovery drafts in both workspace modes.
Local cases do not grant a native folder handle. These are trusted browser-engine
input events, not proof of physical IME devices, touch handles or installed PWAs.
Run after spin build with CHROMEDRIVER and optionally CHROME set.
"""
import argparse
import base64
import json
from pathlib import Path
import runpy
import secrets
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Browser, Runtime = SUPPORT['Browser'], SUPPORT['Runtime']
SELECTOR = 'textarea[data-editor-path]'


def wait(label, predicate, seconds=30):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise AssertionError(label + ' timed out')


def cdp(browser, command, params):
    return browser.call('POST', '/goog/cdp/execute', {'cmd': command, 'params': params})


def compose(browser, text):
    end = len(text.encode('utf-16-le')) // 2
    cdp(browser, 'Input.imeSetComposition', {'text': text, 'selectionStart': end, 'selectionEnd': end})


def history_key(browser, redo=False):
    # The application accepts either Control or Meta as the command modifier.
    keys = ['\ue009'] + (['\ue008'] if redo else []) + ['z']
    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'history', 'actions':
        [{'type': 'keyDown', 'value': key} for key in keys] +
        [{'type': 'keyUp', 'value': key} for key in reversed(keys)]}]})


def keyboard_action(browser, keys):
    browser.call('POST', '/actions', {'actions': [{'type': 'key', 'id': 'editor-accessibility', 'actions':
        [{'type': 'keyDown', 'value': key} for key in keys] +
        [{'type': 'keyUp', 'value': key} for key in reversed(keys)]}]})


def check_tab_accessibility(browser):
    browser.script("document.querySelector('textarea[data-editor-path]').focus();")
    before = snapshot(browser)
    tree = cdp(browser, 'Accessibility.getFullAXTree', {})['nodes']
    inputs = [node for node in tree if node.get('role', {}).get('value') == 'textbox'
              and node.get('name', {}).get('value') == 'Code editor: input.rs']
    assert len(inputs) == 1, 'native editor must expose its file-specific accessible name'
    assert 'Ctrl+M' in inputs[0].get('description', {}).get('value', '')
    keyboard_action(browser, ['\ue009', 'm'])
    wait('Tab navigation announcement', lambda: browser.script(
        "return document.querySelector('[data-editor-tab-announcement]').textContent === 'Tab now moves keyboard focus.';"))
    for keys in [['\ue004'], ['\ue008', '\ue004']]:
        browser.script("document.querySelector('textarea[data-editor-path]').focus();")
        keyboard_action(browser, keys)
        assert browser.script("return document.activeElement !== document.querySelector('textarea[data-editor-path]');"), 'Tab must move focus out of the editor'
        after = snapshot(browser)
        assert (after['value'], after['start'], after['end']) == (before['value'], before['start'], before['end'])
    browser.script("document.querySelector('textarea[data-editor-path]').focus();")
    keyboard_action(browser, ['\ue009', 'm'])
    wait('indentation announcement', lambda: browser.script(
        "return document.querySelector('[data-editor-tab-announcement]').textContent.startsWith('Tab now indents.');"))


def snapshot(browser):
    return browser.script("""
        const input = document.querySelector('textarea[data-editor-path]');
        return input && {value:input.value, start:input.selectionStart, end:input.selectionEnd,
            bound:input.dataset.editorNativeBound === 'true', events:window.editorInputEvents};
    """)


def check(mode, ending, windowed):
    row = '// 文😀 tail' + ending
    source = row * (1500 if windowed else 3)
    with tempfile.TemporaryDirectory(prefix='openwebide-input-') as state, \
            tempfile.TemporaryDirectory(prefix='editor-input-probe-', dir=ROOT) as folder:
        runtime, browser = Runtime(state, frontend=True), None
        try:
            runtime.start()
            runtime.request('POST', '/api/auth/register', {
                'username': 'input-' + secrets.token_hex(8), 'password': secrets.token_urlsafe(32),
            }, expected=201)
            runtime.acknowledge_release()
            path = Path(folder).relative_to(ROOT.parent.parent).as_posix() if mode == 'remote' else 'input-folder'
            project = runtime.request('POST', '/api/projects', {'name': 'Input verification', 'mode': mode, 'path': path}, expected=201)
            (Path(folder) / 'input.rs').write_bytes(source.encode())
            encoded = base64.b64encode(source.encode()).decode()
            recovery = {'format': 1, 'root': {'mode': mode, 'path': path if mode == 'remote' else None},
                'selected': 'input.rs', 'files': [{'path': 'input.rs', 'document': {
                    'text': encoded, 'saved': encoded, 'selections': [{'anchor': 0, 'head': 0}], 'collapsed': []},
                    'scroll': {'top': 0, 'left': 0}, 'read_only': False}]}
            endpoint = f"/api/projects/{project['id']}/editor-recovery"
            runtime.request('PUT', endpoint, {'revision': 0, 'state': recovery})
            for key, value in {'open_tabs': json.dumps([project['id']]), 'active_project': str(project['id']),
                    'bridge_url': 'http://127.0.0.1:1', 'editor_preferences': json.dumps({'word_wrap': False})}.items():
                runtime.request('PUT', '/api/settings', {'key': key, 'value': value})
            browser = Browser(state)
            browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
            for cookie in runtime.cookies:
                browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value,
                    'domain': '127.0.0.1', 'path': cookie.path, 'httpOnly': True}})
            cdp(browser, 'Page.addScriptToEvaluateOnNewDocument', {'source': r"""
                window.editorInputEvents = [];
                window.editorInputWorker = {hold: __HOLD__, queued: [], held: 0};
                const NativeWorker = window.Worker;
                window.Worker = class extends NativeWorker {
                    constructor(url, options) {
                        super(url, options);
                        this.editorInputWorker = String(url).includes('editor-worker.js');
                    }
                    set onmessage(listener) {
                        super.onmessage = event => {
                            if (this.editorInputWorker && editorInputWorker.hold &&
                                typeof event.data === 'string' && event.data.startsWith('{')) {
                                editorInputWorker.held++;
                                editorInputWorker.queued.push(() => listener.call(this, event));
                            } else listener.call(this, event);
                        };
                    }
                };
                window.releaseEditorInputWorker = () => {
                    editorInputWorker.hold = false;
                    for (const deliver of editorInputWorker.queued.splice(0)) deliver();
                };
                for (const type of ['compositionstart','compositionupdate','compositionend','beforeinput','input','keydown']) {
                    document.addEventListener(type, event => {
                        if (event.target.matches('textarea[data-editor-path]')) {
                            editorInputEvents.push({type, trusted:event.isTrusted, composing:!!event.isComposing,
                                inputType:event.inputType || null});
                        }
                    }, true);
                }
            """.replace('__HOLD__', json.dumps(not windowed))})
            browser.call('POST', '/url', {'url': runtime.url + '/'})
            wait('editor hydration', lambda: snapshot(browser))
            if windowed:
                wait('bounded native input', lambda: snapshot(browser)['bound'])
                assert len(snapshot(browser)['value'].encode()) <= 12 * 1024
            else:
                wait('initial native text', lambda: snapshot(browser)['value'] == source.replace('\r\n', '\n'))
            if not windowed:
                wait('worker result held during cold composition', lambda: browser.script('return editorInputWorker.held > 0;'))
            browser.script("document.querySelector('textarea[data-editor-path]').focus();")
            assert snapshot(browser)['start'] == snapshot(browser)['end'] == 0
            check_tab_accessibility(browser)
            compose(browser, 'に')
            wait('first native candidate', lambda: snapshot(browser)['value'].startswith('に//'))
            compose(browser, '日本語😀')
            wait('updated native candidate', lambda: snapshot(browser)['value'].startswith('日本語😀//'))
            cdp(browser, 'Input.insertText', {'text': '日本語😀'})
            browser.script('releaseEditorInputWorker();')
            expected = '日本語😀' + source

            def saved_is(text):
                saved = runtime.request('GET', endpoint)['state']['files'][0]['document']
                return base64.b64decode(saved['text']).decode() == text

            wait('committed source with original line endings', lambda: saved_is(expected))
            history_key(browser)
            wait('one undo restores the original document', lambda: saved_is(source))
            history_key(browser, redo=True)
            wait('redo restores the committed composition', lambda: saved_is(expected))
            compose(browser, 'cancel')
            compose(browser, '')
            wait('cancel removes the candidate', lambda: 'cancel' not in snapshot(browser)['value'])
            wait('cancel preserves committed source', lambda: saved_is(expected))
            if not windowed:
                assert snapshot(browser)['value'] == expected.replace('\r\n', '\n')
            history_key(browser)
            wait('cancel adds no undo step', lambda: saved_is(source))
            history_key(browser, redo=True)
            wait('history after cancellation remains lossless', lambda: saved_is(expected))
            events = snapshot(browser)['events']
            # Chromium's CDP commit/cancel emits an untrusted compositionend.
            # Starts, candidate updates, input events and keyboard shortcuts must
            # still be generated by the engine, rather than dispatchEvent.
            assert events and all(event['trusted'] for event in events if event['type'] != 'compositionend'), events
            assert sum(event['type'] == 'compositionstart' for event in events) >= 2
            assert sum(event['type'] == 'compositionend' for event in events) >= 2
            assert any(event['type'] == 'input' and event['composing'] for event in events)
            print(json.dumps({'mode': mode, 'ending': 'CRLF' if ending == '\r\n' else 'LF',
                'windowed': windowed, 'pending_syntax': not windowed, 'trusted_events': sum(event['trusted'] for event in events), 'commit_undo_redo_cancel': True, 'tab_focus_escape_and_accessible_name': True}), flush=True)
        finally:
            if browser:
                browser.stop()
            runtime.stop()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--modes', nargs='+', choices=['local', 'remote'], default=['local', 'remote'])
    parser.add_argument('--windowed', action='store_true', help='Require bounded surrounding text')
    args = parser.parse_args()
    for mode in args.modes:
        for ending in ['\n', '\r\n']:
            check(mode, ending, args.windowed)
