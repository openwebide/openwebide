"""Supplementary retained-memory probe used in owned Chrome targets.

Call after HeapProfiler.collectGarbage with tools.editor_memory.Cdp and an
attached target session. The caller owns session detachment and browser cleanup.
These object queries are diagnostics; their timings are excluded from latency.
"""

def wasm_memories(client, session):
    group = 'editor-retained-wasm'
    try:
        prototype = client.call('Runtime.evaluate', {'expression': 'WebAssembly.Memory.prototype', 'objectGroup': group}, session_id=session)
        assert 'exceptionDetails' not in prototype, prototype
        objects = client.call('Runtime.queryObjects', {'prototypeObjectId': prototype['result']['objectId'], 'objectGroup': group}, session_id=session)
        result = client.call('Runtime.callFunctionOn', {'objectId': objects['objects']['objectId'], 'functionDeclaration': 'function(){return this.map(memory => memory.buffer.byteLength)}', 'returnByValue': True}, session_id=session)
        assert 'exceptionDetails' not in result, result
        sizes = result['result']['value']
        assert isinstance(sizes, list) and all(isinstance(size, int) and size > 0 for size in sizes), sizes
        return sizes
    finally:
        client.call('Runtime.releaseObjectGroup', {'objectGroup': group}, session_id=session)
