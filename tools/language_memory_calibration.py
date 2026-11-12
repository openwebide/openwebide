"""Validate Chrome memory counter semantics with a disposable dedicated worker."""
import hashlib
import json
import subprocess
import tempfile
import urllib.request


def calibrate(browser_type, collector, output):
    with tempfile.TemporaryDirectory(prefix='openwebide-memory-calibration-') as state:
        browser=browser_type(state)
        try:
            browser.call('POST','/url',{'url':'data:text/html,<title>Owned memory calibration</title>'})
            address=browser.resource_capabilities['goog:chromeOptions']['debuggerAddress']
            client=urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with client.open('http://'+address+'/json/version',timeout=5) as response:ws=json.load(response)['webSocketDebuggerUrl']
            observations=[]
            def sample(phase):
                child=subprocess.run(['node',str(collector),ws],capture_output=True,text=True,timeout=30)
                if child.returncode:raise RuntimeError(child.stderr[:800])
                result=json.loads(child.stdout);observations.append({'phase':phase,**result})
                (output/'observations.json').write_text(json.dumps(observations,indent=2)+'\n')
                workers=[target for target in result['targets'] if target['type']=='worker']
                usages=[iso['usage'] for iso in result['isolates'] if any(t['isolate']==iso['id'] for t in workers)]
                print(json.dumps({'phase':phase,'workers':len(workers),'worker_usage':usages}),flush=True)
                return usages
            sample('empty-page')
            ready=browser.call('POST','/execute/async',{'script':r'''
    const done=arguments[arguments.length-1];
    const source=`globalThis.plain=new ArrayBuffer(4*1024*1024);new Uint8Array(plain).fill(7);
    self.onmessage=event=>{
     if(event.data==='wasm'){globalThis.wasm=new WebAssembly.Memory({initial:128});new Uint8Array(wasm.buffer).fill(9);postMessage({phase:'wasm',bytes:wasm.buffer.byteLength});}
     if(event.data==='release'){globalThis.plain=null;globalThis.wasm=null;postMessage({phase:'release'});}
    };postMessage({phase:'plain',bytes:plain.byteLength});`;
    window.calibrationWorker=new Worker(URL.createObjectURL(new Blob([source],{type:'text/javascript'})));
    calibrationWorker.onmessage=event=>done(event.data);
    calibrationWorker.onerror=event=>done({error:event.message});
    ''','args':[]})
            assert ready=={'phase':'plain','bytes':4*1024**2},ready
            plain=sample('worker-plain-4MiB');assert len(plain)==1
            ready=browser.call('POST','/execute/async',{'script':"const done=arguments[arguments.length-1];calibrationWorker.onmessage=e=>done(e.data);calibrationWorker.postMessage('wasm');",'args':[]})
            assert ready=={'phase':'wasm','bytes':8*1024**2},ready
            wasm=sample('worker-plain-plus-wasm-12MiB');assert len(wasm)==1
            ready=browser.call('POST','/execute/async',{'script':"const done=arguments[arguments.length-1];calibrationWorker.onmessage=e=>done(e.data);calibrationWorker.postMessage('release');",'args':[]})
            assert ready=={'phase':'release'},ready
            released=sample('worker-released-post-gc');assert len(released)==1
            direct={}
            for item in observations:
                worker_isolates={t['isolate'] for t in item['targets'] if t['type']=='worker'}
                direct[item['phase']]=sum(m['linear_bytes'] for realm in item['wasm_realms'] if realm['isolate'] in worker_isolates for m in realm['memories'] if not m['shared'])
            assert direct=={'empty-page':0,'worker-plain-4MiB':0,'worker-plain-plus-wasm-12MiB':8*1024**2,'worker-released-post-gc':0},direct
            delta=wasm[0]['backingStorageSize']-plain[0]['backingStorageSize']
            report={'status':'calibration_completed','wasm_allocation_bytes':8*1024**2,'plain_allocation_bytes':4*1024**2,
                'plain_usage':plain[0],'wasm_usage':wasm[0],'released_usage':released[0],'observed_wasm_backing_storage_delta':delta,
                'wasm_included_in_backing_storage':delta>=8*1024**2,'direct_worker_wasm_bytes_by_phase':direct,
                'collector_sha256':hashlib.sha256(collector.read_bytes()).hexdigest(),
                'limits':['Synthetic owned worker validates collector semantics only, not application memory stability.','Main-page WASM and worker V8 isolates remain separately observable.']}
            (output/'calibration.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report),flush=True)
        finally:
            browser.stop()
