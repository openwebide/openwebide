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
                assert browser.script("return document.querySelectorAll('select[aria-label=\"Component example\"] option').length") == 13
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
                print('Production playground: desktop/phone, dark/light, downloads and embedded renderer passed')
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
