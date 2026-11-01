#!/usr/bin/env python3
"""Keep frontend static initialization out of the compiled module's retained bytes.

Only the pinned wasm-bindgen shape is transformed: one defined, nonshared wasm32
memory, no instantiation-time start function and active constant-offset data. Empty segments keep
every original data index valid. The generated bindings initialize the same memory
before either init API can run. No Rust code, table, global or function index moves.
"""

import argparse
import base64
import gzip
import hashlib
import json
import os
from pathlib import Path
import re

MAGIC = b"\0asm\1\0\0\0"
NAMESPACE = "__openwebide_startup"
METADATA = "frontend-startup.json"
MAX_IMAGE_BYTES = 64 * 1024 * 1024


class UnsupportedStartup(ValueError):
    """A valid module shape that should retain its ordinary inline data."""


def read_leb(data, at, signed=False):
    value = shift = 0
    for _ in range(5):
        if at >= len(data):
            raise ValueError("Truncated WASM integer")
        byte = data[at]
        at += 1
        value |= (byte & 127) << shift
        shift += 7
        if byte < 128:
            if signed and byte & 64:
                value -= 1 << shift
            if not (
                -(1 << 31) <= value < (1 << 31) if signed else 0 <= value < (1 << 32)
            ):
                raise ValueError("WASM integer overflow")
            return value, at
    raise ValueError("Oversized WASM integer")


def encode_leb(value):
    result = bytearray()
    while value >= 128:
        result.append((value & 127) | 128)
        value >>= 7
    result.append(value)
    return bytes(result)


def wasm_string(value):
    value = value.encode()
    return encode_leb(len(value)) + value


def sections(data):
    if data[:8] != MAGIC:
        raise ValueError("Expected a version-1 WASM module")
    result, at = [], 8
    while at < len(data):
        tag = data[at]
        length, start = read_leb(data, at + 1)
        at = start + length
        if at > len(data):
            raise ValueError("Truncated WASM section")
        if tag and any(previous == tag for previous, _ in result):
            raise ValueError("Duplicate WASM section")
        result.append((tag, data[start:at]))
    return result


def function_imports(body):
    count, at = read_leb(body, 0)
    for _ in range(count):
        for position in range(2):
            size, start = read_leb(body, at)
            at = start + size
            if at > len(body):
                raise ValueError("Truncated WASM import")
            if position == 0 and body[start:at] == NAMESPACE.encode():
                raise ValueError("Startup memory namespace already exists")
        if at >= len(body):
            raise ValueError("Truncated WASM import kind")
        kind = body[at]
        at += 1
        if kind != 0:
            raise UnsupportedStartup("non-function imports")
        _, at = read_leb(body, at)
    if at != len(body):
        raise ValueError("Trailing WASM import bytes")
    return count


def transform(wasm):
    original = sections(wasm)
    by_tag = dict(original)
    if 8 in by_tag:
        raise UnsupportedStartup("an instantiation-time start function")
    if 5 not in by_tag or 11 not in by_tag:
        raise UnsupportedStartup("no defined memory or static data")
    memory = by_tag[5]
    count, at = read_leb(memory, 0)
    if count != 1:
        raise UnsupportedStartup("multiple memories")
    memory_type = memory[at:]
    flags, at = read_leb(memory_type, 0)
    if flags not in (0, 1):
        raise UnsupportedStartup("shared or memory64 memory")
    pages, at = read_leb(memory_type, at)
    descriptor = {"initial": pages}
    if flags == 1:
        maximum, at = read_leb(memory_type, at)
        descriptor["maximum"] = maximum
        if not pages <= maximum <= 65536:
            raise ValueError("Invalid wasm32 memory limits")
    if at != len(memory_type) or pages > 65536:
        raise ValueError("Invalid wasm32 memory type")
    imports = by_tag.get(2, b"\0")
    import_count = function_imports(imports)
    _, import_start = read_leb(imports, 0)
    new_imports = (
        encode_leb(import_count + 1)
        + imports[import_start:]
        + wasm_string(NAMESPACE)
        + wasm_string("memory")
        + b"\2"
        + memory_type
    )
    data = by_tag[11]
    count, at = read_leb(data, 0)
    if count > len(data):
        raise ValueError("Invalid WASM data count")
    segments = []
    for _ in range(count):
        flags, at = read_leb(data, at)
        if flags not in (0, 2):
            raise UnsupportedStartup("passive data segments")
        if flags == 2:
            index, at = read_leb(data, at)
            if index != 0:
                raise UnsupportedStartup("nonzero memory index")
        if at >= len(data) or data[at] != 0x41:
            raise UnsupportedStartup("nonconstant static offsets")
        offset, at = read_leb(data, at + 1, signed=True)
        # Constant i32 offsets are interpreted as unsigned memory addresses.
        offset &= 0xFFFFFFFF
        if at >= len(data) or data[at] != 0x0B:
            raise UnsupportedStartup("extended static offset expression")
        length, start = read_leb(data, at + 1)
        at = start + length
        if at > len(data) or not 0 <= offset <= pages * 65536 - length:
            raise ValueError("Static data exceeds initial memory")
        if length:
            segments.append((offset, data[start:at]))
    if at != len(data):
        raise ValueError("Trailing WASM data bytes")
    if 12 in by_tag:
        declared, end = read_leb(by_tag[12], 0)
        if end != len(by_tag[12]) or declared != count:
            raise ValueError("Mismatched WASM data count")
    if not segments:
        raise UnsupportedStartup("empty static data")
    first = min(offset for offset, _ in segments)
    last = max(offset + len(body) for offset, body in segments)
    if last - first > MAX_IMAGE_BYTES:
        raise UnsupportedStartup("static image exceeds packing limit")
    image = bytearray(last - first)
    # Original order matters for overlapping segments and explicit zero writes.
    for offset, body in segments:
        image[offset - first : offset - first + len(body)] = body
    empty = encode_leb(count) + bytes([0, 0x41, 0, 0x0B, 0]) * count
    result = bytearray(MAGIC)
    inserted_imports = False
    for tag, body in original:
        if tag == 5:
            continue
        if not inserted_imports and tag and tag >= 2:
            result += b"\2" + encode_leb(len(new_imports)) + new_imports
            inserted_imports = True
        if tag == 2:
            continue
        if tag == 11:
            body = empty
        result += bytes([tag]) + encode_leb(len(body)) + body
    compressed = gzip.compress(image, compresslevel=9, mtime=0)
    if len(result) + len(compressed) + 4096 >= len(wasm):
        raise UnsupportedStartup("no worthwhile size reduction")
    return (
        bytes(result),
        bytes(image),
        compressed,
        {
            "memory": descriptor,
            "offset": first,
            "end": last,
            "activeSegments": count,
            "codeSha256": sha256(by_tag.get(10, b"")),
        },
    )


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def integrity(data):
    return "sha384-" + base64.b64encode(hashlib.sha384(data).digest()).decode()


def asset(name, data):
    return {
        "file": name,
        "bytes": len(data),
        "sha256": sha256(data),
        "integrity": integrity(data),
    }


LOADER = r"""
// The module finishes loading its original static memory before either init API runs.
const __openwebideStartupMemory = await (async () => {
    const memory = new WebAssembly.Memory(__MEMORY__);
    const compressed = typeof DecompressionStream === 'function';
    const response = await fetch(new URL(compressed ? __GZIP_FILE__ : __RAW_FILE__, import.meta.url),
        {integrity: compressed ? __GZIP_SRI__ : __RAW_SRI__});
    if (!response.ok || !response.body) throw new Error('Frontend startup data unavailable');
    const body = compressed ? response.body.pipeThrough(new DecompressionStream('gzip')) : response.body;
    const reader = body.getReader();
    const target = new Uint8Array(memory.buffer);
    let offset = __OFFSET__;
    try {
        while (true) {
            const {value, done} = await reader.read();
            if (done) break;
            if (offset + value.byteLength > __END__) throw new Error('Frontend startup data exceeds memory');
            target.set(value, offset);
            offset += value.byteLength;
        }
        if (offset !== __END__) throw new Error('Frontend startup data incomplete');
    } catch (error) {
        await reader.cancel().catch(() => {});
        throw error;
    } finally {
        reader.releaseLock();
    }
    return memory;
})();
"""


def guard_initialization(source):
    header = "async function __wbg_init(module_or_path) {"
    start = source.index(header)
    end = source.index("\nexport { initSync, __wbg_init as default };", start)
    attempt = source[start:end]
    publish = "    return __wbg_finalize_init(instance, module);"
    if attempt.count(publish) != 1:
        raise ValueError("Unsupported wasm-bindgen initialization shape")
    attempt = attempt.replace(
        header, "async function __openwebideInitAttempt(module_or_path) {", 1
    )
    # initSync can finish while an asynchronous load is pending. Empty data
    # segments and the absence of a WASM start function make an unused instance
    # harmless; do not publish it or run the Rust entry point a second time.
    attempt = attempt.replace(
        publish, "    if (wasm !== undefined) return wasm;\n" + publish
    )
    wrapper = """
let __openwebideInitPending;
async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;
    if (__openwebideInitPending) return __openwebideInitPending;
    __openwebideInitPending = __openwebideInitAttempt(module_or_path);
    try {
        return await __openwebideInitPending;
    } finally {
        __openwebideInitPending = undefined;
    }
}
"""
    return source[:start] + attempt + wrapper + source[end:]


def rewrite_bindings(source, module_name, metadata):
    start = source.index("function __wbg_get_imports() {\n")
    end = source.index("\nfunction ", start + 1)
    block = source[start:end]
    anchor = "\n    return {\n"
    if block.count(anchor) != 1 or "__openwebideStartupMemory" in source:
        raise ValueError("Unsupported wasm-bindgen imports shape")
    block = block.replace(
        anchor,
        anchor + f"        {NAMESPACE}: {{memory: __openwebideStartupMemory}},\n",
        1,
    )
    source = source[:start] + block + source[end:]
    default = "new URL('openwebide-frontend_bg.wasm', import.meta.url)"
    if source.count(default) != 1:
        raise ValueError("Unsupported wasm-bindgen default module URL")
    source = source.replace(
        default, "new URL(" + json.dumps(module_name) + ", import.meta.url)"
    )
    source = guard_initialization(source)
    replacements = {
        "__MEMORY__": metadata["memory"],
        "__OFFSET__": metadata["offset"],
        "__END__": metadata["end"],
        "__GZIP_FILE__": metadata["gzip"]["file"],
        "__RAW_FILE__": metadata["raw"]["file"],
        "__GZIP_SRI__": metadata["gzip"]["integrity"],
        "__RAW_SRI__": metadata["raw"]["integrity"],
    }
    loader = LOADER
    for marker, value in replacements.items():
        loader = loader.replace(marker, json.dumps(value))
    return source + "\n" + loader


def read_metadata(dist):
    path = dist / METADATA
    if not path.exists():
        if any(
            "__openwebideStartupMemory" in p.read_text()
            for p in dist.glob("openwebide-frontend-*.js")
        ):
            raise ValueError("Packed frontend is missing startup metadata")
        return None
    data = json.loads(path.read_text())
    if data.get("version") != 1:
        raise ValueError("Unsupported frontend startup metadata")
    for role in ("gzip", "raw", "wasm", "bindings"):
        record = data[role]
        name = record["file"]
        if Path(name).name != name or not re.fullmatch(r"[a-zA-Z0-9_.-]+", name):
            raise ValueError("Invalid frontend startup asset name")
        body = (dist / name).read_bytes()
        if (
            len(body) != record["bytes"]
            or sha256(body) != record["sha256"]
            or integrity(body) != record["integrity"]
        ):
            raise ValueError("Frontend startup asset does not match metadata")
    return data


def prepare(dist):
    if read_metadata(dist):
        return
    (wasm_path,) = dist.glob("openwebide-frontend-*_bg.wasm")
    (js_path,) = dist.glob("openwebide-frontend-*.js")
    wasm, bindings = wasm_path.read_bytes(), js_path.read_bytes()
    try:
        packed, raw, compressed, metadata = transform(wasm)
    except UnsupportedStartup as error:
        print(f"Frontend startup data remains inline: {error}")
        return
    metadata["version"] = 1
    metadata["raw"] = asset("frontend-startup-" + sha256(raw)[:16] + ".raw.bin", raw)
    metadata["gzip"] = asset(
        "frontend-startup-" + sha256(compressed)[:16] + ".bin", compressed
    )
    wasm_name = "openwebide-frontend-" + sha256(packed)[:16] + "_bg.wasm"
    source = bindings.decode().replace(wasm_path.name, wasm_name)
    rewritten = rewrite_bindings(source, wasm_name, metadata).encode()
    js_name = "openwebide-frontend-" + sha256(rewritten)[:16] + ".js"
    metadata["wasm"], metadata["bindings"] = (
        asset(wasm_name, packed),
        asset(js_name, rewritten),
    )
    metadata["originalWasm"], metadata["originalBindings"] = (
        asset(wasm_path.name, wasm),
        asset(js_path.name, bindings),
    )
    index = dist / "index.html"
    html = index.read_text()
    if wasm_path.name not in html or js_path.name not in html:
        raise ValueError("Frontend index does not reference its generated assets")
    html = html.replace(wasm_path.name, wasm_name).replace(js_path.name, js_name)
    html = html.replace(integrity(wasm), integrity(packed)).replace(
        integrity(bindings), integrity(rewritten)
    )
    # All parsing and binding-shape checks finish before changing staging files.
    for name, body in [
        (wasm_name, packed),
        (js_name, rewritten),
        (metadata["raw"]["file"], raw),
        (metadata["gzip"]["file"], compressed),
    ]:
        (dist / name).write_bytes(body)
    index.write_text(html)
    if wasm_path.name != wasm_name:
        wasm_path.unlink()
    if js_path.name != js_name:
        js_path.unlink()
    (dist / METADATA).write_text(json.dumps(metadata, indent=2) + "\n")
    print(
        f"Prepared frontend startup memory: {len(wasm)} -> {len(packed)} WASM bytes, {len(compressed)} compressed data bytes"
    )


def shell_files(dist):
    metadata = read_metadata(dist)
    if not metadata:
        return ""
    return (
        "...(typeof DecompressionStream === 'function' ? ["
        + json.dumps("/" + metadata["gzip"]["file"])
        + "] : ["
        + json.dumps("/" + metadata["raw"]["file"])
        + "]),"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dist", type=Path, default=os.environ.get("TRUNK_STAGING_DIR")
    )
    parser.add_argument("--shell-files", action="store_true")
    args = parser.parse_args()
    if args.dist is None:
        parser.error("Set TRUNK_STAGING_DIR or --dist")
    if args.shell_files:
        print(shell_files(args.dist))
    else:
        prepare(args.dist)
