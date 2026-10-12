"""Production recovery evidence follows the framed syntax-worker protocol."""
import ast
from pathlib import Path
import shutil
import subprocess
import unittest


class WorkerObserverTests(unittest.TestCase):
    @unittest.skipUnless(shutil.which("node"), "Node is needed for the worker protocol fixture")
    def test_complete_full_and_delta_sources_reject_partial_or_cancelled_frames(self):
        tree = ast.parse(Path(__file__).with_name("check-editor-recovery.py").read_text())
        observer = next(node.value for node in ast.walk(tree)
                        if isinstance(node, ast.Constant) and isinstance(node.value, str)
                        and "window.editorWorkerEvidence =" in node.value)
        fixture = r"""
const assert = require('node:assert/strict');
global.window=global;
global.Worker=class {
    constructor(){this.listeners=[];}
    addEventListener(_type,fn){this.listeners.push(fn);}
    send(data){for(const fn of this.listeners)fn({data});}
};
"""
        checks = r"""
const worker=new Worker('blob:fixture');
worker.send('openwebide-editor-ready:6:frames:1');
const encode=s=>new TextEncoder().encode(s);
const frame=(ticket,sequence,part)=>worker.send(JSON.stringify({version:6,framing:1,ticket,sequence,part}));
frame(1,0,{Begin:{status:{Ready:{reused:false}},analysis:{source:{Full:{bytes:encode('文😀\r\n').length}},base_ticket:null}}});
frame(1,1,{Source:'文😀\r\n'});
assert.equal(editorWorkerEvidence.sources.length,0);
frame(1,2,'End');
assert.equal(editorWorkerEvidence.sources.at(-1),'文😀\n');
frame(2,0,{Begin:{status:{Ready:{reused:true}},analysis:{source:{Replace:{start:3,end:7,bytes:3}},base_ticket:1}}});
frame(2,1,{Source:'猫'});frame(2,2,'End');
assert.equal(editorWorkerEvidence.sources.at(-1),'文猫\n');
frame(3,0,{Begin:{status:{Ready:{}},analysis:{source:{Full:{bytes:1}}}}});
frame(3,2,{Source:'x'});frame(3,3,'End');
assert.equal(editorWorkerEvidence.sources.length,2);
frame(4,0,{Begin:{status:'Cancelled',analysis:null}});frame(4,1,'End');
assert.equal(editorWorkerEvidence.sources.length,2);
assert.equal(editorWorkerEvidence.instances,1);
"""
        subprocess.run(["node", "-e", fixture + observer + checks], check=True, capture_output=True, text=True)


if __name__ == "__main__":
    unittest.main()
