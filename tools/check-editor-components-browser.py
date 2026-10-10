#!/usr/bin/env python3
"""Run built WASM component tests in Chrome without recompiling for another host.

Generate bindings with `wasm-bindgen --target web --out-name tests --out-dir DIR
COMPONENTS.wasm`, then pass DIR and one or more test-name filters. This uses the
standard wasm-bindgen test context and raw test exports. It can run inside the
existing editor measurement image with read-only bindings and repository mounts.
"""
import argparse
import functools
import http.server
import json
from pathlib import Path
import runpy
import tempfile
import threading
import time


ROOT = Path(__file__).resolve().parent.parent
Browser = runpy.run_path(str(ROOT / "tools/check-editor-recovery.py"))["Browser"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bindings", type=Path)
    parser.add_argument("filters", nargs="+")
    args = parser.parse_args()
    assert (args.bindings / "tests.js").is_file(), "Missing generated tests.js"
    script = """
        import init, * as bindings from '/tests.js';
        window.__wbg_test_invoke = callback => callback();
        try {
            const wasm = await init();
            for (const method of ['log', 'debug', 'info', 'warn', 'error']) {
                const original = console[method].bind(console);
                console[method] = (...args) => {
                    bindings['__wbgtest_console_' + method](args);
                    document.getElementById('console_output').textContent += args.map(String).join(' ') + '\\n';
                    original(...args);
                };
            }
            const filters = FILTERS;
            const names = Object.keys(wasm).filter(name => name.startsWith('__wbgt_') && filters.some(filter => name.includes(filter)));
            const missing = filters.filter(filter => !names.some(name => name.includes(filter)));
            if (missing.length) throw new Error('No matching WASM test exports for: ' + missing.join(', '));
            window.editorComponentNames = names;
            const context = new bindings.WasmBindgenTestContext(false);
            window.editorComponentResult = await context.run(names.map(name => wasm[name]));
            context.free();
        } catch (error) {
            document.getElementById('output').textContent += String(error.stack || error);
            window.editorComponentResult = false;
        }
    """.replace("FILTERS", json.dumps(args.filters))

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/":
                body = b'<pre id="output" style="display:none"></pre><pre id="console_output" style="display:none"></pre><script type="module" src="/runner.js"></script>'
                content_type = "text/html; charset=utf-8"
            elif self.path == "/runner.js":
                body = script.encode()
                content_type = "text/javascript; charset=utf-8"
            else:
                return super().do_GET()
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), functools.partial(Handler, directory=str(args.bindings))
    )
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="editor-component-browser-") as directory:
            browser = Browser(directory)
            try:
                browser.call("POST", "/url", {"url": f"http://127.0.0.1:{server.server_port}/"})
                deadline = time.monotonic() + 300
                while True:
                    result = browser.script("return {passed:window.editorComponentResult ?? null, tests:window.editorComponentNames, output:document.getElementById('output').textContent, console:document.getElementById('console_output').textContent, browser:navigator.userAgent}")
                    if result["passed"] is not None:
                        print(json.dumps(result), flush=True)
                        assert result["passed"] is True, "WASM component tests failed"
                        break
                    if time.monotonic() >= deadline:
                        # Preserve the executed-test output and first failure even
                        # when a later contract prevents the run from completing.
                        print(json.dumps(result), flush=True)
                        raise AssertionError("WASM component tests timed out")
                    time.sleep(0.2)
            finally:
                browser.stop()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
