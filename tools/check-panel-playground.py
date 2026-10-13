#!/usr/bin/env python3
"""Check the compiled docs playground in an isolated headless Chrome profile."""
import argparse
import base64
import functools
import http.server
import json
import os
from pathlib import Path
import runpy
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
Browser = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))['Browser']


def ready(browser):
    assert browser.call('POST', '/execute/async', {'script': """
        const done = arguments[0], deadline = Date.now() + 20000;
        function poll() {
            if (document.querySelector('.plugin-panel-document')) done(true);
            else if (Date.now() > deadline) done(false);
            else setTimeout(poll, 25);
        }
        poll();
    """, 'args': []}), 'Compiled panel renderer did not load'


def visual_gestures(browser):
    """Use native mouse/touch input against the compiled Rust pointer handlers."""
    initial = json.loads(browser.script("return document.querySelector('textarea[aria-label=\"Panel JSON\"]').value"))
    initial['document']['blocks'] = [
        {'type': 'group', 'title': 'Destination', 'blocks': []},
        {'type': 'text', 'text': 'Move me'},
    ]
    browser.script(f"""
        const source = document.querySelector('textarea[aria-label="Panel JSON"]');
        source.value = {json.dumps(json.dumps(initial))};
        source.dispatchEvent(new Event('input', {{bubbles:true}}));
    """)

    def wait(expression):
        deadline = time.monotonic() + 10
        while not browser.script('return ' + expression):
            assert time.monotonic() < deadline, expression
            time.sleep(0.05)

    def document():
        return json.loads(browser.script("return document.querySelector('textarea[aria-label=\"Panel JSON\"]').value"))['document']

    def drag(label, parent, index, pointer):
        selector = f'button[aria-label="{label}"]'
        points = browser.script(f"""
            const source = document.querySelector({json.dumps(selector)});
            const target = [...document.querySelectorAll('[data-panel-drop]')]
                .find(element => element.dataset.panelDrop === {json.dumps(json.dumps([parent, index], separators=(',', ':')))});
            const a = source.getBoundingClientRect(), b = target.getBoundingClientRect();
            return {{a:{{x:a.x+a.width/2,y:a.y+a.height/2}},b:{{x:b.x+b.width/2,y:b.y+b.height/2}}}};
        """)
        browser.call('POST', '/actions', {'actions': [{
            'type': 'pointer', 'id': 'panel-' + pointer, 'parameters': {'pointerType': pointer},
            'actions': [
                {'type': 'pointerMove', 'duration': 0, 'x': round(points['a']['x']), 'y': round(points['a']['y'])},
                {'type': 'pointerDown', 'button': 0},
                {'type': 'pointerMove', 'duration': 250, 'x': round(points['b']['x']), 'y': round(points['b']['y'])},
                {'type': 'pointerUp', 'button': 0},
            ],
        }]})

    wait("document.querySelectorAll('.panel-builder-block').length === 2")
    drag('Select or drag block 2', [0], 0, 'mouse')
    wait("JSON.parse(document.querySelector('textarea[aria-label=\"Panel JSON\"]').value).document.blocks[0].blocks.length === 1")
    assert len(document()['blocks']) == 1
    drag('Select or drag block 1.1', [], 1, 'touch')
    wait("JSON.parse(document.querySelector('textarea[aria-label=\"Panel JSON\"]').value).document.blocks.length === 2")
    assert document()['blocks'][0]['blocks'] == []
    drag('Add or drag Heading', [], 0, 'mouse')
    wait("JSON.parse(document.querySelector('textarea[aria-label=\"Panel JSON\"]').value).document.blocks[0].type === 'heading'")
    assert len(document()['blocks']) == 3, 'Palette drops must not also activate the add button'
    before = document()
    drag('Select or drag block 2', [1], 0, 'mouse')
    wait("document.querySelector('.visual-panel-editor').textContent.includes('cannot be moved into itself')")
    assert document() == before, 'Invalid cycles must leave the canonical document unchanged'
    drag('Add or drag Divider', [], 0, 'pen')
    wait("JSON.parse(document.querySelector('textarea[aria-label=\"Panel JSON\"]').value).document.blocks[0].type === 'divider'")
    assert len(document()['blocks']) == 4
    # Changing the source while a palette pointer is held must suppress its later click.
    point = browser.script("""
        const rect = document.querySelector('button[aria-label="Add or drag Text"]').getBoundingClientRect();
        return {x:rect.x+rect.width/2,y:rect.y+rect.height/2};
    """)
    browser.call('POST', '/actions', {'actions': [{
        'type': 'pointer', 'id': 'stale-panel', 'parameters': {'pointerType': 'mouse'},
        'actions': [
            {'type': 'pointerMove', 'duration': 0, 'x': round(point['x']), 'y': round(point['y'])},
            {'type': 'pointerDown', 'button': 0},
            {'type': 'pointerMove', 'duration': 100, 'x': round(point['x']) + 10, 'y': round(point['y'])},
        ],
    }]})
    changed = json.loads(json.dumps(initial))
    changed['document']['blocks'] = []
    browser.script(f"""
        const source = document.querySelector('textarea[aria-label="Panel JSON"]');
        source.value = {json.dumps(json.dumps(changed))};
        source.dispatchEvent(new Event('input', {{bubbles:true}}));
    """)
    wait("document.querySelectorAll('.panel-builder-block').length === 0")
    browser.call('POST', '/actions', {'actions': [{
        'type': 'pointer', 'id': 'stale-panel', 'parameters': {'pointerType': 'mouse'},
        'actions': [{'type': 'pointerUp', 'button': 0}],
    }]})
    assert document()['blocks'] == [], 'A stale palette gesture must not add to the replacement document'
    # The grip remains a useful button when it is clicked without dragging.
    browser.call('POST', '/actions', {'actions': [{
        'type': 'pointer', 'id': 'stale-panel', 'parameters': {'pointerType': 'mouse'},
        'actions': [
            {'type': 'pointerMove', 'duration': 0, 'x': round(point['x']), 'y': round(point['y'])},
            {'type': 'pointerDown', 'button': 0},
            {'type': 'pointerUp', 'button': 0},
        ],
    }]})
    wait("JSON.parse(document.querySelector('textarea[aria-label=\"Panel JSON\"]').value).document.blocks.length === 1")
    # Restore the gallery so exports and layout checks use the complete schema example.
    browser.script("""
        const gallery = document.querySelector('select[aria-label="Component example"]');
        gallery.value = '0'; gallery.dispatchEvent(new Event('change', {bubbles:true}));
    """)
    wait("JSON.parse(document.querySelector('textarea[aria-label=\"Panel JSON\"]').value).document.blocks.length === 12")


def check(site, screenshots=None):
    assert os.environ.get('CHROMEDRIVER'), 'Set CHROMEDRIVER to a compatible driver'
    assert (site / 'tools/panel-playground/index.html').is_file(), 'Build the site first'
    requests = []

    class Handler(http.server.SimpleHTTPRequestHandler):
        def log_request(self, code='-', size='-'):
            requests.append((self.path, code))

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(
        ('127.0.0.1', 0), functools.partial(Handler, directory=str(site)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='openwebide-playground-browser-') as temporary:
            directory = Path(temporary)
            downloads = directory / 'downloads'
            downloads.mkdir()
            browser = Browser(directory)
            try:
                browser.call('POST', '/goog/cdp/execute', {
                    'cmd': 'Browser.setDownloadBehavior',
                    'params': {'behavior': 'allow', 'downloadPath': str(downloads)}})
                origin = f'http://127.0.0.1:{server.server_port}'
                browser.call('POST', '/url', {'url': origin + '/tools/panel-playground/index.html'})
                ready(browser)
                assert browser.script("return document.querySelectorAll('select[aria-label=\"Component example\"] option:not([disabled])').length") == 13
                visual_gestures(browser)
                for width, theme in [(1280, 'dark'), (390, 'light')]:
                    browser.call('POST', '/window/rect', {'width': width, 'height': 900})
                    browser.call('POST', '/goog/cdp/execute', {
                        'cmd': 'Emulation.setDeviceMetricsOverride',
                        'params': {'width': width, 'height': 900, 'deviceScaleFactor': 1, 'mobile': width < 600}})
                    browser.script(f"""
                        const theme = document.querySelector('select[aria-label="Theme"]');
                        theme.value = '{theme}'; theme.dispatchEvent(new Event('change', {{bubbles:true}}));
                    """)
                    layout = browser.script("""
                        return {width: innerWidth, scroll: document.documentElement.scrollWidth,
                            theme: document.documentElement.dataset.theme,
                            blocks: document.querySelector('.plugin-panel-document').children.length};
                    """)
                    assert layout['width'] == width and layout['scroll'] <= width, layout
                    assert layout['theme'] == theme and layout['blocks'] > 0, layout
                    if screenshots:
                        screenshots.mkdir(parents=True, exist_ok=True)
                        (screenshots / f'panel-playground-{width}-{theme}.png').write_bytes(
                            base64.b64decode(browser.call('GET', '/screenshot')))
                browser.script("document.querySelector('header button').click()")
                browser.script("document.querySelectorAll('header button')[1].click()")
                deadline = time.monotonic() + 15
                while not all((downloads / name).is_file() for name in ('panel.json', 'lib.rs')):
                    assert time.monotonic() < deadline, 'JSON/Rust downloads did not complete'
                    time.sleep(0.05)
                exported = json.loads((downloads / 'panel.json').read_text())
                assert exported['schema_version'] == 1 and exported['revision'] == 1
                assert len(exported['blocks']) == 12
                rust = (downloads / 'lib.rs').read_text()
                assert 'openwebide_plugin_sdk::export!(Example)' in rust
                assert 'fn panels()' in rust and 'fn panel(input: PanelInput)' in rust
                assert browser.script("return navigator.serviceWorker.getRegistrations().then(items => items.length)") == 0
                browser.call('POST', '/url', {'url': origin + '/docs/panel-playground.html'})
                frame = browser.script("return document.querySelector('iframe[title=\"Interactive Open WebIDE panel playground\"]')")
                assert frame, 'Documentation does not embed the playground'
                browser.call('POST', '/frame', {'id': frame})
                ready(browser)
                assert not any('/api/' in path or '/bridge/' in path for path, _ in requests), requests
                assert not any(code == 404 and not path.endswith('/favicon.ico') for path, code in requests), requests
                print('Production playground: mouse/touch/pen drag, nesting, cycle/stale-gesture rejection, desktop/phone, dark/light, downloads and embedded renderer passed')
            finally:
                browser.stop()
    finally:
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--site', type=Path, required=True)
    parser.add_argument('--screenshots', type=Path)
    args = parser.parse_args()
    check(args.site.resolve(), args.screenshots)
