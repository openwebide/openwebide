"""Check the built WASI recovery API against disposable Spin/SQLite state.

Run after `spin build`: python3 tools/check-editor-recovery.py
Add --browser with CHROMEDRIVER set to check actual edits, reload and new windows.
Local browser recovery is tested without granting a native folder capability.
"""

import argparse
import base64
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import tomllib
import urllib.error
import urllib.request


ROOT = Path(__file__).resolve().parents[1]


class Runtime:
    def __init__(self, state, frontend=False):
        self.state = Path(state)
        self.process = None
        self.log = None
        self.frontend = frontend
        self.cookies = http.cookiejar.CookieJar()
        self.client = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.cookies),
            urllib.request.ProxyHandler({}),
        )
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.url = f"http://127.0.0.1:{self.port}"

    def request(self, method, path, body=None, expected=200, authenticated=True):
        request = urllib.request.Request(
            self.url + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"x-openwebide": "1", "Content-Type": "application/json"},
            method=method,
        )
        client = self.client if authenticated else urllib.request.build_opener(
            urllib.request.ProxyHandler({})
        )
        try:
            response = client.open(request, timeout=30)
        except urllib.error.HTTPError as error:
            response = error
        with response:
            status = response.code
            value = json.loads(response.read())
        # Do not include payloads, cookies or credentials in assertion output.
        assert status == expected, f"{method} {path}: expected {expected}, got {status}"
        return value

    def start(self):
        assert self.process is None
        self.log = (self.state / "runtime.log").open("ab")
        command = [
                "spin", "up", "--direct-mounts", "--allow-transient-write",
                "--state-dir", str(self.state), "--listen", f"127.0.0.1:{self.port}",
        ]
        if not self.frontend:
            command.extend(["--component-id", "backend"])
        self.process = subprocess.Popen(
            command,
            cwd=ROOT,
            stdout=self.log,
            stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            assert self.process.poll() is None, "Isolated Spin exited before readiness"
            try:
                self.request("GET", "/api/health", authenticated=False)
                assert (self.state / "sqlite_db.db").is_file()
                return
            except (urllib.error.URLError, TimeoutError):
                time.sleep(0.1)
        raise AssertionError("Isolated Spin did not become ready")

    def acknowledge_release(self):
        """Keep editor fixtures focused on the editor after account setup."""
        version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        self.request("PUT", "/api/settings", {"key": "last_seen_release_version", "value": version})

    def stop(self):
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=10)
            self.process = None
        if self.log is not None:
            self.log.close()
            self.log = None


def encoded(text):
    return base64.b64encode(text.encode()).decode()


def draft(mode, path):
    return {
        "format": 1,
        "root": {"mode": mode, "path": path if mode == "remote" else None},
        "selected": "nested/a.rs",
        "files": [
            {
                "path": "nested/a.rs",
                "document": {
                    "text": encoded("// 日本語 🦀\r\nfn edited() {}\r\n"),
                    "saved": encoded("fn original() {}\r\n"),
                    "selections": [{"anchor": 0, "head": 3}],
                    "collapsed": [],
                },
                "scroll": {"top": 145.5, "left": 32},
                "read_only": False,
            },
            {
                "path": "empty.txt",
                "document": {
                    "text": encoded(""), "saved": encoded(""),
                    "selections": [{"anchor": 0, "head": 0}], "collapsed": [],
                },
                "scroll": {"top": 0, "left": 0},
                "read_only": True,
            },
        ],
    }


def check(browser=False):
    assert (ROOT / "target/wasm32-wasip2/release/openwebide_backend.wasm").is_file(), (
        "Build the backend with spin build first"
    )
    with tempfile.TemporaryDirectory(prefix="openwebide-recovery-") as directory, \
            tempfile.TemporaryDirectory(prefix="editor-recovery-probe-", dir=ROOT) as files:
        runtime = Runtime(directory, frontend=browser)
        try:
            runtime.start()
            runtime.request("POST", "/api/auth/register", {
                "username": "recovery-" + secrets.token_hex(8),
                "password": secrets.token_urlsafe(32),
            }, expected=201)
            runtime.acknowledge_release()
            records = []
            for mode in ("local", "remote"):
                path = Path(files).relative_to(ROOT.parent.parent).as_posix() if mode == "remote" else "test-folder"
                project = runtime.request("POST", "/api/projects", {
                    "name": f"Recovery {mode}", "mode": mode, "path": path,
                }, expected=201)
                endpoint = f"/api/projects/{project['id']}/editor-recovery"
                runtime.request("GET", endpoint, expected=401, authenticated=False)
                initial = runtime.request("GET", endpoint)
                assert initial["revision"] == 0 and initial["state"]["files"] == []
                state = draft(mode, path)
                record = {"revision": 0, "state": state}
                assert runtime.request("PUT", endpoint, record) == {"revision": 1}
                expected = {"revision": 1, "state": state}
                assert runtime.request("GET", endpoint) == expected
                runtime.request("PUT", endpoint, record, expected=409)
                invalid = {"revision": 1, "state": {**state, "format": 999}}
                runtime.request("PUT", endpoint, invalid, expected=400)
                wrong_root = {**state, "root": {"mode": "remote", "path": "other"}}
                runtime.request("PUT", endpoint, {
                    "revision": 1, "state": wrong_root,
                }, expected=409)
                assert runtime.request("GET", endpoint) == expected
                records.append((endpoint, record, expected))
            settings = runtime.request("GET", "/api/settings")
            assert "editor_recovery" not in json.dumps(settings)
            print("PASS: both modes, UTF-8/CRLF, selected/hidden tabs, validation and stale saves")

            runtime.stop()
            runtime.start()
            for endpoint, _, expected in records:
                assert runtime.request("GET", endpoint) == expected
            print("PASS: authenticated recovery after actual Spin restart with the same SQLite state")

            for endpoint, stale, _ in records:
                tombstone = {
                    "revision": 1,
                    "state": {"format": 1, "root": None, "selected": None, "files": []},
                }
                assert runtime.request("PUT", endpoint, tombstone) == {"revision": 2}
                runtime.request("PUT", endpoint, stale, expected=409)
            runtime.stop()
            runtime.start()
            for endpoint, stale, _ in records:
                restored = runtime.request("GET", endpoint)
                assert restored["revision"] == 2 and restored["state"]["files"] == []
                runtime.request("PUT", endpoint, stale, expected=409)
            print("PASS: close-all tombstones survive restart and reject stale-window resurrection")
            if browser:
                check_browser(runtime, records, Path(files))
        finally:
            runtime.stop()


class Browser:
    """Thin WebDriver transport; editing and recovery stay in the actual WASM app."""

    def __init__(self, directory):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        self.url = f"http://127.0.0.1:{port}"
        self.session = None
        self.log = (Path(directory) / "chromedriver.log").open("wb")
        self.process = subprocess.Popen(
            [os.environ["CHROMEDRIVER"], f"--port={port}"],
            stdout=self.log, stderr=subprocess.STDOUT,
        )
        self.client = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        try:
            deadline = time.monotonic() + 20
            while True:
                assert self.process.poll() is None, "ChromeDriver exited"
                try:
                    self.call("GET", "/status", session=False)
                    break
                except urllib.error.URLError:
                    assert time.monotonic() < deadline, "ChromeDriver readiness timed out"
                    time.sleep(0.1)
            options = {"args": ["--headless=new", "--window-size=1280,900",
                                 "--user-data-dir=" + str(Path(directory) / "chrome")]}
            if os.environ.get("CI"):
                # Match wasm-bindgen's headless runner on hosted Linux machines.
                options["args"] += ["--no-sandbox", "--disable-dev-shm-usage"]
            if os.environ.get("CHROME"):
                options["binary"] = os.environ["CHROME"]
            value = self.call("POST", "/session", {"capabilities": {
                "alwaysMatch": {"browserName": "chrome", "goog:chromeOptions": options},
            }}, session=False)
            self.session = value["sessionId"]
        except BaseException:
            self.stop()
            raise

    def call(self, method, path, body=None, session=True, timeout=30):
        prefix = "/session/" + self.session if session else ""
        request = urllib.request.Request(
            self.url + prefix + path,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"}, method=method,
        )
        try:
            with self.client.open(request, timeout=timeout) as response:
                return json.loads(response.read())["value"]
        except urllib.error.HTTPError as error:
            detail = json.loads(error.read()).get("value", {})
            raise RuntimeError(f"WebDriver {path}: {detail.get('message', detail)}") from error

    def script(self, source):
        return self.call("POST", "/execute/sync", {"script": source, "args": []})

    def observe_workers(self):
        self.call("POST", "/goog/cdp/execute", {"cmd": "Page.addScriptToEvaluateOnNewDocument", "params": {
            "source": r"""
                window.editorWorkerEvidence = {instances: 0, sources: []};
                const NativeWorker = window.Worker;
                window.Worker = class extends NativeWorker {
                    constructor(url, options) {
                        super(url, options);
                        if (!String(url).includes('editor-worker.js')) return;
                        window.editorWorkerEvidence.instances++;
                        this.addEventListener('message', event => {
                            if (typeof event.data !== 'string' || !event.data.startsWith('{')) return;
                            const reply = JSON.parse(event.data);
                            if (reply.status?.Ready && reply.analysis?.source !== undefined) {
                                const sources = window.editorWorkerEvidence.sources;
                                sources.push(reply.analysis.source.replace(/\r\n/g, '\n'));
                                if (sources.length > 32) sources.shift();
                            }
                        });
                    }
                };
            """,
        }})

    def wait_worker(self, expected):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            evidence = self.script("return window.editorWorkerEvidence;")
            if evidence and evidence["instances"] and expected in evidence["sources"]:
                return
            time.sleep(0.1)
        raise AssertionError("The production editor did not prepare its current source in the WASM worker")

    def wait_editor(self, expected):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            value = self.script("""
                const input = document.querySelector('textarea[data-editor-path]');
                return input ? {path: input.dataset.editorPath, text: input.value,
                    tabs: [...document.querySelectorAll('[data-editor-tab]')]
                        .map(tab => tab.dataset.editorTab)} : null;
            """)
            if value and value["path"] == "nested/a.rs" and value["text"] == expected:
                assert value["tabs"] == ["nested/a.rs", "empty.txt"]
                return
            time.sleep(0.1)
        raise AssertionError("The WASM editor did not restore its selected tab and draft")

    def stop(self):
        if self.session:
            try:
                self.call("DELETE", "")
            except (urllib.error.URLError, TimeoutError):
                pass
            self.session = None
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
        self.log.close()


def check_browser(runtime, records, files):
    assert os.environ.get("CHROMEDRIVER"), "Set CHROMEDRIVER to a compatible driver"
    (files / "nested").mkdir()
    (files / "nested/a.rs").write_bytes(b"fn original() {}\r\n")
    (files / "empty.txt").write_bytes(b"")
    runtime.request("PUT", "/api/settings", {"key": "bridge_url", "value": "http://127.0.0.1:1"})
    with tempfile.TemporaryDirectory(prefix="openwebide-recovery-browser-") as directory:
        browser = Browser(directory)
        try:
            browser.observe_workers()
            browser.call("POST", "/url", {"url": runtime.url + "/"})
            for cookie in runtime.cookies:
                browser.call("POST", "/cookie", {"cookie": {
                    "name": cookie.name, "value": cookie.value,
                    "domain": "127.0.0.1", "path": cookie.path, "httpOnly": True,
                }})
            for endpoint, original, _ in records:
                project = int(endpoint.split("/")[3])
                if original["state"]["root"]["mode"] == "remote":
                    file = runtime.request("GET", f"/api/projects/{project}/files/read?path=nested/a.rs")
                    assert file["content"] == "fn original() {}\r\n"
                runtime.request("PUT", endpoint, {"revision": 2, "state": original["state"]})
                for key, value in (("open_tabs", json.dumps([project])), ("active_project", str(project))):
                    runtime.request("PUT", "/api/settings", {"key": key, "value": value})
                expected = base64.b64decode(original["state"]["files"][0]["document"]["text"]).decode().replace("\r\n", "\n")
                browser.call("POST", "/url", {"url": runtime.url + "/"})
                browser.wait_editor(expected)
                browser.wait_worker(expected)
                # Use the real browser input path, then observe the app's debounced save.
                browser.script("""
                    const input = document.querySelector('textarea[data-editor-path]');
                    input.focus(); input.setSelectionRange(input.value.length, input.value.length);
                """)
                element = browser.call("POST", "/element", {
                    "using": "css selector", "value": "textarea[data-editor-path]",
                })["element-6066-11e4-a52e-4f735466cecf"]
                browser.call("POST", f"/element/{element}/value", {"text": "// browser edit"})
                expected += "// browser edit"
                browser.wait_editor(expected)
                browser.wait_worker(expected)
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    saved = runtime.request("GET", endpoint)
                    text = base64.b64decode(saved["state"]["files"][0]["document"]["text"]).decode()
                    if text.replace("\r\n", "\n") == expected:
                        break
                    time.sleep(0.1)
                else:
                    raise AssertionError("The WASM editor did not persist its browser input")
                runtime.stop()
                runtime.start()
                browser.call("POST", "/refresh", {})
                browser.wait_editor(expected)
                browser.wait_worker(expected)
                previous = browser.call("GET", "/window")
                window = browser.call("POST", "/window/new", {"type": "window"})
                browser.call("POST", "/window", {"handle": window["handle"]})
                browser.observe_workers()
                browser.call("POST", "/url", {"url": runtime.url + "/"})
                browser.wait_editor(expected)
                browser.wait_worker(expected)
                browser.call("DELETE", "/window")
                browser.call("POST", "/window", {"handle": previous})
                print(f"PASS: {original['state']['root']['mode']} actual edits, WASM worker preparation, autosave, server restart, reload/new-window tabs and draft")
        finally:
            browser.stop()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--browser", action="store_true", help="Also check the built app with CHROMEDRIVER")
    check(browser=parser.parse_args().browser)
