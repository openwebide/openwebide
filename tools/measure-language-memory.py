#!/usr/bin/env python3
"""Measure owned native/browser RSS and retained JS/WASM during language handoff.

Build production backend/frontend and the release bridge with compiled bundled
defaults first. Run natural and post-gc modes separately with identical workloads.
Local folders use the existing OPFS/native correspondence, not device permissions.
"""
import argparse
import os
import platform
import threading
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import signal
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
TOOLS = ROOT / 'tools'
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--samples',type=int,default=16)
parser.add_argument('--modules',type=int,default=512)
parser.add_argument('--collection',choices=['natural','post-gc'],required=True)
parser.add_argument('--idle-snapshots',type=int,default=0,
                    help='Additional post-GC observations after each mode, without issuing UI queries')
parser.add_argument('--output',type=Path,required=True,help='New directory for measurements and source fingerprints')
args=parser.parse_args()
assert platform.system() in ('Darwin','Linux'),'The RSS sampler requires macOS or Linux ps'
assert 2<=args.samples<=32,'Use 2 through 32 repeated samples per mode'
assert 0<=args.modules<=2048,'Use 0 through 2048 dependency modules'
assert 0<=args.idle_snapshots<=64,'Use 0 through 64 idle observations per mode'
assert not args.idle_snapshots or args.collection=='post-gc','Idle observations require post-gc collection'
TASK=args.output.resolve()
TASK.mkdir(parents=True,exist_ok=False)
(TASK/'probe-source').mkdir()
for source in (Path(__file__), TOOLS/'collect-chrome-memory.mjs',TOOLS/'language_memory_calibration.py'):
    (TASK/'probe-source'/source.name).write_bytes(source.read_bytes())
target=Path(os.environ.get('CARGO_TARGET_DIR',ROOT/'target')).expanduser().resolve()
artifacts=[target/'release/openwebide-bridge',ROOT/'target/wasm32-wasip2/release/openwebide_backend.wasm',
           *sorted(path for path in (ROOT/'frontend/dist').rglob('*') if path.is_file())]
assert len(artifacts)>2,'Build the production frontend assets first'
assert all(path.is_file() for path in artifacts),'Build production backend and bridge first'
ledger={str(path):hashlib.sha256(path.read_bytes()).hexdigest() for path in artifacts}
def artifacts_match():
    return all(Path(path).is_file() and hashlib.sha256(Path(path).read_bytes()).hexdigest()==sha for path,sha in ledger.items())
sys.path.insert(0,str(ROOT/'tools'))
spec=importlib.util.spec_from_file_location('retained_handoff',ROOT/'tools/measure-language-handoff-client.py')
module=importlib.util.module_from_spec(spec);sys.modules[spec.name]=module;spec.loader.exec_module(module)
module.OBSERVE += r"""
window.handoffMemory={memory:null,started:0};
addEventListener('TrunkApplicationStarted',event=>{
 handoffMemory.started++;
 handoffMemory.memory=event.detail?.wasm?.memory??null;
});
"""
BaseBrowser=module.Browser
class ResourceBrowser(BaseBrowser):
    def call(self, method, path, body=None, session=True, timeout=30):
        value=super().call(method,path,body,session,timeout)
        if method=='POST' and path=='/session' and not session:
            self.resource_capabilities=value['capabilities']
        return value
module.Browser=ResourceBrowser
original_prepare=module.prepare_browser
context={}
samples=[]
rss_samples=[]
sampler_errors=[]
sampler_stop=threading.Event()
sampler_thread=None
sampler_roots=None
browser_reported_pids=set()
client=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def prepare(runtime,host,state):
    browser=original_prepare(runtime,host,state)
    try:
        return observe_browser(browser,runtime,host)
    except BaseException:
        browser.stop()
        raise

def observe_browser(browser,runtime,host):
    context.update(runtime=runtime,host=host,mode=None)
    original_request=runtime.request
    def request(method,path,body=None,expected=200,authenticated=True):
        result=original_request(method,path,body,expected,authenticated)
        if method=='POST' and path=='/api/projects':
            context['mode']=body['mode'];context['project']=result['id']
        return result
    runtime.request=request
    address=browser.resource_capabilities['goog:chromeOptions']['debuggerAddress']
    with client.open('http://'+address+'/json/version',timeout=5) as response:
        browser.resource_websocket=json.load(response)['webSocketDebuggerUrl']
    with client.open('http://'+address+'/json/protocol',timeout=5) as response:
        protocol=json.load(response)
    commands={(domain['domain'],command['name']) for domain in protocol['domains'] for command in domain.get('commands',[])}
    assert {('Runtime','getIsolateId'),('Runtime','getHeapUsage'),('Runtime','queryObjects'),('HeapProfiler','collectGarbage'),('SystemInfo','getProcessInfo')} <= commands
    global sampler_thread,sampler_roots,browser_reported_pids
    info=subprocess.run(['node',str(TOOLS/'collect-chrome-memory.mjs'),browser.resource_websocket,'--processes-only'],capture_output=True,text=True,timeout=30)
    if info.returncode:
        raise RuntimeError('Owned Chrome process discovery failed: '+info.stderr[:800])
    browser_reported_pids.update(int(p['id']) for p in json.loads(info.stdout)['processes'])
    sampler_roots={'spin':runtime.process.pid,'bridge':host.process.pid,'chromedriver':browser.process.pid}
    sampler_thread=threading.Thread(target=sample_residency,daemon=True)
    sampler_thread.start()
    return browser
module.prepare_browser=prepare

def process_rows():
    result={}
    for line in subprocess.check_output(['ps','-axo','pid=,ppid=,rss=,comm='],text=True).splitlines():
        fields=line.strip().split(None,3)
        if len(fields)==4:
            pid,parent,kib,comm=fields;result[int(pid)]=(int(parent),int(kib)*1024,comm)
    return result

def tree(rows,root):
    assert root in rows
    owned={root};frontier={root}
    while frontier:
        found={pid for pid,(parent,_,_) in rows.items() if parent in frontier}-owned
        owned.update(found);frontier=found
    return owned

def sample_residency():
    while not sampler_stop.is_set():
        started=time.monotonic()
        try:
            rows=process_rows()
            groups={name:tree(rows,pid) for name,pid in sampler_roots.items() if pid in rows}
            if len(groups)!=len(sampler_roots):
                # Base fixture performs cleanup before it returns to this wrapper.
                if not any(pid in rows for pid in sampler_roots.values()):return
            union=set().union(*groups.values())|(browser_reported_pids & rows.keys())
            rss_samples.append({'time':started,'mode':context.get('mode'),'groups':{
                name:{'resident_bytes':sum(rows[p][1] for p in ids),'pids':sorted(ids)} for name,ids in groups.items()},
                'union_resident_bytes':sum(rows[p][1] for p in union),
                'read_seconds':time.monotonic()-started})
        except BaseException as error:
            sampler_errors.append(str(error));return
        sampler_stop.wait(max(0,.25-(time.monotonic()-started)))

def memory(browser,reason='reload-post-gc',expected=None):
    if args.collection=='natural':
        return {'collection':'natural-no-forced-gc','last_rss_index':len(rss_samples)-1}

    result=subprocess.run(['node',str(TOOLS/'collect-chrome-memory.mjs'),browser.resource_websocket],capture_output=True,text=True,timeout=30)
    if result.returncode:
        raise RuntimeError('Owned CDP collection failed: '+result.stderr[:800])
    cdp=json.loads(result.stdout)
    browser_reported_pids.update(int(p['id']) for p in cdp['processes'])
    app_pages=[target for target in cdp['targets'] if target['type']=='page' and target['url'].startswith(context['runtime'].url+'/')]
    assert app_pages and all(t['wasm'] and t['wasm']['started']==1 and t['wasm']['export_is_memory'] and t['wasm']['linear_bytes']>0 for t in app_pages)
    direct=sum(t['wasm']['linear_bytes'] for t in app_pages)
    assert cdp['shared_wasm_memories']==0,'Shared WASM requires backing-store identity accounting'
    assert cdp['unshared_wasm_linear_bytes']>=direct,'Direct memory enumeration omitted a main-page WASM memory'
    rows=process_rows()
    roots={'spin':context['runtime'].process.pid,'bridge':context['host'].process.pid,'chromedriver':browser.process.pid}
    groups={name:tree(rows,pid) for name,pid in roots.items()}
    browser_pids={int(p['id']) for p in cdp['processes']}|groups['chromedriver']
    assert all(pid in rows for pid in browser_pids),'Browser process exited during residency snapshot'
    groups['browser_reported_and_driver']=browser_pids
    all_pids=groups['spin']|groups['bridge']|browser_pids
    record={'mode':context['mode'],'project':context['project'],'reason':reason,'expected':expected,'time':time.monotonic(),
        'cdp':cdp,'process_groups':{name:{'pids':sorted(ids),'resident_bytes':sum(rows[p][1] for p in ids)} for name,ids in groups.items()},
        'owned_process_union_resident_bytes':sum(rows[p][1] for p in all_pids),
        'owned_processes':[{'pid':pid,'parent_pid':rows[pid][0],'resident_bytes':rows[pid][1],'program':rows[pid][2]} for pid in sorted(all_pids)]}
    samples.append(record)
    (TASK/'memory-samples.json').write_text(json.dumps(samples,indent=2)+'\n')
    print(json.dumps({'phase':'retained-memory','mode':context['mode'],'reason':reason,'samples':len(samples),
       'isolates':cdp['unique_isolates'],'targets':len(cdp['targets']),'wasm_pages':[t['wasm']['linear_bytes'] for t in app_pages]}),flush=True)
    return {'complete_probe_snapshot_index':len(samples)-1,**cdp['per_isolate_totals']}
module.memory=memory

# The shared workflow calls this only after it has checked and copied every
# response/retry needed by the current query. Keep trace evidence bounded so the
# measurement harness does not itself create apparent application retention.
def observe_query(browser,phase,index):
    module.script(browser,'handoffEvidence.requests.length=0;handoffEvidence.hover_retries.length=0;')
    memory(browser,phase,index)
    if phase=='settled':
        for idle_index in range(args.idle_snapshots):
            memory(browser,'idle-post-gc',idle_index)
            # Background application queries, if any, make this an observation
            # of passive activity rather than collector-only overhead.
            events=module.script(browser,'const count=handoffEvidence.requests.length;handoffEvidence.requests.length=0;return count;')
            samples[-1]['background_query_events']=events
        if args.idle_snapshots:
            (TASK/'memory-samples.json').write_text(json.dumps(samples,indent=2)+'\n')
module.memory=lambda browser:{'collection':args.collection,'complete_observation':'memory-samples.json or residency-samples.json'}
provenance={'status':'measurement_instrumentation',
    'checkout_commit':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT,text=True).strip(),
    'checkout_dirty':bool(subprocess.check_output(['git','status','--porcelain','--untracked-files=no'],cwd=ROOT,text=True).strip()),
    'runtime_artifact_sha256':ledger,
    'fixture_script_sha256':hashlib.sha256((ROOT/'tools/measure-language-handoff-client.py').read_bytes()).hexdigest(),
    'calibration_script_sha256':hashlib.sha256((TOOLS/'language_memory_calibration.py').read_bytes()).hexdigest(),
    'node_collector_sha256':hashlib.sha256((TOOLS/'collect-chrome-memory.mjs').read_bytes()).hexdigest(),
    'collection':args.collection,'query_trace_retention':'drained only after all current query assertions and report copies',
    'idle_snapshots_after_each_mode':args.idle_snapshots,
    'samples_per_mode':args.samples,'dependency_modules':args.modules,
    'limits':['Finite repeated reload/handoff workload cannot establish stability for all editing or active-command workloads.',
              'Artifact fingerprints identify measured bytes; the checkout revision alone does not establish their producer commit.',
              'Whole-process RSS sums include shared pages in each process; this is not unique physical memory or live allocator heap.',
              'WASM linear capacity is not live Rust allocation size. Shared WASM backing-store deduplication is not supported.',
              'Forced collection and inspector attachment add overhead in post-gc mode; natural mode does not force GC. Neither establishes latency or prefetch benefit.',
              '250ms RSS sampling can miss short peaks; sampling starts after authenticated browser preparation and excludes runtime startup.',
              'Local-stage bridge RSS includes still-idle Remote services. Browser tooling is measured separately from app/server processes.']}
(TASK/'provenance.json').write_text(json.dumps(provenance,indent=2)+'\n')
print(json.dumps({'task':str(TASK),'checkout_commit':provenance['checkout_commit'],'collection':args.collection}),flush=True)
def interrupted(*_):
    raise KeyboardInterrupt('Owned language-memory probe interrupted or exceeded its deadline')
signal.signal(signal.SIGTERM,interrupted)
signal.signal(signal.SIGINT,interrupted)
signal.signal(signal.SIGALRM,interrupted)
signal.alarm(1200)
started=time.monotonic()
try:
    if args.collection=='post-gc':
        from language_memory_calibration import calibrate
        calibrate(module.Browser,TOOLS/'collect-chrome-memory.mjs',TASK)
    module.measure(args.samples,args.modules,TASK/'handoff.json',query_observer=observe_query)
    assert artifacts_match()
    assert rss_samples and {row['mode'] for row in rss_samples if row['mode']}=={'local','remote'}
    assert not sampler_errors,sampler_errors
    if args.collection=='post-gc':assert samples and {row['mode'] for row in samples}=={'local','remote'}
finally:
    sampler_stop.set()
    if sampler_thread:sampler_thread.join(timeout=5)
    (TASK/'residency-samples.json').write_text(json.dumps(rss_samples,indent=2)+'\n')
    (TASK/'sampler-errors.json').write_text(json.dumps(sampler_errors)+'\n')
    signal.alarm(0)
    assert artifacts_match(),'Measured runtime artifact bytes changed during measurement'
    assert not sampler_thread or not sampler_thread.is_alive(),'Residency sampler did not stop'

(TASK/'result.json').write_text(json.dumps({'status':'measurement_passed','seconds':time.monotonic()-started,'memory_snapshots':len(samples),'residency_snapshots':len(rss_samples),'provenance':provenance},indent=2)+'\n')
