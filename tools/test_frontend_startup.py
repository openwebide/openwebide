"""Native WASM initialization is the oracle for packed frontend static data."""

import contextlib
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

spec = importlib.util.spec_from_file_location(
    "frontend_startup", Path(__file__).with_name("prepare-frontend-startup.py")
)
startup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(startup)


def u(value):
    result = bytearray()
    while value >= 128:
        result.append((value & 127) | 128)
        value >>= 7
    result.append(value)
    return bytes(result)


def signed(value):
    result = bytearray()
    while True:
        byte = value & 127
        value >>= 7
        done = (value == 0 and not byte & 64) or (value == -1 and byte & 64)
        result.append(byte if done else byte | 128)
        if done:
            return bytes(result)


def section(tag, body):
    return bytes([tag]) + u(len(body)) + body


def string(value):
    body = value.encode()
    return u(len(body)) + body


def fixture(passive=False, offset=65536):
    # Out-of-order, overlapping segments include explicit zero overwrites and
    # arbitrary bytes. Bulk instructions reference nonzero data indices, even
    # though all active segments have been dropped by native instantiation.
    segments = [
        (offset, ("static 文😀\r\n".encode() + b"\0\xff") * 10000),
        (66000, b"\0" * 1000 + b"overwrite" * 37),
        (65540, b"last\0segment"),
    ]
    data = u(len(segments))
    for number, (at, body) in enumerate(segments):
        data += (
            (b"\1" if passive and number == 1 else b"\0\x41" + signed(at) + b"\x0b")
            + u(len(body))
            + body
        )
    no_op_bulk = b"\0\x41\0\x41\0\x41\0\xfc\x08\1\0\xfc\x09\2\x0b"
    dropped_bulk = b"\0\x41\0\x41\0\x41\1\xfc\x08\1\0\x0b"
    exports = (
        u(3)
        + string("memory")
        + b"\2\0"
        + string("drop")
        + b"\0\0"
        + string("read_dropped")
        + b"\0\1"
    )
    return (
        b"\0asm\1\0\0\0"
        + section(1, b"\1\x60\0\0")
        + section(3, b"\2\0\0")
        + section(5, b"\1\1\5\7")
        + section(7, exports)
        + section(12, u(3))
        + section(
            10,
            u(2)
            + u(len(no_op_bulk))
            + no_op_bulk
            + u(len(dropped_bulk))
            + dropped_bulk,
        )
        + section(11, data)
    )


BINDINGS = """let wasm;
function __wbg_get_imports() {
    return {
        env: {},
    };
}
function __wbg_finalize_init(instance, module) {
    wasm = instance.exports;
    globalThis.startupInitializations = (globalThis.startupInitializations || 0) + 1;
    return wasm;
}
function initSync(module) {
    if (wasm !== undefined) return wasm;
    if (!(module instanceof WebAssembly.Module)) ({module} = module);
    return __wbg_finalize_init(new WebAssembly.Instance(module, __wbg_get_imports()), module);
}
async function __wbg_load(response, imports) {
    return await WebAssembly.instantiate(await response.arrayBuffer(), imports);
}
async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;
    if (!module_or_path) module_or_path = new URL('openwebide-frontend_bg.wasm', import.meta.url);
    const imports = __wbg_get_imports();
    const {instance, module} = await __wbg_load(await fetch(module_or_path), imports);
    return __wbg_finalize_init(instance, module);
}
export { initSync, __wbg_init as default };
"""


class StartupTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.original = fixture()
        self.wasm = self.dist / "openwebide-frontend-aaaaaaaaaaaaaaaa_bg.wasm"
        self.js = self.dist / "openwebide-frontend-bbbbbbbbbbbbbbbb.js"
        self.wasm.write_bytes(self.original)
        self.js.write_text(BINDINGS)
        (self.dist / "package.json").write_text('{"type":"module"}')
        (self.dist / "index.html").write_text(
            f'<link href="{self.wasm.name}" integrity="{startup.integrity(self.original)}">'
            f'<script src="{self.js.name}" integrity="{startup.integrity(BINDINGS.encode())}"></script>'
        )

    def prepare(self):
        with contextlib.redirect_stdout(io.StringIO()):
            startup.prepare(self.dist)
        return startup.read_metadata(self.dist)

    def snapshot(self):
        return {path.name: path.read_bytes() for path in self.dist.iterdir()}

    def test_native_initial_memory_and_bulk_data_lifetimes_match_both_loader_apis(self):
        metadata = self.prepare()
        original = self.root / "original.wasm"
        original.write_bytes(self.original)
        self.assertEqual(metadata["memory"], {"initial": 5, "maximum": 7})
        self.assertEqual(metadata["activeSegments"], 3)
        self.assertLess(
            metadata["wasm"]["bytes"] + metadata["gzip"]["bytes"], len(self.original)
        )
        self.assertIsNotNone(
            shutil.which("node"),
            "Node is required for the independent native WASM oracle",
        )
        # Each capability/API combination starts a fresh JS module. Node's
        # WebAssembly engine, not the Python decoder, initializes the reference.
        program = r"""
          const fs=require('node:fs'),crypto=require('node:crypto'),assert=require('node:assert/strict');
          const {pathToFileURL,fileURLToPath}=require('node:url');
          (async()=>{
            const metadata=JSON.parse(fs.readFileSync(process.argv[1]));
            const original=new WebAssembly.Instance(new WebAssembly.Module(fs.readFileSync(process.argv[2]))).exports;
            const expected=Buffer.from(original.memory.buffer).toString('hex');
            let failWasm=process.argv[6]==='retry';
            globalThis.fetch=async(input,options={})=>{
              if(fileURLToPath(input).endsWith('.wasm') && failWasm){failWasm=false;throw Error('Transient WASM load failure');}
              const bytes=fs.readFileSync(fileURLToPath(input));
              if(options.integrity)assert.equal(options.integrity,'sha384-'+crypto.createHash('sha384').update(bytes).digest('base64'));
              return new Response(bytes);
            };
            if(process.argv[3]==='raw')globalThis.DecompressionStream=undefined;
            const bindings=await import(pathToFileURL(process.argv[4]));
            const module=new WebAssembly.Module(fs.readFileSync(process.argv[5]));
            let actual;
            switch(process.argv[6]) {
              case 'sync': actual=bindings.initSync({module});break;
              case 'concurrent': {
                const [a,b]=await Promise.all([bindings.default(),bindings.default()]);
                assert.equal(a,b);actual=a;break;
              }
              case 'mixed': {
                const pending=bindings.default();actual=bindings.initSync({module});
                assert.equal(await pending,actual);break;
              }
              case 'retry': {
                const results=await Promise.allSettled([bindings.default(),bindings.default()]);
                assert.ok(results.every(result=>result.status==='rejected'));
                assert.equal(globalThis.startupInitializations,undefined);
                actual=await bindings.default();break;
              }
              default: actual=await bindings.default();
            }
            assert.equal(globalThis.startupInitializations,1);
            assert.equal(Buffer.from(actual.memory.buffer).toString('hex'),expected);
            original.drop();actual.drop();
            assert.equal(Buffer.from(actual.memory.buffer).toString('hex'),Buffer.from(original.memory.buffer).toString('hex'));
            assert.throws(()=>original.read_dropped(),WebAssembly.RuntimeError);
            assert.throws(()=>actual.read_dropped(),WebAssembly.RuntimeError);
            assert.equal(await bindings.default(),actual);
          })().catch(e=>{console.error(e);process.exitCode=1;});
        """
        for capability in ("gzip", "raw"):
            for api in ("sync", "async", "concurrent", "mixed", "retry"):
                with self.subTest(capability=capability, api=api):
                    subprocess.run(
                        [
                            "node",
                            "-e",
                            program,
                            str(self.dist / startup.METADATA),
                            str(original),
                            capability,
                            str(self.dist / metadata["bindings"]["file"]),
                            str(self.dist / metadata["wasm"]["file"]),
                            api,
                        ],
                        check=True,
                        capture_output=True,
                        text=True,
                    )

    def test_asset_identity_integrity_and_repeated_build_remain_coherent(self):
        metadata = self.prepare()
        before = self.snapshot()
        self.prepare()
        self.assertEqual(self.snapshot(), before)
        index = (self.dist / "index.html").read_text()
        for role in ("wasm", "bindings"):
            self.assertIn(metadata[role]["file"], index)
            self.assertIn(metadata[role]["integrity"], index)
        self.assertNotIn(self.wasm.name, index)
        self.assertNotIn(self.js.name, index)
        cache = startup.shell_files(self.dist)
        for role in ("gzip", "raw"):
            self.assertIn(metadata[role]["file"], cache)
        data = self.dist / metadata["gzip"]["file"]
        data.write_bytes(data.read_bytes()[:-1] + b"x")
        with self.assertRaisesRegex(ValueError, "does not match"):
            startup.shell_files(self.dist)
        with self.assertRaisesRegex(ValueError, "does not match"):
            startup.prepare(self.dist)

    def test_missing_metadata_cannot_publish_an_incomplete_offline_family(self):
        self.prepare()
        (self.dist / startup.METADATA).unlink()
        with self.assertRaisesRegex(ValueError, "missing startup metadata"):
            startup.shell_files(self.dist)

    def test_static_data_only_changes_update_bindings_and_worker_identity(self):
        originals = self.snapshot()
        first = self.prepare()

        def worker_name(dist):
            (dist / "fonts").mkdir(exist_ok=True)
            (dist / "styles-test.css").write_text("/* fixture */")
            subprocess.run(
                ["sh", "pwa/build.sh"],
                cwd=Path(__file__).resolve().parent.parent / "frontend",
                env=dict(os.environ, TRUNK_STAGING_DIR=str(dist)),
                check=True,
                capture_output=True,
                text=True,
            )
            workers = list(dist.glob("editor-worker-*.js"))
            self.assertEqual(len(workers), 1)
            return workers[0].name

        first_worker = worker_name(self.dist)
        second_dist = self.root / "second"
        second_dist.mkdir()
        for name, body in originals.items():
            (second_dist / name).write_bytes(body)
        changed = self.original.replace(b"static", b"STATIC", 1)
        (second_dist / self.wasm.name).write_bytes(changed)
        index = second_dist / "index.html"
        index.write_text(
            index.read_text().replace(
                startup.integrity(self.original), startup.integrity(changed)
            )
        )
        with contextlib.redirect_stdout(io.StringIO()):
            startup.prepare(second_dist)
        second = startup.read_metadata(second_dist)
        self.assertEqual(first["wasm"], second["wasm"])
        self.assertNotEqual(first["raw"]["file"], second["raw"]["file"])
        self.assertNotEqual(first["bindings"]["file"], second["bindings"]["file"])
        self.assertNotEqual(first_worker, worker_name(second_dist))

    def test_passive_segments_keep_original_module_and_bindings_untouched(self):
        self.wasm.write_bytes(fixture(passive=True))
        before = self.snapshot()
        self.assertIsNone(self.prepare())
        self.assertEqual(self.snapshot(), before)
        self.assertEqual(startup.shell_files(self.dist), "")

    def test_instantiation_start_functions_keep_original_loading_untouched(self):
        marker = section(12, u(3))
        self.wasm.write_bytes(
            self.original.replace(marker, section(8, u(0)) + marker, 1)
        )
        before = self.snapshot()
        self.assertIsNone(self.prepare())
        self.assertEqual(self.snapshot(), before)

    def test_out_of_bounds_static_data_is_rejected_before_any_staging_write(self):
        self.wasm.write_bytes(fixture(offset=400000))
        before = self.snapshot()
        with self.assertRaisesRegex(ValueError, "exceeds initial memory"):
            self.prepare()
        self.assertEqual(self.snapshot(), before)

    def test_unknown_binding_shape_cannot_partially_rewrite_a_build(self):
        self.js.write_text("export default function custom() {}")
        before = self.snapshot()
        with self.assertRaises(ValueError):
            self.prepare()
        self.assertEqual(self.snapshot(), before)


if __name__ == "__main__":
    unittest.main()
