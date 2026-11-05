"""Recovery evidence follows complete, source-checked host language responses."""
from pathlib import Path
import shutil
import subprocess
import unittest


class LanguageObserverTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("node"), "Node is needed for the language protocol fixture")
    def test_complete_full_and_delta_sources_reject_failed_cancelled_and_mismatched_results(self):
        tools = Path(__file__).parent
        self.assertIn("tools/editor-language-observer.js", (tools / "check-editor-recovery.py").read_text())
        observer = (tools / "editor-language-observer.js").read_text().replace("__HOLD_LANGUAGE__", "false")
        fixture = r"""
const assert = require('node:assert/strict');
const {createHash} = require('node:crypto');
global.window=global;
const responses=[];
global.fetch=async()=>new Response(JSON.stringify(responses.shift()),{headers:{'content-type':'application/json'}});
"""
        checks = r"""
(async()=>{
const digest=text=>createHash('sha256').update(text).digest('hex');
const input=(request,revision,source,text)=>({version:1,operation:'analyze',document:'document',
    language:'fixture',request,revision,sourceDigest:digest(text),source});
const output=(value,status='ready')=>({version:1,operation:'analyzed',document:value.document,
    language:value.language,request:value.request,revision:value.revision,
    sourceDigest:value.sourceDigest,status:{type:status}});
async function post(path,body,response){
    responses.push(response);
    await fetch('http://localhost/plugins/'+path,{method:'POST',body:JSON.stringify(body)});
}
async function begin(id,value){
    await post('invoke',{call:{name:'@openwebide/language',arguments:JSON.stringify(value)}},
        {id,step:{status:'pending'}});
}
async function complete(id,value){
    await post('continue',{continuation:{id}},
        {id,step:{status:'complete',ok:true,content:JSON.stringify(value)}});
}
const full=input(1,1,{type:'full',text:'文😀\r\n'},'文😀\r\n');
await begin('full',full);
assert.equal(editorLanguageEvidence.sources.length,0);
await complete('full',output(full));
assert.deepEqual(editorLanguageEvidence.sources,['文😀\n']);
const delta=input(2,2,{type:'replace',baseRevision:1,start:3,end:7,text:'猫'},'文猫\r\n');
await begin('delta',delta);
assert.equal(editorLanguageEvidence.sources.length,1);
await complete('delta',output(delta));
assert.deepEqual(editorLanguageEvidence.sources,['文😀\n','文猫\n']);
const later=input(3,3,{type:'full',text:'later'},'later');
await begin('failed',later);
await post('continue',{continuation:{id:'failed'}},{id:'failed',step:{status:'failed'}});
await begin('cancelled',later);
await complete('cancelled',output(later,'cancelled'));
assert.equal(editorLanguageEvidence.sources.length,2);
assert.equal(editorLanguageEvidence.errors.length,0);
await begin('wrong-identity',later);
await complete('wrong-identity',{...output(later),request:999});
const badDigest={...later,sourceDigest:digest('different')};
await begin('bad-digest',badDigest);
await complete('bad-digest',output(badDigest));
const missingBase=input(4,4,{type:'replace',baseRevision:99,start:0,end:0,text:'x'},'x');
await begin('missing-base',missingBase);
await complete('missing-base',output(missingBase));
assert.equal(editorLanguageEvidence.sources.length,2);
assert.equal(editorLanguageEvidence.errors.length,3);
// A malformed UTF-8 replacement must not produce evidence either.
const splitScalar=input(5,5,{type:'replace',baseRevision:2,start:1,end:3,text:'x'},'x');
await begin('split-scalar',splitScalar);
assert.equal(editorLanguageEvidence.sources.length,2);
assert.equal(editorLanguageEvidence.errors.length,4);
})().catch(error=>{console.error(error);process.exitCode=1;});
"""
        subprocess.run(["node", "-e", fixture + observer + checks], check=True, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
