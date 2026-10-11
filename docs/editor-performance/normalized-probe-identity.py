import runpy,json,sys,hashlib,pathlib
module=runpy.run_path('/workspace/repos/openwebide/tools/measure-editor-view.py')
Original=module['Browser']
PATCH=r"""
(() => {
 const native=Element.prototype.getBoundingClientRect, seen=new WeakSet(), retained=new Map();
 window.probeLayoutIdentities={unique:0,withinPhaseMatches:0,previousPhaseMatches:0,extentDisagreements:0,samples:[]};
 Element.prototype.getBoundingClientRect=function(...args){
  const result=native.apply(this,args);
  if(this.matches('.editor-row-measure .editor-source-line') && !seen.has(this)) {
   seen.add(this);
   const owner=this.closest('.editor-row-measure'), phase=window.editorViewMeasurement?.phase;
   const copy=this.cloneNode(true);
   for(const element of [copy,...copy.querySelectorAll('*')]) {
    for(const attribute of [...element.attributes]) {
     if(attribute.name==='data-line' || attribute.name.startsWith('data-source-') || attribute.name.startsWith('data-native-')) element.removeAttribute(attribute.name);
    }
   }
   const key=owner.style.cssText+'|'+owner.getAttribute('data-measure-font')+'|'+copy.outerHTML;
   const previous=retained.get(key), current={phase,start:this.getAttribute('data-source-start'),width:result.width,height:result.height};
   if(previous) {
    probeLayoutIdentities[previous.phase===phase?'withinPhaseMatches':'previousPhaseMatches']++;
    if(previous.width!==current.width || previous.height!==current.height)probeLayoutIdentities.extentDisagreements++;
    if(probeLayoutIdentities.samples.length<12)probeLayoutIdentities.samples.push({previous,current});
   } else {probeLayoutIdentities.unique++;retained.set(key,current);}
  }
  return result;
 };
})();
"""
class Browser(Original):
 def call(self,method,path,body=None,session=True,timeout=30):
  if path=='/goog/cdp/execute' and body and body.get('cmd')=='Page.addScriptToEvaluateOnNewDocument':
   body={**body,'params':{**body['params'],'source':PATCH+body['params']['source']}}
  return super().call(method,path,body,session,timeout)
 def stop(self):
  try:
   if self.session:print(json.dumps({'diagnostic':'Source-position-normalized probe identity; no cache adoption or geometry authorization','identities':self.script('return window.probeLayoutIdentities')}),flush=True)
  finally:super().stop()
module['measure'].__globals__['Browser']=Browser
root=pathlib.Path('/workspace/repos/openwebide')
print(json.dumps({'diagnostic':True,'frontendWasmSha256':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in (root/'frontend/dist').glob('*.wasm')},'backendWasmSha256':hashlib.sha256((root/'target/wasm32-wasip2/release/openwebide_backend.wasm').read_bytes()).hexdigest(),'frontendBaseline':'f86dd7b production bundle; predates routing/indexed navigation/source-equality changes','backendBaseline':'Frozen 2e934df artifact; disposable database and projects only'}),flush=True)
for mode in ['local','remote']:
 print(json.dumps(module['measure']('styled-long-line',mode,True,True,1,'beginning')),flush=True)
