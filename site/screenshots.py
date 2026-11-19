"""Capture the release frontend with fictional responses from a loopback fixture."""

import base64
from difflib import SequenceMatcher
import hashlib
import http.server
import json
import os
from pathlib import Path
import runpy
import struct
import sys
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlsplit

from screenshot_features import (
    git_commit_diff, git_history, pending_monitor, plugin_marketplaces, rust_invocation, rust_provider,
)

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "site/fixtures"
WORKSPACE = FIXTURES / "harbor-forecast"
FILE = "src/forecast.rs"
VIEWPORT = (1440, 900)
THEME = "dark"
CREATED_AT = 1794672000
PROJECT = {"id": 1, "name": "Harbor Forecast", "mode": "remote", "path": "harbor-forecast", "created_at": CREATED_AT}
PROMPT = "Can you handle empty weather readings without returning NaN?"
REPLY = "The forecast now returns `None` for an empty set of readings. Callers can show “No forecast yet” instead of displaying `NaN`. I also added a regression test for the empty case."


def message(identifier, role, content):
    return {"kind": "message", "id": identifier, "role": role, "content": content,
            "session_id": 1, "created_at": CREATED_AT}


class Demo:
    def __init__(self, version):
        self.settings = {"theme": THEME, "last_seen_release_version": version,
                         "open_tabs": "[1]", "active_project": "1", "last_session_1": "1",
                         "default_connection": "1", "panel_visibility": '{"sessions": false}',
                         "editor_preferences": '{"word_wrap": true}'}
        self.recovery = {"revision": 0, "state": {"format": 1, "selected": None, "files": [],
                         "root": {"mode": "remote", "path": PROJECT["path"]}}}
        source = (WORKSPACE / FILE).read_bytes()
        original = (FIXTURES / "forecast-at-head.rs").read_text()
        changes = [edit for edit in SequenceMatcher(None, original.splitlines(), source.decode().splitlines()).get_opcodes()
                   if edit[0] != "equal"]
        stats = {"insertions": sum(end - start for _, _, _, start, end in changes),
                 "deletions": sum(end - start for _, start, end, _, _ in changes)}
        files = {path.relative_to(WORKSPACE).as_posix(): path for path in WORKSPACE.rglob("*")}
        self.listings = {"": self.entries(WORKSPACE)}
        self.listings.update({name: self.entries(path) for name, path in files.items() if path.is_dir()})
        self.source = source
        self.pending_edit = {
            "project_id": PROJECT["id"], "path": FILE, "revision": 1, "decision": "pending",
            "diff": {"path": FILE, "old": original, "new": source.decode()},
        }
        self.provider = rust_provider()
        self.history = git_history()
        self.commit_diff = git_commit_diff(FILE, (FIXTURES / "forecast-before-readings.rs").read_text(), original)
        self.responses = {
            "/api/auth/me": {"user": {"id": 1, "username": "demo", "role": "admin", "created_at": CREATED_AT}},
            "/api/health": {"status": "ok", "version": version},
            "/api/host/connection": {"enabled": False, "destination": "", "port": 22,
                                     "platform": "auto", "known_hosts": ""},
            "/api/settings": self.settings,
            "/api/projects": [PROJECT],
            "/api/connections": [{"id": 1, "name": "Forecast model", "kind": "ollama",
                                  "base_url": "http://model.example.invalid", "model": "weather-coder",
                                  "enabled": True, "context_limit": 32768}],
            "/api/models": [{"name": "weather-coder"}],
            "/api/model-setup": {},
            "/api/system-prompts": [],
            "/api/plugins": [{"prepared": self.provider, "revision": 1, "hosts": ["demo-host"],
                              "installed_at": CREATED_AT, "default_enabled": True, "update_policy": "notify"}],
            "/api/plugin-marketplaces": plugin_marketplaces(self.provider, CREATED_AT),
            "/api/projects/1/git/branches": [{"name": "main", "is_current": True, "is_remote": False}],
            "/api/projects/1/plugins": [{"id": 1, "revision": 1, "enabled": True, "prepared": self.provider}],
            "/api/projects/1/memories": {"enabled": True, "entries": []},
            "/api/projects/1/skills": {"enabled": True, "entries": []},
            "/api/scheduled-tasks": [],
            "/api/projects/1/workspace-activity": {"file_recovery": False, "language_command": False},
            "/api/projects/1/workspace-transactions": None,
            "/api/projects/1/workspace-transactions/root": {"identity": "harbor-forecast-demo"},
            "/api/projects/1/pending-edits": [],
            "/api/projects/1/editor-recovery": self.recovery,
            "/api/projects/1/git/status": {
                "branch": "main", "commit_hash": self.history["commits"][0]["hash"],
                "commit_message": self.history["commits"][0]["subject"],
                "upstream": None, "ahead": 0, "behind": 0, "is_clean": False,
                "line_stats": stats, "file_line_stats": {FILE: stats}, "files": {FILE: "modified"},
            },
            "/api/projects/1/git/path-status": {
                "has_head": True, "staged": [], "unstaged": [FILE], "untracked": [], "renamed_from": {},
            },
            "/api/projects/1/git/show": {"content": original},
            "/api/sessions": [{"id": 1, "project_id": 1, "connection_id": 1,
                               "name": "Handle missing readings", "created_at": CREATED_AT}],
            "/api/sessions/1/messages": [message(1, "user", PROMPT), message(2, "assistant", REPLY)],
            "/api/sessions/1/queue": [],
            "/api/sessions/1/goal": None,
            "/api/sessions/1/todos": None,
            "/api/sessions/1/monitors": [],
        }

    @staticmethod
    def entries(directory):
        return [{"name": path.name, "path": path.relative_to(WORKSPACE).as_posix(),
                 "is_dir": path.is_dir(), "size": 0 if path.is_dir() else path.stat().st_size}
                for path in sorted(directory.iterdir())]

    def get(self, handler, path, query):
        file = query.get("path", [""])[0]
        if path == "/api/theme.js":
            handler.reply(200, f'document.documentElement.dataset.theme = "{THEME}";'.encode(), "text/javascript")
        elif path == "/api/projects/1/files" and file in self.listings:
            handler.reply_json(200, self.listings[file])
        elif path == "/api/browse" and file == "":
            handler.reply_json(200, [{"name": PROJECT["path"], "path": PROJECT["path"], "is_dir": True, "size": 0}])
        elif path == "/api/browse" and file == PROJECT["path"]:
            handler.reply_json(200, [{**entry, "path": f"{file}/{entry['path']}"} for entry in self.listings[""]])
        elif file == FILE and path in ("/api/projects/1/files/raw", "/api/projects/1/files/bounded"):
            handler.reply(200, self.source, "application/octet-stream")
        elif file == FILE and path == "/api/projects/1/files/read":
            handler.reply_json(200, {"content": self.source.decode()})
        elif file == FILE and path == "/api/projects/1/files/canonical":
            handler.reply_json(200, {"path": FILE})
        elif path in self.responses:
            handler.reply_json(200, self.responses[path])
        else:
            handler.unavailable()

    def put(self, handler):
        if handler.path not in ("/api/settings", "/api/projects/1/editor-recovery"):
            handler.unavailable()
            return
        data = json.loads(handler.rfile.read(int(handler.headers["Content-Length"])))
        if handler.path == "/api/settings":
            self.settings[data["key"]] = data["value"]
            handler.reply_json(200, {"ok": True})
        elif data["revision"] == self.recovery["revision"]:
            self.recovery.update(revision=data["revision"] + 1, state=data["state"])
            handler.reply_json(200, {"revision": self.recovery["revision"]})
        else:
            handler.reply_json(409, {"error": "Recovery revision changed"})


def demo_handler(bundle, demo):
    class Handler(http.server.SimpleHTTPRequestHandler):
        extensions_map = {**http.server.SimpleHTTPRequestHandler.extensions_map, ".wasm": "application/wasm"}

        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=str(bundle), **kwargs)

        def log_message(self, *args):
            pass

        def end_headers(self):
            # The compiled app sees only this origin and the fixture's data. Its
            # embedded styles and WebAssembly bootstrap still run normally.
            self.send_header("Content-Security-Policy", "default-src 'self'; "
                             "script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval'; "
                             "style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; "
                             "worker-src 'self' blob:; connect-src 'self'")
            super().end_headers()

        def reply(self, status, payload, content_type):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def reply_json(self, status, value):
            self.reply(status, json.dumps(value).encode(), "application/json")

        def unavailable(self):
            self.reply_json(404, {"error": "Unavailable in the screenshot workspace"})

        def do_GET(self):
            url = urlsplit(self.path)
            if url.path.startswith("/api/"):
                demo.get(self, url.path, parse_qs(url.query))
            elif url.path == "/service-worker.js":
                self.unavailable()
            else:
                super().do_GET()

        def do_PUT(self):
            demo.put(self)

        def do_POST(self):
            request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
            path = self.path
            if path == "/api/projects/1/language-actions/recovery" and request == {"operation": "load", "id": None}:
                self.reply_json(200, None)
            elif path == "/api/models/preview":
                models = {
                    "weather-coder": {"context_limit": 32768, "source": "Demo model metadata",
                                      "capabilities": ["completion", "tools"], "loaded": True},
                    "weather-small": {"context_limit": 8192, "source": "Demo model metadata",
                                      "capabilities": ["completion"], "loaded": False},
                    "weather-embeddings": {"source": "Demo model metadata", "capabilities": ["embedding"]},
                }
                self.reply_json(200, {"base_url": request["base_url"], "kind": request["kind"],
                                      "models": [{"name": name} for name in models], "detections": models})
            elif path == "/api/projects/1/git/history":
                self.reply_json(200, demo.history)
            elif path == "/api/projects/1/git/commit-diff" and request.get("hash") == demo.history["commits"][0]["hash"]:
                self.reply_json(200, demo.commit_diff)
            elif path == "/api/projects/1/git/stash" and request.get("action") == "list":
                self.reply_json(200, {"stashes": [], "output": ""})
            elif path == "/api/projects/1/plugins/preparation" and request.get("source") == demo.provider["source"]:
                self.reply_json(200, {"id": "1" * 32, "state": "ready", "prepared": demo.provider, "error": None})
            elif path == "/api/plugins/execution-grants":
                self.reply_json(200, {demo.provider["digest"]: "documentation-fixture"})
            elif path == "/api/plugins/invoke" and request.get("call", {}).get("operation") == "language":
                self.reply_json(200, rust_invocation(request, FILE, demo.source))
            else:
                self.unavailable()

    return Handler


def wait(browser, expression, description):
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if browser.script(f"return Boolean({expression});"):
            return
        time.sleep(0.1)
    visible = browser.script("return document.body.innerText.slice(0, 1500);")
    raise RuntimeError(f"Screenshot workspace did not show {description}:\n{visible}")


def element_request(browser, strategy, selector, action, data):
    element = browser.call("POST", "/element", {"using": strategy, "value": selector})
    element_id = element["element-6066-11e4-a52e-4f735466cecf"]
    browser.call("POST", f"/element/{element_id}/{action}", data)


def click(browser, strategy, selector):
    target = json.dumps(selector)
    if strategy == "xpath":
        find = f"document.evaluate({target}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE).singleNodeValue"
    else:
        find = f"document.querySelector({target})"
    wait(browser, f"({find})?.getClientRects().length", selector)
    element_request(browser, strategy, selector, "click", {})


def type_text(browser, selector, value):
    element_request(browser, "css selector", selector, "value", {"text": value})


def cdp(browser, command, params):
    return browser.call("POST", "/goog/cdp/execute", {"cmd": command, "params": params})


def set_viewport(browser, width, height, *, phone=False):
    cdp(browser, "Emulation.setDeviceMetricsOverride", {
        "width": width, "height": height, "deviceScaleFactor": 1, "mobile": phone,
    })


def open_forecast(browser):
    folder = f'#panel-files [data-tree-path="src"]'
    file = f'#panel-files [data-tree-path="{FILE}"]'
    wait(browser, f'document.querySelector({json.dumps(folder)})?.getClientRects().length', "src")
    if not browser.script(f'return Boolean(document.querySelector({json.dumps(file)})?.getClientRects().length);'):
        click(browser, "css selector", folder)
    wait(browser, f'document.querySelector({json.dumps(file)})?.getClientRects().length', FILE)
    click(browser, "css selector", file)


def capture_workspace(browser, url):
    set_viewport(browser, *VIEWPORT)
    cdp(browser, "Emulation.setEmulatedMedia", {"features": [
        {"name": "prefers-color-scheme", "value": THEME},
        {"name": "prefers-reduced-motion", "value": "reduce"},
    ]})
    cdp(browser, "Emulation.setTimezoneOverride", {"timezoneId": "UTC"})
    browser.call("POST", "/url", {"url": url})
    open_forecast(browser)
    wait(browser, "document.body.innerText.includes('The forecast now returns')", "the demo conversation")
    wait(browser, "document.querySelector('.editor-highlight')?.textContent.includes('empty_forecast_has_no_average')",
         "the source in the editor")
    wait(browser, "!document.body.innerText.includes('File saves are paused') "
         "&& !document.body.innerText.includes('Unavailable in the screenshot workspace')",
         "the workspace without fixture errors")
    click(browser, "xpath", "//*[contains(@class, 'editor-view-desktop')]//button[normalize-space(.)='Diff']")
    wait(browser, "document.querySelector('.editor-diff .diff-line.add') "
         "&& document.querySelector('.editor-diff .diff-line.del')", "the inline Git diff")
    result = browser.call("POST", "/execute/async", {"script": """
        const done = arguments[0];
        document.activeElement?.blur();
        document.fonts.ready.then(() => requestAnimationFrame(() => requestAnimationFrame(() => {
            done({ width: innerWidth, height: innerHeight, scrollWidth: document.documentElement.scrollWidth,
                theme: document.documentElement.dataset.theme });
        })));
    """, "args": []})
    if ((result["width"], result["height"]) != VIEWPORT or result["scrollWidth"] > VIEWPORT[0]
            or result["theme"] != THEME):
        raise ValueError(f"Unexpected screenshot layout: {result}")
    return base64.b64decode(browser.call("GET", "/screenshot"), validate=True)


def capture_panel(browser, selector, height=VIEWPORT[1]):
    clip = browser.call("POST", "/execute/async", {"script": """
        const [selector, maxHeight, done] = arguments;
        document.fonts.ready.then(() => requestAnimationFrame(() => requestAnimationFrame(() => {
            const bounds = Array.from(document.querySelectorAll(selector), element => element.getBoundingClientRect())
                .filter(rect => rect.width > 0 && rect.height > 0);
            if (!bounds.length) { done(null); return; }
            const x = Math.max(0, Math.ceil(Math.min(...bounds.map(rect => rect.left))));
            const y = Math.max(0, Math.ceil(Math.min(...bounds.map(rect => rect.top))));
            const right = Math.min(innerWidth, Math.floor(Math.max(...bounds.map(rect => rect.right))));
            const bottom = Math.min(innerHeight, Math.floor(Math.max(...bounds.map(rect => rect.bottom))));
            done({x, y, width: right - x, height: Math.min(bottom - y, maxHeight), scale: 1});
        })));
    """, "args": [selector, height]})
    if not clip or clip["width"] <= 0 or clip["height"] <= 0:
        raise ValueError(f"No visible screenshot area for {selector}")
    result = cdp(browser, "Page.captureScreenshot", {
        "format": "png", "clip": clip, "captureBeyondViewport": False,
    })
    return base64.b64decode(result["data"], validate=True)


def capture_features(browser):
    editor = "#panel-editor"
    click(browser, "xpath", "//*[contains(@class, 'editor-view-desktop')]//button[normalize-space(.)='Edit']")
    wait(browser, "document.querySelector('.editor-highlight .tok-keyword') "
         "&& document.querySelector('.editor-highlight .tok-type')", "syntax highlighting")
    yield "editor-syntax", capture_panel(browser, editor, 535)

    click(browser, "css selector", '#panel-editor button[aria-label^="Find in file"]')
    search = 'input[aria-label="Find in file"]'
    wait(browser, f'document.querySelector({json.dumps(search)})?.getClientRects().length', "Find in file")
    type_text(browser, search, "average_temperature")
    wait(browser, "document.querySelector('.editor-find .form-hint')?.textContent.trim() === '1 / 3'", "three search matches")
    yield "editor-find", capture_panel(browser, editor, 500)
    click(browser, "css selector", '#panel-editor button[aria-label="Close find"]')

    # History has a commit list, file tree and diff. Start with room to show the
    # example patch's full lines, then crop away the rest of the workspace.
    set_viewport(browser, 1800, VIEWPORT[1])
    click(browser, "css selector", 'button[aria-controls="panel-history"]')
    for panel in ("files", "editor", "chat"):
        click(browser, "css selector", f'button[aria-controls="panel-{panel}"]')
    wait(browser, "document.querySelector('.git-history-row')?.getClientRects().length", "commit history")
    click(browser, "css selector", ".git-history-row")
    wait(browser, "document.querySelector('.git-commit-file button')?.getClientRects().length", "changed files")
    click(browser, "css selector", ".git-commit-file button")
    wait(browser, "document.querySelector('.git-readonly-diff .diff-line.add') "
         "&& document.querySelector('.git-commit-file[aria-selected=true]')", "the selected file's commit diff")
    yield "git-history", capture_panel(browser, "#panel-history", 740)

    click(browser, "css selector", 'button[aria-controls="panel-history"]')
    for panel in ("files", "editor", "chat"):
        click(browser, "css selector", f'button[aria-controls="panel-{panel}"]')
    set_viewport(browser, *VIEWPORT)


def capture_navigation(browser):
    click(browser, "css selector", 'button[aria-label="App menu"]')
    click(browser, "xpath", '//*[@role="menuitem"][normalize-space(.)="Open remote project"]')
    folder = '//button[contains(@class,"browser-item")][normalize-space(.)="harbor-forecast"]'
    wait(browser, "document.querySelector('.browser-list button.dir')", "the remote folder listing")
    click(browser, "xpath", folder)
    wait(browser, "document.querySelector('.browser-path')?.textContent.includes('harbor-forecast') "
         "&& document.querySelector('.browser-list')?.innerText.includes('Cargo.toml')",
         "the remote project folder and its contents")
    yield "remote-folder", capture_panel(browser, '[role="dialog"]:has(.browser-list)')
    click(browser, "css selector", '[role="dialog"]:has(.browser-list) button[aria-label="Close dialog"]')

    click(browser, "css selector", 'button[aria-label="App menu"]')
    click(browser, "xpath", '//*[@role="menuitem"][normalize-space(.)="Servers"]')
    wait(browser, "document.querySelector('.server-configure')", "the saved server and its endpoint")
    yield "model-servers", capture_panel(browser, '[role="dialog"]:has(.server-list)')
    click(browser, "xpath", '//button[normalize-space(.)="Add server"]')
    wait(browser, "document.querySelector('.model-setup input[type=url]')", "the server connection step")
    yield "model-setup", capture_panel(browser, '[role="dialog"]:has(.model-setup)')
    click(browser, "xpath", '//button[normalize-space(.)="Discover models"]')
    wait(browser, "document.querySelector('.model-review:not([hidden])')?.innerText.includes('2 chat models will be saved.')",
         "the compact model review")
    yield "model-review", capture_panel(browser, '[role="dialog"]:has(.model-review)')
    click(browser, "css selector", '[role="dialog"]:has(.model-setup) button[aria-label="Close dialog"]')
    click(browser, "css selector", '[role="dialog"]:has(.app-configuration) button[aria-label="Close dialog"]')

    click(browser, "css selector", "#command-search")
    type_text(browser, "#command-search", "/forecast")
    wait(browser, "document.querySelector('#command-search').dataset.loading === 'false' "
         "&& document.querySelector('#command-results').innerText.includes('src/forecast.rs') "
         "&& document.querySelector('#command-results').innerText.includes('tests/forecast.rs')",
         "the two matching forecast files")
    yield "workspace-search", capture_panel(browser, ".omnibar-field, .omnibar-panel")
    type_text(browser, "#command-search", "\ue00c")  # WebDriver Escape


def capture_project_controls(browser):
    click(browser, "css selector", '.files-panel-toolbar button[aria-label="File actions"]')
    click(browser, "xpath", '//*[@role="menuitem"][normalize-space(.)="Refresh files"]')
    file_row = f'#panel-files [data-tree-path="{FILE}"]'
    click(browser, "css selector", f'{file_row} button[aria-label="File actions"]')
    file_menu = f'{file_row} [role="menu"]'
    wait(browser, f'''document.querySelector('{file_menu} button:not(:disabled)[title^="Explain"]')
         && Array.from(document.querySelectorAll('{file_menu} button:not(:disabled)'))
             .some(button => button.textContent.trim() === 'Stage')''', "the file's enabled Stage and chat actions")
    yield "file-actions", capture_panel(browser, f"{file_row}, {file_menu}")
    click(browser, "css selector", ".ui-dropdown-backdrop")

    click(browser, "xpath", '//*[@id="panel-files"]//button[normalize-space(.)="Changes"]')
    wait(browser, '''document.querySelector('.git-pane [aria-label="Unstaged changes"]')?.innerText.includes('forecast.rs')
         && document.querySelector('.git-pane [aria-label="Stage file"]:not(:disabled)')
         && document.querySelector('.git-pane [aria-label="Git branch"]')?.textContent.includes('main')
         && !document.querySelector('.git-pane [role="alert"]')''',
         "working-tree changes")
    yield "git-working-changes", capture_panel(browser, "#panel-files", 575)
    click(browser, "xpath", '//*[@id="panel-files"]//button[normalize-space(.)="Tree"]')

    click(browser, "css selector", 'button[aria-label="Account menu"]')
    click(browser, "css selector", '[role="menuitem"][aria-label="Settings"]')
    click(browser, "css selector", "#settings-tab-editor")
    wait(browser, "document.querySelector('#settings-panel-editor:not([hidden])')?.innerText.includes('Coding ligatures')",
         "the editor appearance settings")
    yield "editor-settings", capture_panel(browser, '[role="dialog"]:has(#settings-tab-editor)')
    click(browser, "css selector", "#settings-tab-host")
    wait(browser, """['settings-execution-bridge', 'settings-host-administration', 'settings-host-updates']
         .every(id => document.getElementById(id)?.getClientRects().length)""", "the host settings sections")
    yield "host-settings", capture_panel(browser, '[role="dialog"]:has(#settings-tab-host)', 375)
    click(browser, "css selector", '[role="dialog"]:has(#settings-tab-editor) button[aria-label="Close dialog"]')

    click(browser, "css selector", 'button[title="Browse and manage plugins"]')
    wait(browser, "document.querySelector('.plugin-entry')?.innerText.includes('Enabled in this project') "
         "&& Array.from(document.querySelectorAll('.plugin-entry')).some(row => row.innerText.includes('Demo TOML') "
         "&& row.innerText.includes('Install'))", "installed and available language plugins")
    yield "project-plugin", capture_panel(browser, '[role="dialog"]:has([aria-label="Search plugins"])')
    click(browser, "css selector", 'button[aria-label="Plugin actions"]')
    click(browser, "xpath", '//*[@role="menuitem"][normalize-space(.)="Manage marketplace sources"]')
    wait(browser, """document.querySelector('#plugins-panel-sources:not([hidden])')?.innerText
         .includes('Add marketplace')""", "marketplace sources in the Plugins dialog")
    yield "marketplace-sources", capture_panel(browser, '[role="dialog"]:has(#plugins-tab-sources)')
    click(browser, "css selector", '[role="dialog"]:has([aria-label="Search plugins"]) button[aria-label="Close dialog"]')

    click(browser, "css selector", 'button[aria-controls="panel-sessions"]')
    click(browser, "css selector", 'button[aria-label="New task"]')
    type_text(browser, '[aria-label="Task title"]', "Weekly forecast review")
    type_text(browser, '[aria-label="Task prompt"]', "Summarize the week's changes to the forecast service.")
    wait(browser, "document.querySelector('.scheduled-tasks .task-weekdays')?.getClientRects().length",
         "the repeating task form")
    browser.script("document.querySelector('.scheduled-tasks').scrollIntoView({block: 'start'});")
    yield "scheduled-task", capture_panel(browser, ".scheduled-tasks")
    click(browser, "xpath", '//*[@aria-label="Scheduled tasks"]//button[normalize-space(.)="Cancel"]')
    click(browser, "css selector", 'button[aria-controls="panel-sessions"]')


def capture_chat_workflows(browser, url, demo):
    composer = 'textarea[aria-label="Chat message"]'
    type_text(browser, composer, "/c")
    wait(browser, "document.querySelector('#slash-suggestions')?.innerText.includes('/compact') "
         "&& document.querySelector('#slash-suggestions')?.innerText.includes('/context')", "slash command suggestions")
    yield "chat-commands", capture_panel(browser, "#slash-suggestions, .tui-composer")

    demo.responses["/api/projects/1/pending-edits"] = [demo.pending_edit]
    demo.responses["/api/sessions/1/monitors"] = [pending_monitor(PROJECT["id"], CREATED_AT + 600)]
    browser.call("POST", "/url", {"url": url})
    open_forecast(browser)
    wait(browser, "document.querySelector('.editor-diff-actions')?.innerText.includes('Accept') "
         "&& document.querySelector('.editor-diff-actions')?.innerText.includes('Reject') "
         "&& document.querySelector('.editor-diff .diff-line.add')", "the pending edit diff and review buttons")
    yield "pending-edit", capture_panel(browser, "#panel-editor", 530)

    wait(browser, '''document.querySelector('button[aria-label="View monitors"]')''', "the active monitor indicator")
    click(browser, "css selector", 'button[aria-label="View monitors"]')
    wait(browser, "document.querySelector('.tui-monitors')?.innerText.includes('Cancel future checks')",
         "the active monitor's status and controls")
    yield "conversation-monitor", capture_panel(browser, ".tui-monitor-menu, .tui-monitor-label")
    click(browser, "css selector", ".ui-dropdown-backdrop")

    set_viewport(browser, 390, 844, phone=True)
    wait(browser, "document.querySelector('.app-drawer-trigger')?.getClientRects().length", "phone navigation")
    click(browser, "css selector", 'button[aria-controls="panel-editor"]')
    wait(browser, '''document.querySelector('#panel-editor .editor-diff .diff-line.add')?.getClientRects().length
         && document.querySelector('button[aria-controls="panel-editor"][aria-current="page"]')''',
         "the phone editor and review controls")
    yield "workspace-phone", base64.b64decode(browser.call("GET", "/screenshot"), validate=True)
    click(browser, "css selector", ".app-drawer-trigger")
    wait(browser, "document.querySelector('.app-drawer #app-drawer')?.innerText.includes('Open local project') "
         "&& document.querySelector('.app-drawer-status')?.innerText.includes('Plugins')",
         "the phone drawer and workspace status")
    yield "phone-drawer", capture_panel(browser, ".app-drawer", 700)


def capture(bundle, destination, revision, version, modified):
    if not os.environ.get("CHROME") or not os.environ.get("CHROMEDRIVER"):
        raise ValueError("Set CHROME and CHROMEDRIVER to the project's pinned browser pair")
    # Reuse the same private-profile launcher as the production browser checks.
    sys.path.insert(0, str(ROOT / "tools"))
    try:
        browser_type = runpy.run_path(str(ROOT / "tools/check-editor-recovery.py"))["Browser"]
    finally:
        sys.path.pop(0)
    demo = Demo(version)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), demo_handler(bundle, demo))
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix="openwebide-site-browser-") as temporary:
            browser = browser_type(temporary)
            try:
                url = f"http://127.0.0.1:{server.server_port}/"
                captures = {"workspace": capture_workspace(browser, url)}
                captures.update(capture_features(browser))
                captures.update(capture_navigation(browser))
                captures.update(capture_project_controls(browser))
                captures.update(capture_chat_workflows(browser, url, demo))
            finally:
                browser.stop()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()
    destination.mkdir()
    records = []
    for name, png in captures.items():
        if len(png) < 24 or png[:16] != b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR":
            raise ValueError(f"Chrome did not return a PNG for {name}")
        width, height = struct.unpack(">II", png[16:24])
        if not (0 < width <= VIEWPORT[0] and 0 < height <= VIEWPORT[1]):
            raise ValueError(f"Invalid image dimensions for {name}: {width} × {height}")
        filename = f"{name}.png"
        (destination / filename).write_bytes(png)
        records.append({"name": name, "filename": filename, "theme": THEME, "width": width, "height": height,
                        "sha256": hashlib.sha256(png).hexdigest()})
    (destination / "manifest.json").write_text(json.dumps({
        "revision": revision, "working_tree_modified": modified, "captures": records,
    }, indent=2) + "\n")
