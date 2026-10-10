#!/usr/bin/env python3
"""Measure the built Rust/WASM editor using disposable Spin/SQLite and Chrome.

Set CHROMEDRIVER (and optionally CHROME) after spin build. Results are JSON lines;
each case has a fresh account, runtime and browser process tree. Local cases use
recovery without an OS directory handle, so these are editor/view measurements,
not local filesystem permission checks. Summed RSS includes shared pages more
than once; Linux PSS, when readable, apportions those pages across processes.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import runpy
import secrets
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / "tools/check-editor-recovery.py"))
Browser, Runtime = SUPPORT["Browser"], SUPPORT["Runtime"]


def host_constraints():
    """Record Linux resource limits so container samples keep their context."""
    constraints = {"cpuCount": os.cpu_count()}
    for field, name in [("cgroupMemoryMax", "memory.max"), ("cgroupCpuMax", "cpu.max")]:
        try:
            constraints[field] = Path("/sys/fs/cgroup", name).read_text().strip()
        except OSError:
            constraints[field] = None
    return constraints


def process_memory(driver):
    # Chrome can create or retire a utility process between ps and smaps reads.
    # Retry only the snapshot, retaining None when coverage stays incomplete.
    for attempt in range(1, 4):
        result = process_memory_snapshot(driver)
        result["memorySnapshotAttempts"] = attempt
        if result["chrome_pss_kib"] is not None or not Path("/proc").is_dir():
            return result
        if attempt < 3:
            time.sleep(.01)
    return result


def process_memory_snapshot(driver):
    records = {}
    for line in subprocess.check_output(["ps", "-axo", "pid=,ppid=,rss="], text=True).splitlines():
        pid, parent, rss = map(int, line.split())
        records[pid] = (parent, rss)
    owned = {driver}
    while True:
        children = {pid for pid, (parent, _) in records.items() if parent in owned}
        if children <= owned:
            break
        owned |= children
    owned.discard(driver)
    pss = []
    for pid in owned:
        try:
            fields = Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()
            pss.append(int(next(line for line in fields if line.startswith("Pss:")).split()[1]))
        except (OSError, StopIteration):
            pass
    return {"chrome_processes": len(owned),
            "chrome_summed_rss_kib": sum(records[pid][1] for pid in owned),
            "chrome_pss_kib": sum(pss) if len(pss) == len(owned) and owned else None}


def source_for(case):
    row = "let value = call(\"文😀 café\"); // words for wrapping\r\n"
    if case == "small":
        return row * 1_000
    if case == "medium":
        return row * (2 * 1024 * 1024 // len(row.encode()))
    if case == "byte-limit":
        row = "let value = call(\"文😀 café\"); // " + "words " * 64 + "\r\n"
        return row * (8 * 1024 * 1024 // len(row.encode()))
    if case == "line-limit":
        return "x\n" * 99_999
    if case == "long-line":
        return "文😀 words " * (1024 * 1024 // len("文😀 words ".encode()))
    if case in {"styled-long-line", "styled-long-line-following-row"}:
        prefix, suffix, unit = 'const VALUE: &str = "', '";', "文😀 words "
        budget = 1024 * 1024 - len((prefix + suffix).encode())
        source = prefix + unit * (budget // len(unit.encode())) + suffix
        return source + "\nlet next = 0;" if case == "styled-long-line-following-row" else source
    raise ValueError(case)


class MemorySamples:
    """Observe owned Chrome processes; never collect unrelated process arguments."""

    def __init__(self, driver):
        self.driver = driver
        self.initial = process_memory(driver)
        self.peak_rss = self.initial["chrome_summed_rss_kib"]
        self.peak_pss = self.initial["chrome_pss_kib"]
        self.count = 1
        self.error = None
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.sample, daemon=True)
        self.thread.start()

    def sample(self):
        while not self.stopped.wait(.2):
            try:
                value = process_memory(self.driver)
                self.peak_rss = max(self.peak_rss, value["chrome_summed_rss_kib"])
                if value["chrome_pss_kib"] is not None:
                    self.peak_pss = max(self.peak_pss or 0, value["chrome_pss_kib"])
                self.count += 1
            except (OSError, ValueError) as error:
                self.error = type(error).__name__
                return

    def finish(self):
        self.stopped.set()
        self.thread.join()
        assert self.error is None, f"Process memory sampling failed: {self.error}"
        final = process_memory(self.driver)
        self.peak_rss = max(self.peak_rss, final["chrome_summed_rss_kib"])
        if final["chrome_pss_kib"] is not None:
            self.peak_pss = max(self.peak_pss or 0, final["chrome_pss_kib"])
        self.count += 1
        return {"beforeLoadMemory": self.initial, "peakChromeSummedRssKiB": self.peak_rss,
                "peakChromePssKiB": self.peak_pss, "memorySamples": self.count,
                "memorySampleIntervalMs": 200}


READY = """
    window.editorViewObservePaint?.();
    const input = document.querySelector('textarea[data-editor-path]');
    const paint = document.querySelector('.editor-highlight:not(.editor-caret-measure) .editor-highlight-content');
    const extent = input?.parentElement.querySelector('.editor-scroll-extent');
    const bound = input?.dataset.editorNativeBound === 'true' &&
        input.dataset.editorNativeGeneration && extent?.dataset.editorScope === input.dataset.editorScope;
    return input && paint && (input.value.length === arguments[0] || bound) &&
        paint.dataset.editorScope === input.dataset.editorScope &&
        (getComputedStyle(input).whiteSpace !== 'pre-wrap' ||
            (Number(extent?.dataset.sourceHeight) > 0 && extent.dataset.editorScope === input.dataset.editorScope)) &&
        input.parentElement.classList.contains('highlight-ready') &&
        (!arguments[1] || !!paint.querySelector('.tok-string'));
"""


def measure(case, mode, wrapped, trace=False, repetition=1, input_position="start", profile=False, file_name="measure.rs"):
    source = source_for(case)
    require_styled = case in {"styled-long-line", "styled-long-line-following-row"}
    native = source.replace("\r\n", "\n")
    # JS lengths are UTF-16, not Python's Unicode scalar count.
    native_length = len(native.encode("utf-16-le")) // 2
    with tempfile.TemporaryDirectory(prefix="openwebide-view-") as state, \
            tempfile.TemporaryDirectory(prefix="editor-view-probe-", dir=ROOT) as folder:
        runtime = Runtime(state, frontend=True)
        browser = None
        samples = None
        phase = "runtime"
        try:
            runtime.start()
            runtime.request("POST", "/api/auth/register", {
                "username": "measure-" + secrets.token_hex(8), "password": secrets.token_urlsafe(32),
            }, expected=201)
            path = Path(folder).relative_to(ROOT.parent.parent).as_posix() if mode == "remote" else "measure-folder"
            project = runtime.request("POST", "/api/projects", {
                "name": "View measurement", "mode": mode, "path": path,
            }, expected=201)
            (Path(folder) / file_name).write_bytes(source.encode())
            encoded = base64.b64encode(source.encode()).decode()
            recovery = {"format": 1, "root": {"mode": mode, "path": path if mode == "remote" else None},
                        "selected": file_name, "files": [{"path": file_name,
                        "document": {"text": encoded, "saved": encoded,
                                     "selections": [{"anchor": 0, "head": 0}], "collapsed": []},
                        "scroll": {"top": 0, "left": 0}, "read_only": False}]}
            runtime.request("PUT", f"/api/projects/{project['id']}/editor-recovery",
                            {"revision": 0, "state": recovery})
            for key, value in {
                "open_tabs": json.dumps([project["id"]]), "active_project": str(project["id"]),
                "bridge_url": "http://127.0.0.1:1",
                "editor_preferences": json.dumps({"indentation": {"style": "Spaces", "width": 4, "tab_width": 4},
                                                   "word_wrap": wrapped, "show_whitespace": False}),
            }.items():
                runtime.request("PUT", "/api/settings", {"key": key, "value": value})
            browser = Browser(state)
            browser.call("POST", "/url", {"url": runtime.url + "/offline.html"})
            for cookie in runtime.cookies:
                browser.call("POST", "/cookie", {"cookie": {"name": cookie.name, "value": cookie.value,
                             "domain": "127.0.0.1", "path": cookie.path, "httpOnly": True}})
            samples = MemorySamples(browser.process.pid)
            if profile:
                for command, params in [("Performance.enable", {}), ("Profiler.enable", {}),
                                        ("Profiler.setSamplingInterval", {"interval": 2000}), ("Profiler.start", {})]:
                    browser.call("POST", "/goog/cdp/execute", {"cmd": command, "params": params})
            browser.call("POST", "/goog/cdp/execute", {"cmd": "Page.addScriptToEvaluateOnNewDocument", "params": {
                "source": """
                    window.editorViewScroll = input => {
                        const scroll = input.parentElement?.querySelector('.editor-scroll-surface');
                        return scroll && getComputedStyle(scroll).position === 'absolute' ? scroll : input;
                    };
                    window.editorViewMeasurement = {maxFrameMs: 0, inputAt: 0,
                        coldViewportPaintMs: null, coldCompleteGeometryMs: null,
                        inputViewportPaintMs: null, inputCompleteGeometryMs: null,
                        coldViewportHadCompleteExtent: null, inputViewportHadCompleteExtent: null,
                        inputPreviousScope: null, inputScrollTop: null, inputScrollLeft: null,
                        longTasks: 0, longTaskMs: 0, maxLongTaskMs: 0, longTaskEvents: [], phase: "cold", probes: [], batches: [], workers: [], fonts: [], nativeEvents: [], traceTruncated: false};
                    if (__TRACE__) {
                        editorViewMeasurement.runtimeErrors = [];
                        const recordError = text => {
                            if (editorViewMeasurement.runtimeErrors.length < 16)
                                editorViewMeasurement.runtimeErrors.push(String(text).slice(0,1024));
                            else editorViewMeasurement.traceTruncated = true;
                        };
                        window.addEventListener('error', event => recordError(event.message));
                        const originalError = console.error.bind(console);
                        console.error = (...args) => { recordError(args.map(String).join(' ')); originalError(...args); };
                        const batches = new WeakMap();
                        window.__openwebideEditorProbeTiming = (paint, phase, elapsedMs, units) => {
                            let record = batches.get(paint);
                            if (phase === "render" || phase === "paragraph-render") {
                                record = null;
                                if (editorViewMeasurement.batches.length < 256) {
                                    record = {at: performance.now(), phase: editorViewMeasurement.phase,
                                        fontFaces: Array.from(document.fonts).slice(0, 16)
                                            .map(face => ({family: face.family, status: face.status})),
                                        scope: Object.fromEntries(Array.from(paint.closest(".editor-row-measure").attributes)
                                            .filter(attribute => attribute.name.startsWith("data-measure-"))
                                            .map(attribute => [attribute.name, attribute.value]))};
                                    editorViewMeasurement.batches.push(record);
                                } else editorViewMeasurement.traceTruncated = true;
                                batches.set(paint, record);
                            }
                            if (record) record[phase] = {elapsedMs, units};
                        };
                        const rows = new WeakMap();
                        function recordFor(element) {
                            const row = element?.closest(".editor-row-measure .editor-source-line");
                            let record = row && rows.get(row);
                            if (row && !record && editorViewMeasurement.probes.length < 256) {
                                record = {at: performance.now(), phase: editorViewMeasurement.phase,
                                    cold: !!row.closest(".editor-height-measure"),
                                    sliced: row.hasAttribute("data-source-start"),
                                    sourceNative: row.textContent.length,
                                    probeStyle: row.closest(".editor-row-measure").getAttribute("style"),
                                    rowStyle: row.getAttribute("style"),
                                    scope: Object.fromEntries(Array.from(row.closest(".editor-row-measure").attributes)
                                        .filter(attribute => attribute.name.startsWith("data-measure-"))
                                        .map(attribute => [attribute.name, attribute.value])),
                                    calls: 0, layoutMs: 0,
                                    boundsCalls: 0, boundsMs: 0};
                                rows.set(row, record); editorViewMeasurement.probes.push(record);
                            }
                            if (row && !record) editorViewMeasurement.traceTruncated = true;
                            return record;
                        }
                        const original = Range.prototype.getClientRects;
                        Range.prototype.getClientRects = function(...args) {
                            const node = this.startContainer;
                            const record = recordFor(node.nodeType === 1 ? node : node.parentElement);
                            const started = performance.now();
                            try { return original.apply(this, args); }
                            finally { if (record) { record.calls++; record.layoutMs += performance.now()-started; } }
                        };
                        const originalBounds = Element.prototype.getBoundingClientRect;
                        Element.prototype.getBoundingClientRect = function(...args) {
                            const record = recordFor(this);
                            const started = performance.now();
                            try { return originalBounds.apply(this, args); }
                            finally { if (record) { record.boundsCalls++; record.boundsMs += performance.now()-started; } }
                        };
                        function recordEvent(events, kind) {
                            if (events.length >= 256) {
                                editorViewMeasurement.traceTruncated = true;
                                return null;
                            }
                            const record = {at: performance.now(), phase: editorViewMeasurement.phase, kind};
                            events.push(record);
                            return record;
                        }
                        // Native extent reads can force whole-source layout before
                        // a bounded window is installed. Keep this separate from
                        // the styled row probes, and never read value just to log it.
                        function nativeRecord(input, kind) {
                            if (!(input instanceof HTMLTextAreaElement) ||
                                !input.matches('.editor-textarea, [data-editor-path]')) return null;
                            const record = recordEvent(editorViewMeasurement.nativeEvents, kind);
                            if (record) record.bound = input.dataset.editorNativeBound === 'true';
                            return record;
                        }
                        const inputEvents = new WeakMap();
                        for (const kind of ['beforeinput', 'input']) {
                            document.addEventListener(kind, event => {
                                const record = nativeRecord(event.target, kind);
                                if (record) inputEvents.set(event, {record, started: performance.now()});
                            }, true);
                            document.addEventListener(kind, event => {
                                const measured = inputEvents.get(event);
                                if (measured) measured.record.elapsedMs = performance.now() - measured.started;
                            });
                        }
                        for (const property of ['scrollWidth', 'scrollHeight']) {
                            const descriptor = Object.getOwnPropertyDescriptor(Element.prototype, property);
                            if (!descriptor?.get) continue;
                            Object.defineProperty(Element.prototype, property, {
                                ...descriptor,
                                get() {
                                    const record = nativeRecord(this, property), started = performance.now();
                                    try { return descriptor.get.call(this); }
                                    finally { if (record) record.elapsedMs = performance.now() - started; }
                                }
                            });
                        }
                        const nativeValue = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value');
                        if (nativeValue?.set) Object.defineProperty(HTMLTextAreaElement.prototype, 'value', {
                            ...nativeValue,
                            set(value) {
                                const record = nativeRecord(this, 'value'), started = performance.now();
                                if (record) record.nativeLength = typeof value === 'string' ? value.length : null;
                                try { return nativeValue.set.call(this, value); }
                                finally { if (record) record.elapsedMs = performance.now() - started; }
                            }
                        });
                        for (const kind of ["loading", "loadingdone", "loadingerror"])
                            document.fonts.addEventListener(kind, event => {
                                const record = recordEvent(editorViewMeasurement.fonts, kind);
                                if (record) record.faces = Array.from(event.fontfaces || []).slice(0, 16)
                                    .map(face => ({family: face.family, status: face.status}));
                            });
                        const OriginalWorker = Worker;
                        Worker = class extends OriginalWorker {
                            constructor(...args) {
                                super(...args);
                                this.addEventListener("message", event => {
                                    const record = recordEvent(editorViewMeasurement.workers, "reply");
                                    if (!record) return;
                                    record.messageUtf16Units = typeof event.data === "string" ? event.data.length : null;
                                    try {
                                        const reply = typeof event.data === "string" ? JSON.parse(event.data) : event.data;
                                        record.status = reply?.status ?? null;
                                        record.hasAnalysis = !!reply?.analysis;
                                        record.highlightRows = Array.isArray(reply?.analysis?.highlights)
                                            ? reply.analysis.highlights.length : null;
                                    } catch { record.status = "non-JSON control message"; }
                                });
                            }
                            postMessage(...args) {
                                recordEvent(editorViewMeasurement.workers, "request");
                                return super.postMessage(...args);
                            }
                        };
                    }
                    new PerformanceObserver(list => {
                        for (const entry of list.getEntries()) {
                            if (__TRACE__ && editorViewMeasurement.longTaskEvents.length < 256) {
                                editorViewMeasurement.longTaskEvents.push({at: entry.startTime,
                                    durationMs: entry.duration, observedAt: performance.now(),
                                    phase: editorViewMeasurement.phase,
                                    scope: document.querySelector('textarea[data-editor-path]')?.dataset.editorScope ?? null});
                            } else if (__TRACE__) editorViewMeasurement.traceTruncated = true;
                            editorViewMeasurement.longTasks++;
                            editorViewMeasurement.longTaskMs += entry.duration;
                            editorViewMeasurement.maxLongTaskMs = Math.max(editorViewMeasurement.maxLongTaskMs, entry.duration);
                        }
                    }).observe({type: 'longtask', buffered: true});
                    window.editorViewObservePaint = () => {
                        const input = document.querySelector('textarea[data-editor-path]');
                        const paint = input?.parentElement.querySelector('.editor-highlight-content');
                        const scope = input?.dataset.editorScope;
                        if (scope && paint?.dataset.editorScope === scope &&
                            input.parentElement.classList.contains('highlight-ready') &&
                            (!__REQUIRE_STYLED__ || paint.querySelector('.tok-string'))) {
                            const extent = input.parentElement.querySelector('.editor-scroll-extent');
                            const complete = extent?.dataset.editorScope === scope &&
                                Number(extent.dataset.sourceHeight) > 0;
                            const measurement = editorViewMeasurement;
                            if (measurement.phase === 'cold') {
                                if (measurement.coldViewportPaintMs === null) {
                                    measurement.coldViewportPaintMs = performance.now();
                                    measurement.coldViewportHadCompleteExtent = complete;
                                }
                                if (complete && measurement.coldCompleteGeometryMs === null)
                                    measurement.coldCompleteGeometryMs = performance.now();
                            } else if (measurement.phase === 'input' && measurement.inputAt &&
                                scope !== measurement.inputPreviousScope) {
                                if (measurement.inputViewportPaintMs === null) {
                                    measurement.inputViewportPaintMs = performance.now() - measurement.inputAt;
                                    measurement.inputViewportHadCompleteExtent = complete;
                                }
                                if (complete && measurement.inputCompleteGeometryMs === null)
                                    measurement.inputCompleteGeometryMs = performance.now() - measurement.inputAt;
                            }
                        }
                    };
                    let last;
                    function tick(at) {
                        if (last !== undefined) editorViewMeasurement.maxFrameMs = Math.max(editorViewMeasurement.maxFrameMs, at-last);
                        editorViewObservePaint();
                        last = at; requestAnimationFrame(tick);
                    }
                    requestAnimationFrame(tick);
                    document.addEventListener('beforeinput', event => {
                        if (event.target.matches('textarea[data-editor-path]')) {
                            editorViewMeasurement.inputAt = performance.now();
                            editorViewMeasurement.inputPreviousScope = event.target.dataset.editorScope;
                            const scroll = editorViewScroll(event.target);
                            editorViewMeasurement.inputScrollTop = scroll?.scrollTop ?? null;
                            editorViewMeasurement.inputScrollLeft = scroll?.scrollLeft ?? null;
                        }
                    }, true);
                """.replace("__TRACE__", json.dumps(trace)).replace("__REQUIRE_STYLED__", json.dumps(require_styled))}})
            started = time.monotonic()
            phase = "cold paint"
            browser.call("POST", "/url", {"url": runtime.url + "/"})
            deadline = time.monotonic() + 60
            while not browser.call("POST", "/execute/sync", {"script": READY, "args": [native_length, require_styled]}):
                assert time.monotonic() < deadline, "Editor view readiness timed out"
                time.sleep(0.05)
            load_ms = (time.monotonic() - started) * 1000
            snapshot = browser.call("POST", "/execute/async", {"script": r"""
                const done = arguments[0];
                (async () => {
                    const link = [...document.querySelectorAll('link[rel=modulepreload]')]
                        .find(link => /\/openwebide-frontend-[^/]+\.js$/.test(link.href));
                    const module = await import(link.href); const wasm = await module.default();
                    window.editorViewWasmMemory = wasm.memory;
                    const input = document.querySelector('textarea[data-editor-path]');
                    const scroll = editorViewScroll(input);
                    done({styledString: !!document.querySelector('.editor-highlight-content .tok-string'), nativeInputLength: input.value.length, nativeBound: input.dataset.editorNativeBound === 'true', appModule: new URL(link.href).pathname, wasmCommittedBytes: wasm.memory.buffer.byteLength,
                        domNodes: document.getElementsByTagName('*').length,
                        paintRows: document.querySelectorAll('.editor-source-line').length,
                        scrollHeight: scroll.scrollHeight, scrollWidth: scroll.scrollWidth, clientWidth: scroll.clientWidth,
                        maxFrameMs: editorViewMeasurement.maxFrameMs,
                        wrapped: getComputedStyle(input).whiteSpace === 'pre-wrap'});
                })().catch(error => done({error: String(error)}));
            """, "args": []})
            assert "error" not in snapshot, snapshot
            assert not require_styled or snapshot["styledString"], "Prepared string style was unavailable"
            assert snapshot["wrapped"] == wrapped, "Editor preference was not applied"
            memory = process_memory(browser.process.pid)
            phase = "scroll paint"
            scroll = browser.call("POST", "/execute/async", {"script": """
                const done = arguments[arguments.length - 1], input = document.querySelector('textarea[data-editor-path]');
                const singleRow = arguments[0], requireStyled = arguments[1];
                const scroll = editorViewScroll(input);
                const horizontal = singleRow && getComputedStyle(input).whiteSpace !== 'pre-wrap';
                editorViewMeasurement.phase = "scroll";
                const started = performance.now(); const old = document.querySelector('.editor-source-line')?.dataset.line;
                if (horizontal) scroll.scrollLeft = (scroll.scrollWidth - scroll.clientWidth) * .7;
                else scroll.scrollTop = scroll.scrollHeight * .7;
                scroll.dispatchEvent(new Event('scroll'));
                const deadline = started + 10000;
                function check() {
                    const row = document.querySelector('.editor-source-line');
                    const first = row?.dataset.line;
                    const paint = document.querySelector('.editor-highlight-content');
                    if (performance.now() > deadline) return done({error: 'Scroll paint timed out'});
                    const offset = Number(horizontal ? row?.dataset.paintLeft : row?.dataset.paintTop);
                    const target = horizontal ? scroll.scrollLeft : scroll.scrollTop;
                    const margin = horizontal ? scroll.clientWidth * 2 + 30 : scroll.clientHeight + 8 * parseFloat(getComputedStyle(input).lineHeight) + 32;
                    // A wrapped long row can occupy many screens even when the
                    // file has other rows. Prove its crop moved using geometry;
                    // ordinary multiline windows can also change the first row.
                    const cropped = Number.isFinite(offset) && offset <= target + 30 && target - offset <= margin;
                    const moved = cropped || (!singleRow && first !== old);
                    if (moved && paint?.dataset.editorScope === input.dataset.editorScope && input.parentElement.classList.contains('highlight-ready') && (!requireStyled || paint.querySelector('.tok-string')))
                        return done({scrollToPaintMs: performance.now()-started, scrollAxis: horizontal ? 'horizontal' : 'vertical', scrollOffset: target});
                    requestAnimationFrame(check);
                }
                requestAnimationFrame(check);
            """, "args": ["\n" not in native, require_styled]})
            assert "error" not in scroll, scroll
            verified_source_caret = None
            if input_position == "start":
                browser.script("const input=document.querySelector('textarea[data-editor-path]'); input.focus(); input.setSelectionRange(0,0);")
            else:
                phase = "input navigation"
                browser.script("editorViewMeasurement.phase = 'navigate'; document.querySelector('textarea[data-editor-path]').focus();")
                key = "Home" if input_position == "beginning" else "End"
                key_code = 36 if input_position == "beginning" else 35
                verified_source_caret = 0 if input_position == "beginning" else len(source.encode())
                for kind in ["keyDown", "keyUp"]:
                    browser.call("POST", "/goog/cdp/execute", {"cmd": "Input.dispatchKeyEvent", "params": {
                        "type": kind, "key": key, "code": key, "modifiers": 2,
                        "windowsVirtualKeyCode": key_code, "nativeVirtualKeyCode": key_code,
                    }})
                deadline = time.monotonic() + 10
                while True:
                    recovered = runtime.request("GET", f"/api/projects/{project['id']}/editor-recovery")
                    document = recovered["state"]["files"][0]["document"]
                    if document["selections"][0]["head"] == verified_source_caret:
                        assert document["selections"][0]["anchor"] == verified_source_caret, "Navigation retained a selection"
                        assert document["text"] == encoded, "Document navigation changed source"
                        break
                    assert time.monotonic() < deadline, "Source caret did not reach the requested document boundary"
                    time.sleep(0.1)
            phase = "input paint"
            browser.script("editorViewMeasurement.phase = 'input';")
            browser.call("POST", "/goog/cdp/execute", {"cmd": "Input.insertText", "params": {"text": "z"}})
            edited = browser.call("POST", "/execute/async", {"script": """
                const done=arguments[arguments.length-1], inputPosition=arguments[0], requireStyled=arguments[1], deadline=performance.now()+10000;
                function check() {
                    editorViewObservePaint();
                    const input=document.querySelector('textarea[data-editor-path]');
                    const paint=document.querySelector('.editor-highlight-content');
                    if (performance.now()>deadline) return done({error:'Input paint timed out'});
                    if ((inputPosition === 'end' ? input.value.endsWith('z') : input.value.startsWith('z')) && paint.dataset.editorScope===input.dataset.editorScope &&
                        (getComputedStyle(input).whiteSpace !== 'pre-wrap' ||
                            (Number(input.parentElement.querySelector('.editor-scroll-extent')?.dataset.sourceHeight)>0 &&
                             input.parentElement.querySelector('.editor-scroll-extent')?.dataset.editorScope===input.dataset.editorScope)) &&
                        input.parentElement.classList.contains('highlight-ready') && (!requireStyled || paint.querySelector('.tok-string')))
                        return done({inputToPaintMs:performance.now()-editorViewMeasurement.inputAt});
                    requestAnimationFrame(check);
                }
                requestAnimationFrame(check);
            """, "args": [input_position, require_styled]})
            assert "error" not in edited, edited
            browser.script("editorViewMeasurement.phase = 'deferred';")
            # Include deferred syntax preparation and queued recovery saves.
            browser.call("POST", "/execute/async", {"script": "setTimeout(arguments[0], 1200);", "args": []})
            if input_position in {"beginning", "end"}:
                expected_source = "z" + source if input_position == "beginning" else source + "z"
                deadline = time.monotonic() + 10
                while True:
                    recovered = runtime.request("GET", f"/api/projects/{project['id']}/editor-recovery")
                    actual = base64.b64decode(recovered["state"]["files"][0]["document"]["text"]).decode()
                    if actual == expected_source:
                        break
                    assert time.monotonic() < deadline, "Boundary input did not preserve complete source"
                    time.sleep(.1)
            sampled = samples.finish()
            samples = None
            tasks = browser.script("return {...window.editorViewMeasurement, wasmBytes: window.editorViewWasmMemory.buffer.byteLength};")
            profiling = {}
            if profile:
                metrics = browser.call("POST", "/goog/cdp/execute", {"cmd": "Performance.getMetrics", "params": {}})
                cpu = browser.call("POST", "/goog/cdp/execute", {"cmd": "Profiler.stop", "params": {}})
                profiling = {"cpuProfile": cpu["profile"], "cpuProfileMetrics": metrics["metrics"]}
            return {**profiling, "cpuProfiling": profile, "case": case, "fileName": file_name, "mode": mode, "wrap": wrapped, "repetition": repetition, "sourceBytes": len(source.encode()),
                    "inputPosition": input_position, "verifiedSourceCaretBeforeInput": verified_source_caret,
                    "completeSourceAfterInputVerified": input_position in {"beginning", "end"}, "loadToPaintMs": load_ms, **snapshot, **scroll, **edited, **memory,
                    "afterInputMemory": process_memory(browser.process.pid), **sampled,
                    **{key: tasks[key] for key in ["coldViewportPaintMs", "coldCompleteGeometryMs",
                        "coldViewportHadCompleteExtent", "inputViewportPaintMs", "inputCompleteGeometryMs",
                        "inputViewportHadCompleteExtent", "inputScrollTop", "inputScrollLeft"]},
                    "afterInputWasmCommittedBytes": tasks["wasmBytes"],
                    "longTasks": tasks["longTasks"], "longTaskMs": tasks["longTaskMs"],
                    "maxLongTaskMs": tasks["maxLongTaskMs"], "maxFrameIncludingInputMs": tasks["maxFrameMs"],
                    **({"layoutProbes": tasks["probes"], "preparationBatches": tasks["batches"], "workerEvents": tasks["workers"], "fontEvents": tasks["fonts"], "nativeEvents": tasks["nativeEvents"], "longTaskEvents": tasks["longTaskEvents"], "traceTruncated": tasks["traceTruncated"]} if trace else {})}
        except (AssertionError, TimeoutError, RuntimeError) as error:
            observed = samples.finish() if samples else {}
            samples = None
            diagnostics = {}
            if trace and browser:
                try:
                    diagnostics = browser.call("POST", "/execute/sync", {"script": """
                        const state=globalThis.editorViewMeasurement ?? {};
                        const input=document.querySelector('textarea[data-editor-path]');
                        const paint=document.querySelector('.editor-highlight-content');
                        const extent=input?.parentElement.querySelector('.editor-scroll-extent');
                        return {failureState:{nativeLength:input?.value.length,
                            nativeStartsWithMarker:input?.value.startsWith('z'),
                            inputScope:input?.dataset.editorScope,paintScope:paint?.dataset.editorScope,
                            extentScope:extent?.dataset.editorScope,sourceHeight:extent?.dataset.sourceHeight,
                            highlightReady:input?.parentElement.classList.contains('highlight-ready'),
                            alerts:[...document.querySelectorAll('[role="alert"],.notice.error')].map(node=>node.textContent.slice(0,512))},
                            layoutProbes:state.probes,preparationBatches:state.batches,workerEvents:state.workers,
                            nativeEvents:state.nativeEvents,fontEvents:state.fonts,runtimeErrors:state.runtimeErrors,traceTruncated:state.traceTruncated};
                    """, "args": []}, timeout=5)
                except (OSError, ValueError, RuntimeError) as diagnostic_error:
                    diagnostics = {"failureDiagnosticError": type(diagnostic_error).__name__}
                try:
                    recovered = runtime.request("GET", f"/api/projects/{project['id']}/editor-recovery")
                    actual = base64.b64decode(recovered["state"]["files"][0]["document"]["text"]).decode()
                    diagnostics["failureRecovery"] = {"sourceBytes": len(actual.encode()),
                        "unchanged": actual == source, "markerDelta": actual.count("z") - source.count("z")}
                except (OSError, ValueError, RuntimeError, KeyError, IndexError) as diagnostic_error:
                    diagnostics["failureRecoveryError"] = type(diagnostic_error).__name__
            print(json.dumps({"case": case, "fileName": file_name, "mode": mode, "wrap": wrapped, "repetition": repetition, "sourceBytes": len(source.encode()),
                              "failedPhase": phase, "failure": type(error).__name__, **observed, **diagnostics}), flush=True)
            raise
        finally:
            try:
                if samples:
                    samples.finish()
            finally:
                try:
                    if browser:
                        browser.stop()
                finally:
                    runtime.stop()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", nargs="+", choices=["small", "medium", "byte-limit", "line-limit", "long-line", "styled-long-line", "styled-long-line-following-row"], default=["small"])
    parser.add_argument("--modes", nargs="+", choices=["local", "remote"], default=["local", "remote"])
    parser.add_argument("--wrap", action="store_true")
    parser.add_argument("--trace", action="store_true", help="Record bounded probe/worker diagnostics; timings include instrumentation overhead")
    parser.add_argument("--profile", action="store_true", help="Capture a diagnostic main-thread CPU profile; timings and memory include profiler overhead")
    parser.add_argument("--input-position", choices=["start", "beginning", "end"], default="start", help="Use native-window start or verified complete-source Home/End navigation before typing")
    parser.add_argument("--file-name", default="measure.rs", help="Fixture filename selects the production language policy")
    parser.add_argument("--repeat", type=int, default=1, help="Fresh browser/runtime runs for each case and mode")
    parser.add_argument("--require-pss", action="store_true", help="Fail if apportioned Chrome process memory cannot be measured")
    args = parser.parse_args()
    if args.repeat < 1:
        parser.error("--repeat must be positive")
    if Path(args.file_name).name != args.file_name or args.file_name in {"", ".", ".."}:
        parser.error("--file-name must be a single filename")
    assert os.environ.get("CHROMEDRIVER"), "Set CHROMEDRIVER to a compatible driver"
    print(json.dumps({"host": platform.platform(), "measurement": "production editor; process-tree memory",
                      **host_constraints(),
                      "repetitions": args.repeat, "fileName": args.file_name,
                      "measurementImage": os.environ.get("EDITOR_VIEW_IMAGE"),
                      "inputTimingStart": "beforeinput", "inputPosition": args.input_position, "cpuProfiling": args.profile,
                      "frontendWasmSha256": {
                          path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                          for path in sorted((ROOT / "frontend/dist").glob("*.wasm"))
                      },
                      "backendWasmSha256": hashlib.sha256(
                          (ROOT / "target/wasm32-wasip2/release/openwebide_backend.wasm").read_bytes()
                      ).hexdigest(),
                      "measurementScriptSha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                      "checkoutHead": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
                      "browser": subprocess.check_output([os.environ["CHROME"], "--version"], text=True).strip() if os.environ.get("CHROME") else "WebDriver default"}), flush=True)
    for case in args.cases:
        for mode in args.modes:
            for repetition in range(1, args.repeat + 1):
                result = measure(case, mode, args.wrap, args.trace, repetition, args.input_position, args.profile, args.file_name)
                print(json.dumps(result), flush=True)
                if args.require_pss:
                    assert result["peakChromePssKiB"] is not None, "Chrome peak PSS was unavailable"
                    assert result["afterInputMemory"]["chrome_pss_kib"] is not None, "Chrome final PSS was unavailable"
