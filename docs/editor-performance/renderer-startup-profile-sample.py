import runpy,json,pathlib,hashlib,sys
root=pathlib.Path('/workspace/repos/openwebide');manifest=json.loads(pathlib.Path('/fixture/manifest.json').read_text());sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
assert {p.name:sha(p) for p in (root/'frontend/dist').glob('*.wasm')}==manifest['frontendWasmSha256']
assert sha(root/'target/wasm32-wasip2/release/openwebide_backend.wasm')==manifest['backendWasmSha256']
assert sha(root/'tools/measure-editor-view.py')==manifest['measurementScriptSha256']
module=runpy.run_path(str(root/'tools/measure-editor-view.py'))
case=sys.argv[2]
if case=='small': module['measure'].__globals__['source_for']=lambda case: 'x\n' * 1000
result=module['measure'](case,sys.argv[1],False,True,1,'beginning',True)
assert result['completeSourceAfterInputVerified'] and result['peakChromePssKiB'] is not None
print(json.dumps({'runtime':manifest,'probeScriptSha256':sha(pathlib.Path(__file__)),**result}),flush=True)
