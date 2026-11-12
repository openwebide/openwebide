#!/usr/bin/env python3
"""Measure real production reload/handoff with the installed TypeScript server.

Run after building current production assets and the bundled release bridge.
Local mode uses OPFS plus the existing native marker mirror, not physical folder
permission prompts. Latencies include WebDriver polling and UI scheduling. RSS
is sampled process residency, not retained allocator heap; CDP reports JS heap,
not the frontend's complete WASM/browser footprint. No result cache is enabled.
"""
import argparse
import base64
import hashlib
import json
import math
import os
from pathlib import Path
import runpy
import secrets
import signal
import statistics
import platform
import subprocess
import sys
import tempfile
import threading
import time

from language_host_support import ExecutionHost
from language_process_observation import process_residency, service_tree
from language_server_client_support import ProbeMirror, caret, close, insert, local_handle, script, wait

ROOT = Path(__file__).resolve().parent.parent
SUPPORT = runpy.run_path(str(ROOT / 'tools/check-editor-recovery.py'))
Runtime, Browser = SUPPORT['Runtime'], SUPPORT['Browser']
OBSERVE = r'''
window.handoffEvidence={requests:[],errors:[],hover_retries:[]};
addEventListener('error',e=>handoffEvidence.errors.push(String(e.message)));
addEventListener('unhandledrejection',e=>handoffEvidence.errors.push(String(e.reason)));
const originalFetch=window.fetch;
window.fetch=function(...args){
 const url=String(args[0]?.url??args[0]), start=performance.now();
 return originalFetch.apply(this,args).then(async response=>{
  if(/\/plugins\/languages\/query$/.test(new URL(url,location.href).pathname)){
   const event={status:response.status,start,end:performance.now()};
   try{const data=await response.clone().json();event.server=data.server;event.ready=data.diagnostics_ready;event.error=data.error;}catch{}
   handoffEvidence.requests.push(event);
  }
  return response;
 });
};
'''


class Residency:
    """Read only the disposable bridge and its observed descendants, every 250ms."""
    def __init__(self, pid):
        self.pid, self.samples, self.errors = pid, [], []
        self.stopping = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def sample(self):
        rows = {}
        output = subprocess.check_output(['ps', '-axo', 'pid=,ppid=,rss=,comm='], text=True)
        for line in output.splitlines():
            fields = line.strip().split(None, 3)
            if len(fields) == 4:
                pid, parent, kib, name = fields
                rows[int(pid)] = (int(parent), int(kib) * 1024, name)
        record = process_residency(rows, self.pid, time.monotonic())
        self.samples.append(record)
        return record

    def run(self):
        while not self.stopping.is_set():
            try:
                self.sample()
            except (OSError, ValueError, AssertionError) as error:
                self.errors.append(str(error))
            self.stopping.wait(.25)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stopping.set()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive(), 'Residency sampler did not stop'


def prepare_browser(runtime, host, state):
    browser = Browser(state)
    try:
        browser.call('POST', '/url', {'url': runtime.url + '/offline.html'})
        for cookie in runtime.cookies:
            browser.call('POST', '/cookie', {'cookie': {'name': cookie.name, 'value': cookie.value,
                'domain': '127.0.0.1', 'path': cookie.path, 'httpOnly': True}})
        host.pair_browser(browser)
        browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.addScriptToEvaluateOnNewDocument',
                                               'params': {'source': OBSERVE}})
        return browser
    except BaseException:
        browser.stop()
        raise


def hover(browser, expected):
    caret(browser, 'answer'); start = time.monotonic()
    close(browser)
    event_start = script(browser, 'return handoffEvidence.requests.length;')
    def request():
        return script(browser, '''
            const item=[...document.querySelectorAll('button')]
                .find(button=>button.textContent.trim()==='Symbol information'&&!button.disabled);
            if(item){item.click();return true;}
            const control=document.querySelector('button[aria-label="Code intelligence"]');
            if(control&&!control.disabled)control.click();
            return false;
        ''')
    wait('stable Symbol information menu',request)
    retried, retry_pending = set(), False
    def result():
        nonlocal retry_pending
        state = script(browser, '''
            return {ready:document.querySelector('[role=dialog]')?.textContent.includes(arguments[0]),
                changed:handoffEvidence.requests.slice(arguments[1]).filter(event=>event.error===
                    'Project files changed during language analysis; retry with the current project').at(-1)?.end};
        ''', expected, event_start)
        if state.get('ready'):
            return True
        changed = state.get('changed')
        if changed is not None and changed not in retried:
            retried.add(changed)
            assert len(retried) <= 3, 'Project analysis did not settle after three hover retries'
            script(browser, 'handoffEvidence.hover_retries.push({time:performance.now(),rejected:arguments[0]});', changed)
            retry_pending = True
        if retry_pending:
            retry_pending = not request()
        return False
    wait('exact draft hover ' + expected, result, 90)
    elapsed = (time.monotonic() - start) * 1000
    close(browser)
    return elapsed


def memory(browser):
    browser.call('POST', '/goog/cdp/execute', {'cmd': 'HeapProfiler.collectGarbage', 'params': {}})
    result = browser.call('POST', '/goog/cdp/execute', {'cmd': 'Runtime.getHeapUsage', 'params': {}})
    return {key: result[key] for key in ('usedSize', 'totalSize', 'embedderHeapUsedSize', 'backingStorageSize') if key in result}


def distribution(values):
    ordered = sorted(values)
    return {'samples': len(values), 'minimum_ms': min(values), 'median_ms': statistics.median(ordered),
            'p95_ms': ordered[math.ceil(.95 * len(ordered))-1], 'maximum_ms': max(values)}


def process_transitions(records):
    observed={'same_single_root':0,'changed_single_root':0,'overlapping_or_missing_roots':0}
    for previous,current in zip(records,records[1:]):
        before,after=set(previous['mode_server_pids']),set(current['mode_server_pids'])
        key=('same_single_root' if before==after else 'changed_single_root') if len(before)==len(after)==1 else 'overlapping_or_missing_roots'
        observed[key]+=1
    return observed


def pressure_recovery(browser, environment, residency, record, previous_children, first, second):
    """Same production recovery contract for each mode and both live clients."""
    before = residency.sample()
    roots = set(before['server_pids']) - previous_children
    tree = service_tree(before, roots, residency.pid)
    observed = {'before': before, 'original_tree_pids': sorted(tree), 'clients': []}
    record['pressure_recovery'] = observed
    frozen = []
    try:
        # Prevent periodic synchronization from restarting a service while the
        # allocator checks eviction. Freezing the pages does not alter the host.
        for handle in (first, second):
            browser.call('POST', '/window', {'handle': handle})
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.setWebLifecycleState', 'params': {'state': 'frozen'}})
            frozen.append(handle)
        observed['pressure'] = environment.evict_under_pressure(tree, residency)
        # Preserve the app's actual five-second pressure retry backoff.
        time.sleep(5.25)
    finally:
        for handle in frozen:
            browser.call('POST', '/window', {'handle': handle})
            browser.call('POST', '/goog/cdp/execute', {'cmd': 'Page.setWebLifecycleState', 'params': {'state': 'active'}})
    old_services = {item['server'] for item in record['handoffs']}
    for handle, expected in ((first, 'string'), (second, 'boolean')):
        browser.call('POST', '/window', {'handle': handle})
        event_index = script(browser, 'return handoffEvidence.requests.length;')
        elapsed = hover(browser, expected)
        events = script(browser, 'return handoffEvidence.requests.slice(arguments[0]);', event_index)
        services = {event['server'] for event in events if event.get('server')}
        assert services and not services.intersection(old_services), 'Pressure recovery retained an expired service identity'
        sample = residency.sample()
        current = set(environment.processes())
        assert not tree.intersection(current), 'The original process tree survived pressure recovery'
        assert set(sample['server_pids']) - previous_children, 'No replacement server was observed'
        observed['clients'].append({'client': 'first' if handle == first else 'second', 'expected': expected,
            'hover_ms': elapsed, 'services': sorted(services), 'residency': sample,
            'container_process_pids': sorted(current)})


def measure(samples, modules, output, environment_factory=None, exercise_pressure=False,
            query_observer=None):
    report = {'format': 2, 'status': 'running', 'platform': {'os':os.uname().sysname,'release':platform.release(),'machine':platform.machine(),'cpu_count':os.cpu_count()},
              'samples_per_mode': samples, 'dependency_modules': modules, 'modes': [], 'limits': [
                  'Generated TypeScript dependency graph; not a whole-app 2CPU/2GiB Linux benchmark.',
                  'Local browser uses OPFS/native marker correspondence; no physical permissions measured.',
                  'Browser requests run through actual installed plugin, authorization and native LSP transports.',
                  'UI latency includes WebDriver polling; no speed improvement inferred from cold/warm samples.',
                  'RSS includes allocator retention; post-GC CDP JS heap excludes complete WASM/browser memory.',
                  '250ms residency samples can miss short peaks; sampling itself adds overhead.',
                  'Local-stage RSS includes any still-idle Remote server in this shared bridge.',
                  'Each workload has one first-load sample per mode; repeated-run p95 is descriptive, not SLO confidence.',
                  'PID observations describe process continuity and changes; they do not assign a response to one process during overlap.']}
    with tempfile.TemporaryDirectory(prefix='openwebide-handoff-') as state_path, \
            tempfile.TemporaryDirectory(prefix='handoff-project-', dir=ROOT.parent) as project_path:
        state, parent = Path(state_path), Path(project_path)
        environment = environment_factory(state, parent) if environment_factory else None
        if environment is None:
            runtime = Runtime(state / 'spin', frontend=True); runtime.state.mkdir()
            host = ExecutionHost(runtime)
            residency_factory = Residency
        else:
            runtime, host = environment.runtime, environment.host
            residency_factory = environment.residency
            report['runtime_environment'] = environment.metadata
            report['limits'][0] = 'Whole final app image, including Spin, bridge and language servers, under asserted 2 CPU / 2 GiB limits.'
            report['limits'][5] = 'Engine process readings sample every second and at query boundaries; short peaks can be missed. Cgroup peak includes bounded control reads.'
            report['limits'].append('Browser and proof-mirror clients run outside the server cgroup; workspace/state use owned host bind mounts.')
        browser, mirror = None, ProbeMirror(runtime.url)
        second_mirror = ProbeMirror(runtime.url)
        try:
            host.start(); runtime.start()
            runtime.request('POST', '/api/auth/register', {'username': 'handoff-'+secrets.token_hex(6),
                'password': secrets.token_urlsafe(32)}, expected=201)
            runtime.acknowledge_release()
            user = runtime.request('GET', '/api/auth/me')['user']['id']
            installations = host.initialize_defaults()
            installed = next(item for item in installations if item['prepared']['manifest']['name']=='javascript-typescript')
            report['plugin'] = {'name': installed['prepared']['manifest']['name'], 'version': installed['prepared']['manifest']['version']}
            if environment is None:
                report['bridge_sha256'] = hashlib.sha256(host.binary.read_bytes()).hexdigest()
            browser = prepare_browser(runtime, host, state)
            report['browser']=browser.call('POST','/goog/cdp/execute',{'cmd':'Browser.getVersion','params':{}})
            if environment is None:
                report['node_version']=subprocess.check_output(['node','--version'],text=True).strip()
                report['server_version']=subprocess.check_output(['typescript-language-server','--version'],text=True).strip()
                report['artifacts']={str(path.relative_to(ROOT)):hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in [ROOT/'target/wasm32-wasip2/release/openwebide_backend.wasm',*sorted((ROOT/'frontend/dist').glob('*.wasm'))]}
            else:
                report.update(environment.describe())
                report['client_platform']=report['platform']
                report['platform']=environment.metadata['runtime_platform']
            with residency_factory(host.process.pid) as residency:
                for mode in ('remote', 'local'):
                    folder = parent / mode; folder.mkdir()
                    files = {'tsconfig.json': '{"compilerOptions":{"strict":true,"noEmit":true,"types":[]}}',
                             'package.json': '{"type":"module"}',
                             'lib.ts': 'export function answer(): number { return 42; }\r\n',
                             'main.ts': 'import { answer } from "./lib";\r\nexport const value=answer();\r\n'}
                    for index in range(modules):
                        imports = '' if index == 0 else f'import {{ dependency{index-1} }} from "./dependency{index-1}";\n'
                        fields = ','.join(f'field{field}: {index+field}' for field in range(96))
                        files[f'dependency{index}.ts'] = imports + f'export const dependency{index} = {{{fields}}};\n'
                    if modules:
                        files['main.ts'] += f'import {{ dependency{modules-1} }} from "./dependency{modules-1}";\nexport const dependency = dependency{modules-1};\n'
                    for path, text in files.items(): (folder/path).write_bytes(text.encode())
                    subprocess.run(['git','init','--quiet','--initial-branch=main','--template=',str(folder)],check=True)
                    relative = folder.relative_to(ROOT.parent.parent).as_posix()
                    project = runtime.request('POST','/api/projects',{'name':'Handoff '+mode,'mode':mode,
                        'path':relative if mode=='remote' else folder.name},expected=201)
                    if mode=='local':
                        local_handle(browser,project['id'],user,folder.name,files)
                        mirror.install(browser,project['id'],folder)
                    drafts=[]
                    for path in ('lib.ts','main.ts'):
                        source=files[path].replace('number { return 42; }','string { return "draft"; }')
                        drafts.append({'path':path,'document':{'text':base64.b64encode(source.encode()).decode(),
                            'saved':base64.b64encode(files[path].encode()).decode(),'selections':[{'anchor':0,'head':0}],
                            'collapsed':[]},'scroll':{'top':0,'left':0},'read_only':False})
                    runtime.request('PUT',f"/api/projects/{project['id']}/editor-recovery",{'revision':0,
                        'state':{'format':1,'root':{'mode':mode,'path':relative if mode=='remote' else None},
                                 'selected':'main.ts','files':drafts}})
                    for key,value in {'open_tabs':json.dumps([project['id']]),'active_project':str(project['id'])}.items():
                        runtime.request('PUT','/api/settings',{'key':key,'value':value})
                    record={'mode':mode,'files':len(files),'source_bytes':sum(len(text.encode()) for text in files.values()),'reloads':[],'handoffs':[]}
                    report['modes'].append(record)
                    start_index=len(residency.samples)
                    previous_children={child['pid'] for child in residency.sample()['children']}
                    for index in range(samples):
                        print(json.dumps({'phase':'reload','mode':mode,'index':index}),flush=True)
                        start=time.monotonic()
                        browser.call('POST','/url',{'url':runtime.url+'/'})
                        wait('restored editor',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.dataset.editorPath==='main.ts';"))
                        hover_ms=hover(browser,'string')
                        ready_ms=(time.monotonic()-start)*1000
                        events=script(browser,'return handoffEvidence;')
                        assert not events['errors'],events['errors']
                        servers={e['server'] for e in events['requests'] if e.get('server')}
                        assert len(servers)==1,servers
                        sample=residency.sample()
                        record['reloads'].append({'index':index,'ready_ms':ready_ms,'hover_ms':hover_ms,
                            'hover_retries':events['hover_retries'],
                            'server':servers.pop(),'residency':sample,'mode_server_pids':[pid for pid in sample['server_pids'] if pid not in previous_children],'post_gc_js_heap':memory(browser)})
                        if query_observer:
                            query_observer(browser, 'reload', index)
                    # Two genuine page clients alternate the same server while each
                    # keeps a different unsaved overlay. Both windows remain alive.
                    first=browser.call('GET','/window')
                    second=browser.call('POST','/window/new',{'type':'tab'})['handle']
                    browser.call('POST','/window',{'handle':second})
                    browser.call('POST','/goog/cdp/execute',{'cmd':'Page.addScriptToEvaluateOnNewDocument','params':{'source':OBSERVE}})
                    if mode=='local':second_mirror.install(browser,project['id'],folder)
                    browser.call('POST','/url',{'url':runtime.url+'/'})
                    wait('second client restored',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.dataset.editorPath==='main.ts';"))
                    # Change only this page's retained library buffer, never Save.
                    assert script(browser,"const tab=[...document.querySelectorAll('[data-editor-tab]')].find(x=>x.dataset.editorTab==='lib.ts');if(!tab)return false;tab.click();return true;")
                    wait('second library tab',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.dataset.editorPath==='lib.ts';"))
                    caret(browser,'string { return "draft"; }',select=True);insert(browser,'boolean { return true; }')
                    assert script(browser,"const tab=[...document.querySelectorAll('[data-editor-tab]')].find(x=>x.dataset.editorTab==='main.ts');if(!tab)return false;tab.click();return true;")
                    wait('second main tab',lambda:script(browser,"return document.querySelector('textarea[data-editor-path]')?.dataset.editorPath==='main.ts';"))
                    for index in range(samples):
                        for handle,expected in ((second,'boolean'),(first,'string')):
                            print(json.dumps({'phase':'handoff','mode':mode,'index':index,'expected':expected}),flush=True)
                            browser.call('POST','/window',{'handle':handle})
                            before=script(browser,'return handoffEvidence.requests.length;')
                            retries_before=script(browser,'return handoffEvidence.hover_retries.length;')
                            elapsed=hover(browser,expected)
                            events=script(browser,'return handoffEvidence.requests.slice(arguments[0]);',before)
                            servers={e['server'] for e in events if e.get('server')}
                            assert len(servers)==1,servers
                            record['handoffs'].append({'index':index,'client':'second' if handle==second else 'first',
                                'expected':expected,'hover_ms':elapsed,'server':servers.pop(),
                                'hover_retries':script(browser,'return handoffEvidence.hover_retries.slice(arguments[0]);',retries_before),
                                'residency':residency.sample()})
                            record['handoffs'][-1]['mode_server_pids']=[pid for pid in record['handoffs'][-1]['residency']['server_pids'] if pid not in previous_children]
                            if query_observer:
                                query_observer(browser, 'handoff', index)
                    if exercise_pressure:
                        print(json.dumps({'phase':'pressure_recovery','mode':mode}),flush=True)
                        pressure_recovery(browser,environment,residency,record,previous_children,first,second)
                    browser.call('POST','/window',{'handle':second});browser.call('DELETE','/window')
                    browser.call('POST','/window',{'handle':first})
                    hover(browser,'string')
                    def settled():
                        sample=residency.sample()
                        roots=set(sample['server_pids'])-previous_children
                        return sample if len(roots)==1 else None
                    record['settled_residency']=wait('one idle server after client overlap',settled,15)
                    if query_observer:
                        query_observer(browser, 'settled', None)
                    for path,text in files.items():assert (folder/path).read_bytes()==text.encode(),path+' written behind Save'
                    observed=residency.samples[start_index:]
                    record['summary']={'first_load_ms':record['reloads'][0]['ready_ms'],
                        'subsequent_reload':distribution([item['ready_ms'] for item in record['reloads'][1:]]),
                        'reload_process_observations':process_transitions(record['reloads']),
                        'alternating_process_observations':process_transitions(record['handoffs']),
                        'client_handoff':distribution([item['hover_ms'] for item in record['handoffs']]),
                        'peak_bridge_rss_bytes':max(item['bridge_bytes'] for item in observed),
                        'peak_children_rss_bytes':max(item['children_bytes'] for item in observed),
                        'residency_samples':len(observed),
                        'reload_server_identities':len({item['server'] for item in record['reloads']}),
                        'alternating_client_service_identities':len({item['server'] for item in record['handoffs']}),
                        'last_handoff_children_rss_bytes':record['handoffs'][-1]['residency']['children_bytes'],
                        'settled_children_rss_bytes':record['settled_residency']['children_bytes'],
                        'reload_common_child_pids':sorted(set.intersection(*[
                            {child['pid'] for child in item['residency']['children']} for item in record['reloads']])),
                        'handoff_common_child_pids':sorted(set.intersection(*[
                            {child['pid'] for child in item['residency']['children']} for item in record['handoffs']]))}
                    record['summary']['mode_reload_common_child_pids']=sorted(
                        set(record['summary']['reload_common_child_pids'])-previous_children)
                    record['summary']['mode_handoff_common_child_pids']=sorted(
                        set(record['summary']['handoff_common_child_pids'])-previous_children)
                    assert record['summary']['reload_server_identities']==samples,'Reload did not establish fresh service identities'
                    record['residency_samples']=observed
                    output.write_text(json.dumps(report,indent=2)+'\n')
                    print(json.dumps({'mode':mode,**record['summary']}),flush=True)
                assert not residency.errors,residency.errors
                report['residency_errors']=residency.errors
            report['status']='passed'
            output.write_text(json.dumps(report,indent=2)+'\n')
        except BaseException as error:
            report['status']='failed';report['error']=str(error)
            if browser and not isinstance(error,(KeyboardInterrupt,SystemExit)):
                try:
                    output.with_suffix('.png').write_bytes(base64.b64decode(browser.call('GET','/screenshot')))
                    report['failure_ui']=script(browser,'return {evidence:window.handoffEvidence,body:document.body.innerText,editor:document.querySelector("textarea[data-editor-path]")?.value};')
                except Exception as observation_error:
                    report['failure_observation_error']=str(observation_error)
            output.write_text(json.dumps(report,indent=2)+'\n')
            raise
        finally:
            # Complete every owned cleanup even if a driver or container fails.
            original_error=sys.exc_info()[0]
            cleanup_errors=[]
            for cleanup in ([browser.stop] if browser else [])+[runtime.stop,host.stop,mirror.stop,second_mirror.stop]:
                try:cleanup()
                except Exception as error:cleanup_errors.append(str(error))
            if cleanup_errors:
                report['status']='failed';report['cleanup_errors']=cleanup_errors
            if environment is not None or cleanup_errors:
                output.write_text(json.dumps(report,indent=2)+'\n')
            if cleanup_errors and original_error is None:
                raise RuntimeError('Owned measurement cleanup failed: '+ '; '.join(cleanup_errors))


if __name__=='__main__':
    def interrupted(*_):raise KeyboardInterrupt('Language measurement terminated')
    signal.signal(signal.SIGTERM,interrupted)
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--samples',type=int,default=8)
    parser.add_argument('--modules',type=int,default=128)
    parser.add_argument('--container-image',help='Run the normal whole app image with 2 CPU / 2 GiB limits')
    parser.add_argument('--exercise-pressure',action='store_true',help='Exercise bounded cgroup reserve pressure and recover both unsaved clients in each mode')
    parser.add_argument('--output',type=Path,default=Path('/tmp/openwebide-language-handoff-measurements.json'))
    args=parser.parse_args()
    assert args.samples>=2,'At least two samples are required'
    assert 0<=args.modules<=2048,'Use 0 through 2048 dependency modules'
    assert not args.exercise_pressure or args.container_image,'Pressure requires an owned whole-app container'
    environment_factory=None
    if args.container_image:
        from language_container_support import ContainerEnvironment
        environment_factory=lambda state,parent:ContainerEnvironment(state,parent,args.container_image,args.output.with_suffix('.container.log'))
    measure(args.samples,args.modules,args.output,environment_factory,args.exercise_pressure)
