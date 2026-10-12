#!/usr/bin/env python3
"""Verify editable/read-only editor panning with Chromium-generated touch input.

Run after spin build, with CHROMEDRIVER and optionally CHROME set. Uses disposable
accounts, Spin/SQLite state and recovery drafts in both project modes. Local cases
need no folder handle. Browser-engine touch emulation does not replace physical
mobile/PWA testing.
"""
import base64
import json
from pathlib import Path
import runpy
import secrets
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-input.py'))
Browser, Runtime = SUPPORT['Browser'], SUPPORT['Runtime']
cdp, wait, snapshot = SUPPORT['cdp'], SUPPORT['wait'], SUPPORT['snapshot']


def check(mode, read_only):
    ending = "\n"
    row = '// 文😀 tail' + ending
    source = row * 1500
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
                    'scroll': {'top': 0, 'left': 0}, 'read_only': read_only}]}
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
            browser.call('POST', '/url', {'url': runtime.url + '/'})
            wait('editor hydration', lambda: snapshot(browser))
            wait('prepared input', lambda: snapshot(browser)['bound'])
            cdp(browser, 'Emulation.setTouchEmulationEnabled', {'enabled': True, 'maxTouchPoints': 5})
            browser.script("""
                window.touchEvidence=[];
                for(const type of ['touchstart','touchmove','touchend']) document.addEventListener(type,event=>{
                    if(event.target.matches('textarea[data-editor-path]')) touchEvidence.push({type,trusted:event.isTrusted,cancelable:event.cancelable,prevented:event.defaultPrevented});
                });
            """)
            box=browser.script("const r=document.querySelector('.editor-code').getBoundingClientRect();return {x:r.left+100,y:r.top+Math.min(250,r.height-40)};")
            assert browser.script("return document.querySelector('textarea[data-editor-path]').readOnly;") == read_only
            before=snapshot(browser)
            cdp(browser,'Input.dispatchTouchEvent',{'type':'touchStart','touchPoints':[{'x':box['x'],'y':box['y'],'id':1}]})
            for offset in [10,30,60,90,120]:
                cdp(browser,'Input.dispatchTouchEvent',{'type':'touchMove','touchPoints':[{'x':box['x'],'y':box['y']-offset,'id':1}]})
                time.sleep(.04)
            cdp(browser,'Input.dispatchTouchEvent',{'type':'touchEnd','touchPoints':[]})
            top=browser.script("return document.querySelector('.editor-scroll-surface').scrollTop;")
            evidence=browser.script('return touchEvidence;')
            assert top>=90, f'touch pan failed: top={top}, events={evidence}'
            after=snapshot(browser)
            assert (before['value'],before['start'],before['end'])==(after['value'],after['start'],after['end']), 'pan changed source or selection'
            assert evidence and all(event['trusted'] for event in evidence), 'expected browser-generated touch events'
            assert any(event['type'] == 'touchmove' and event['prevented'] for event in evidence), 'editor must claim the pan'
            print(json.dumps({'mode':mode,'readOnly':read_only,'scrollTop':top,'events':evidence}),flush=True)
        finally:
            if browser: browser.stop()
            runtime.stop()

if __name__ == '__main__':
    for mode in ['local','remote']:
        for read_only in [False,True]: check(mode,read_only)
