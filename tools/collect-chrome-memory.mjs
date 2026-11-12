const ws = new WebSocket(process.argv[2]);
const pending = new Map(), contexts = new Map();
let sequence = 0;
const deadline = setTimeout(() => { process.stderr.write('CDP snapshot deadline\n'); process.exit(124); }, 20000);
ws.addEventListener('message', event => {
  const value = JSON.parse(event.data);
  if (value.method === 'Runtime.executionContextCreated') {
    const entries = contexts.get(value.sessionId) ?? new Map();
    entries.set(value.params.context.id, value.params.context);
    contexts.set(value.sessionId, entries);
  }
  if (value.method === 'Runtime.executionContextDestroyed') contexts.get(value.sessionId)?.delete(value.params.executionContextId);
  if (value.method === 'Runtime.executionContextsCleared') contexts.get(value.sessionId)?.clear();
  if (!value.id) return;
  const waiter = pending.get(value.id);
  if (!waiter) return;
  pending.delete(value.id);
  if (value.error) waiter.reject(new Error(value.error.message));
  else waiter.resolve(value.result ?? {});
});
function call(method, params = {}, sessionId) {
  return new Promise((resolve, reject) => {
    const id = ++sequence;
    pending.set(id, {resolve, reject});
    ws.send(JSON.stringify({id, method, params, ...(sessionId ? {sessionId} : {})}));
  });
}
try {
  await new Promise((resolve, reject) => {
    ws.addEventListener('open', resolve, {once:true});
    ws.addEventListener('error', () => reject(new Error('Owned Chrome CDP connection failed')), {once:true});
  });
  if (process.argv[3] === '--processes-only') {
    console.log(JSON.stringify({processes:(await call('SystemInfo.getProcessInfo')).processInfo}));
  } else {
  const accepted = new Set(['page', 'iframe', 'worker', 'shared_worker', 'service_worker']);
  const before = (await call('Target.getTargets')).targetInfos.filter(t => accepted.has(t.type));
  if (!before.some(t => t.type === 'page')) throw new Error('No owned page target');
  const targets = [], isolates = new Map(), realms = new Map();
  for (const target of before) {
    const {sessionId} = await call('Target.attachToTarget', {targetId:target.targetId, flatten:true});
    try {
      await call('Runtime.enable', {}, sessionId);
      const isolate = (await call('Runtime.getIsolateId', {}, sessionId)).id;
      if (!isolates.has(isolate)) {
        const preGcUsage = await call('Runtime.getHeapUsage', {}, sessionId);
        await call('HeapProfiler.enable', {}, sessionId);
        await call('HeapProfiler.collectGarbage', {}, sessionId);
        const usage = await call('Runtime.getHeapUsage', {}, sessionId);
        isolates.set(isolate, {id:isolate, pre_gc_usage:preGcUsage, usage, targets:[]});
      }
      isolates.get(isolate).targets.push(target.targetId);
      const execution = [...(contexts.get(sessionId)?.values() ?? [])].filter(c => c.auxData?.isDefault !== false);
      if (execution.length === 0) throw new Error('No default execution realm');
      const realmIds = [];
      for (const context of execution) {
        if (!context.uniqueId) throw new Error('No globally unique execution realm id');
        realmIds.push(context.uniqueId);
        if (realms.has(context.uniqueId)) {
          realms.get(context.uniqueId).targets.push(target.targetId);
          continue;
        }
        const group = 'owned-wasm-memory-' + context.uniqueId;
        try {
          const prototype = await call('Runtime.evaluate', {expression:'WebAssembly.Memory.prototype', contextId:context.id, objectGroup:group}, sessionId);
          if (prototype.exceptionDetails || !prototype.result.objectId) throw new Error('Cannot inspect WASM memory prototype');
          const objects = await call('Runtime.queryObjects', {prototypeObjectId:prototype.result.objectId, objectGroup:group}, sessionId);
          const response = await call('Runtime.callFunctionOn', {
            objectId:objects.objects.objectId, objectGroup:group, returnByValue:true,
            functionDeclaration:'function(){return this.map(memory=>({linear_bytes:memory.buffer.byteLength,shared:typeof SharedArrayBuffer!=="undefined"&&memory.buffer instanceof SharedArrayBuffer}));}'
          }, sessionId);
          if (response.exceptionDetails || !Array.isArray(response.result.value)) throw new Error('Cannot read live WASM memories');
          realms.set(context.uniqueId, {id:context.uniqueId, isolate, targets:[target.targetId], memories:response.result.value});
        } finally {
          await call('Runtime.releaseObjectGroup', {objectGroup:group}, sessionId);
        }
      }
      let wasm = null;
      let dom = null;
      if (target.type === 'page') {
        dom = await call('Memory.getDOMCounters', {}, sessionId);
        const response = await call('Runtime.evaluate', {expression:`(() => {
          const capture = globalThis.handoffMemory;
          return capture ? {started:capture.started, linear_bytes:capture.memory?.buffer.byteLength ?? null,
            export_is_memory:capture.memory instanceof WebAssembly.Memory,
            editor_path:document.querySelector('textarea[data-editor-path]')?.dataset.editorPath ?? null} : null;
        })()`, returnByValue:true}, sessionId);
        if (response.exceptionDetails) throw new Error('Page memory expression failed');
        wasm = response.result.value ?? null;
      }
      targets.push({id:target.targetId, type:target.type, url:target.url, isolate, realm_ids:realmIds, wasm, dom});
    } finally {
      await call('Target.detachFromTarget', {sessionId});
    }
  }
  const after = (await call('Target.getTargets')).targetInfos.filter(t => accepted.has(t.type));
  const ids = list => list.map(t => t.targetId).sort().join(',');
  if (ids(before) !== ids(after)) throw new Error('Target set changed during snapshot');
  const processes = (await call('SystemInfo.getProcessInfo')).processInfo;
  const totals = {};
  for (const value of isolates.values()) {
    for (const [key, bytes] of Object.entries(value.usage)) totals[key] = (totals[key] ?? 0) + bytes;
  }
  const memories = [...realms.values()].flatMap(r => r.memories);
  console.log(JSON.stringify({targets, isolates:[...isolates.values()], unique_isolates:isolates.size,
    wasm_realms:[...realms.values()], live_wasm_memories:memories.length,
    unshared_wasm_linear_bytes:memories.filter(m=>!m.shared).reduce((total,m)=>total+m.linear_bytes,0),
    shared_wasm_memories:memories.filter(m=>m.shared).length,
    per_isolate_totals:totals, processes, stable_target_set:true}));
  }
} catch (error) {
  process.stderr.write(String(error.message) + '\n');
  process.exitCode = 1;
} finally {
  clearTimeout(deadline);
  ws.close();
}
