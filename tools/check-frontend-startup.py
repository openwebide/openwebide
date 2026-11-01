#!/usr/bin/env python3
"""Verify the built startup loader, integrity failures and cached worker paths in Chrome."""

from pathlib import Path
import functools
import http.server
import json
import runpy
import tempfile
import threading

ROOT = Path(__file__).resolve().parent.parent
Browser = runpy.run_path(str(ROOT / "tools/check-editor-recovery.py"))["Browser"]
dist = ROOT / "frontend/dist"


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_GET(self):
        if self.server.fault and self.path.startswith("/frontend-startup-"):
            if self.server.fault == "missing":
                self.send_error(404)
                return
            body = b"changed startup asset"
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if self.server.raw and (
            self.path == "/service-worker.js" or self.path.startswith("/editor-worker")
        ):
            source = (dist / self.path.removeprefix("/")).read_bytes()
            body = b"globalThis.DecompressionStream=undefined;\n" + source
            self.send_response(200)
            self.send_header("Content-Type", "text/javascript")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        super().do_GET()


def run(metadata, raw=False, fault=None):
    assets = {"js": metadata["bindings"]["file"], "wasm": metadata["wasm"]["file"]}
    server = http.server.ThreadingHTTPServer(
        ("127.0.0.1", 0), functools.partial(Handler, directory=str(dist))
    )
    server.raw = raw
    server.fault = fault
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory() as folder:
            browser = Browser(folder)
            try:
                if raw:
                    browser.call(
                        "POST",
                        "/goog/cdp/execute",
                        {
                            "cmd": "Page.addScriptToEvaluateOnNewDocument",
                            "params": {
                                "source": "Object.defineProperty(globalThis,'DecompressionStream',{value:undefined});"
                            },
                        },
                    )
                browser.call(
                    "POST",
                    "/url",
                    {"url": f"http://127.0.0.1:{server.server_port}/offline.html"},
                )
                if not fault:
                    ready = browser.call(
                        "POST",
                        "/execute/async",
                        {
                            "script": """
      const done=arguments[0];(async()=>{
       await navigator.serviceWorker.register('/service-worker.js');await navigator.serviceWorker.ready;
       if(!navigator.serviceWorker.controller)await new Promise((resolve,reject)=>{const t=setTimeout(()=>reject(Error('No active controller')),10000);navigator.serviceWorker.addEventListener('controllerchange',()=>{clearTimeout(t);resolve();},{once:true});});
       done({controlled:!!navigator.serviceWorker.controller});
      })().catch(e=>done({error:String(e)}));""",
                            "args": [],
                        },
                    )
                    assert ready.get("controlled"), ready
                    worker_source = r"""
const compiled = new Promise(resolve => self.addEventListener('message', event => resolve(event.data), {once:true}));
const {default:init, initSync} = await import(__BUNDLE__);
const module = await compiled;
const mode = __MODE__;
let same = true;
if (mode === 'sync') {
    initSync({module});
} else if (mode === 'concurrent') {
    const [a,b] = await Promise.all([init({module_or_path:module}), init({module_or_path:module})]);
    same = a === b;
} else if (mode === 'mixed') {
    const pending = init({module_or_path:module});
    const current = initSync({module});
    same = await pending === current;
} else {
    const original = self.fetch.bind(self);
    let fail = true;
    self.fetch = (...args) => {
        if (fail) { fail = false; return Promise.reject(new Error('Transient WASM load failure')); }
        return original(...args);
    };
    let failed;
    try { failed = await Promise.allSettled([init(), init()]); }
    finally { self.fetch = original; }
    same = failed.every(result => result.status === 'rejected');
    await init({module_or_path:module});
}
self.postMessage({startupContract:true, same});
"""
                    for mode in ["sync", "concurrent", "mixed", "retry"]:
                        source = (
                            "globalThis.DecompressionStream=undefined;\n" if raw else ""
                        ) + worker_source
                        source = source.replace("__MODE__", json.dumps(mode)).replace(
                            "__BUNDLE__",
                            "new URL("
                            + json.dumps("/" + assets["js"])
                            + ", self.location.origin).href",
                        )
                        worker_script = """
const done = arguments[1];
(async () => {
    const module = await WebAssembly.compileStreaming(fetch(__WASM__));
    const url = URL.createObjectURL(new Blob([arguments[0]], {type:'text/javascript'}));
    try {
        await new Promise((resolve,reject) => {
            const worker = new Worker(url, {type:'module'});
            let ready = 0, verified = false;
            const finish = error => {clearTimeout(timer);worker.terminate();error ? reject(error) : resolve();};
            const timer = setTimeout(() => finish(Error('Worker initialization contract timed out')), 10000);
            worker.onmessage = event => {
                if (event.data === 'openwebide-editor-ready:6:frames:1') ready++;
                if (event.data?.startupContract) {
                    if (!event.data.same || ready !== 1) return finish(Error('Initialization did not publish one instance'));
                    verified = true;
                }
                if (verified && ready === 1) finish();
            };
            worker.onerror = event => finish(Error(event.message));
            worker.postMessage(module);
        });
        done({verified:true});
    } finally {URL.revokeObjectURL(url);}
})().catch(error => done({error:String(error)}));
""".replace("__WASM__", json.dumps("/" + assets["wasm"]))
                        verified = browser.call(
                            "POST",
                            "/execute/async",
                            {"script": worker_script, "args": [source]},
                        )
                        assert verified.get("verified"), (mode, verified)
                    browser.call(
                        "POST",
                        "/goog/cdp/execute",
                        {"cmd": "Network.enable", "params": {}},
                    )
                    browser.call(
                        "POST",
                        "/goog/cdp/execute",
                        {
                            "cmd": "Network.emulateNetworkConditions",
                            "params": {
                                "offline": True,
                                "latency": 0,
                                "downloadThroughput": 0,
                                "uploadThroughput": 0,
                            },
                        },
                    )
                script = (
                    """
     const done=arguments[0];(async()=>{
      const bindings=await import(JS_URL);const wasm=await bindings.default();
      if(FAULT)return done({initialized:true});
      const workerReady=await new Promise((resolve,reject)=>{
       const worker=new Worker('/editor-worker.js',{type:'module'});window.startupContractWorker=worker;
       const timeout=setTimeout(()=>reject(Error('Offline worker readiness timed out')),10000);
       worker.onmessage=e=>{if(e.data==='openwebide-editor-ready:6:frames:1'){clearTimeout(timeout);resolve(true);}};
       worker.onerror=e=>{clearTimeout(timeout);reject(Error(e.message));};
      });
      const keys=await caches.keys();const cache=await caches.open(keys.find(k=>k.startsWith('openwebide-shell-')));
      const paths=(await cache.keys()).map(r=>new URL(r.url).pathname);
      done({initialized:true,workerReady,linearBytes:wasm.memory.buffer.byteLength,startupAssets:paths.filter(p=>p.startsWith('/frontend-startup-'))});
     })().catch(e=>done({initialized:false,errorName:e.name,error:String(e)}));
    """.replace("JS_URL", json.dumps("/" + assets["js"]))
                    .replace("WASM_URL", json.dumps("/" + assets["wasm"]))
                    .replace("FAULT", json.dumps(bool(fault)))
                )
                result = browser.call(
                    "POST", "/execute/async", {"script": script, "args": []}
                )
                if fault:
                    assert not result["initialized"] and result.get("errorName") in [
                        "TypeError",
                        "Error",
                    ], result
                else:
                    assert result.get("initialized") and result.get("workerReady"), (
                        result
                    )
                    expected = "/" + metadata["raw" if raw else "gzip"]["file"]
                    assert result["startupAssets"] == [expected], (expected, result)
                print(
                    json.dumps(
                        {
                            "rawCapability": raw,
                            "fault": fault,
                            "result": result,
                            **(
                                {
                                    "workerInitializationContracts": [
                                        "sync",
                                        "concurrent",
                                        "mixed",
                                        "retry",
                                    ]
                                }
                                if not fault
                                else {}
                            ),
                        }
                    ),
                    flush=True,
                )
            finally:
                browser.stop()
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    metadata = runpy.run_path(str(ROOT / "tools/prepare-frontend-startup.py"))[
        "read_metadata"
    ](dist)
    assert metadata, "Build the frontend with startup data first"
    for raw in [False, True]:
        for fault in ["missing", "corrupt", None]:
            run(metadata, raw, fault)
