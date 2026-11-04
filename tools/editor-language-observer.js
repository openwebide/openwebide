// Test-only observer for the public language protocol over either real host
// transport. It never supplies a plugin response or a language implementation.
(() => {
  if (window.editorLanguageEvidence) return;
  const evidence = window.editorLanguageEvidence = {
    hold: __HOLD_LANGUAGE__, held: 0, queued: [], sources: [], errors: [], responses: 0, outcomes: [], requests: [],
  };
  window.releaseEditorLanguage = () => {
    evidence.hold = false;
    for (const deliver of evidence.queued.splice(0)) deliver();
  };
  const original = window.fetch, actors = new Map(), documents = new Map();
  const encoder = new TextEncoder(), decoder = new TextDecoder('utf-8', {fatal: true});
  function sourceFor(input) {
    if (input.source.type === 'full') return encoder.encode(input.source.text);
    const base = documents.get(input.document), change = input.source;
    if (change.type !== 'replace' || !base || base.revision !== change.baseRevision ||
        change.start > change.end || change.end > base.bytes.length) return null;
    // Fatal decoding checks UTF-8 scalar boundaries independently of Rust.
    decoder.decode(base.bytes.subarray(0, change.start));
    decoder.decode(base.bytes.subarray(change.end));
    const inserted = encoder.encode(change.text);
    const bytes = new Uint8Array(change.start + inserted.length + base.bytes.length - change.end);
    bytes.set(base.bytes.subarray(0, change.start));
    bytes.set(inserted, change.start);
    bytes.set(base.bytes.subarray(change.end), change.start + inserted.length);
    return bytes;
  }
  window.fetch = async function(...args) {
    const request = args[0] instanceof Request ? args[0].clone() : new Request(args[0], args[1]);
    const path = new URL(request.url).pathname;
    const preparation = /\/plugins\/(prepare(?:\/status)?|preparation)$/.test(path);
    if (request.method !== 'POST' || (!preparation && !/\/plugins\/(invoke|continue|cancel)$/.test(path))) {
      return original.apply(this, args);
    }
    let body = null;
    try { body = JSON.parse(await request.text()); } catch {}
    const trace = {path, started: performance.now(),
      revision: actors.get(body?.continuation?.id ?? body?.id)?.input.revision ?? null};
    evidence.requests.push(trace);
    if (evidence.requests.length > 64) evidence.requests.shift();
    let response;
    try {
      response = await original.apply(this, args);
      trace.http = response.status;
    } catch (error) {
      trace.error = String(error).slice(0, 256);
      throw error;
    } finally {
      trace.elapsed = performance.now() - trace.started;
    }
    if (preparation) {
      try { trace.preparation = (await response.clone().json()).state; } catch {}
      return response;
    }
    if (path.endsWith('/cancel')) return response;
    if (!response.ok || !body) {
      evidence.outcomes.push({path, http: response.status});
      if (evidence.outcomes.length > 64) evidence.outcomes.shift();
      return response;
    }
    try {
      const invocation = await response.clone().json();
      if (path.endsWith('/invoke')) {
        const call = body.call ?? body;
        if (call.name !== '@openwebide/language') return response;
        const input = JSON.parse(call.arguments);
        if (input.operation === 'close') documents.delete(input.document);
        if (input.operation !== 'analyze') return response;
        trace.revision = input.revision;
        actors.set(invocation.id, {input, bytes: sourceFor(input)});
        if (actors.size > 128) actors.delete(actors.keys().next().value);
      }
      const id = invocation.id ?? body.continuation?.id ?? body.id;
      const pending = actors.get(id), step = invocation.step;
      if (pending) {
        evidence.outcomes.push({path, http: response.status, step: step?.status,
          ok: step?.ok, revision: pending.input.revision});
        if (evidence.outcomes.length > 64) evidence.outcomes.shift();
      }
      if (step?.status === 'failed') actors.delete(id);
      if (!pending || step?.status !== 'complete') return response;
      actors.delete(id);
      if (!step.ok) return response;
      const output = JSON.parse(step.content), input = pending.input, bytes = pending.bytes;
      evidence.outcomes[evidence.outcomes.length - 1].analysis = output.status?.type;
      if (output.operation !== 'analyzed' || output.status.type !== 'ready') return response;
      if (!bytes || output.version !== 1 || output.document !== input.document ||
          output.request !== input.request || output.revision !== input.revision ||
          output.language !== input.language || output.sourceDigest !== input.sourceDigest) {
        throw new Error('Language result identity does not match its source request');
      }
      const digest = [...new Uint8Array(await crypto.subtle.digest('SHA-256', bytes))]
        .map(byte => byte.toString(16).padStart(2, '0')).join('');
      if (digest !== output.sourceDigest) throw new Error('Language source digest differs');
      const source = decoder.decode(bytes);
      const previous = documents.get(input.document);
      if (!previous || previous.request < input.request) {
        documents.set(input.document, {bytes, revision: input.revision, request: input.request});
      }
      if (documents.size > 32) documents.delete(documents.keys().next().value);
      if (evidence.hold) {
        evidence.held++;
        await new Promise(resolve => evidence.queued.push(resolve));
      }
      evidence.responses++;
      evidence.sources.push(source.replace(/\r\n/g, '\n'));
      if (evidence.sources.length > 32) evidence.sources.shift();
    } catch (error) {
      evidence.errors.push(String(error));
    }
    return response;
  };
})();
