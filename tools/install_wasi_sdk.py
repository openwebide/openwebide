#!/usr/bin/env python3
"""Install the pinned public C/C++ toolchain for sandboxed WASI plugin builds."""
import argparse
import hashlib
import json
import platform
from pathlib import Path
import sys
import tarfile
import tempfile
import urllib.request


def install(destination: Path) -> Path:
    pin = json.loads(Path(__file__).with_name("wasi-sdk.json").read_text())
    architecture = {"aarch64": "arm64", "arm64": "arm64", "x86_64": "x86_64", "AMD64": "x86_64"}.get(platform.machine())
    operating_system = {"linux": "linux", "darwin": "macos"}.get(sys.platform)
    key = f"{architecture}-{operating_system}"
    if key not in pin["assets"]:
        raise RuntimeError(f"No pinned WASI SDK for {platform.machine()}/{sys.platform}")
    name = f"wasi-sdk-{pin['version']}-{key}"
    checksum = pin["assets"][key]
    destination = destination.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    installed = destination / name
    marker = installed / ".openwebide-checksum"
    if installed.exists():
        if not marker.is_file() or marker.read_text().strip() != checksum:
            raise RuntimeError(f"Existing SDK does not match the pinned installation: {installed}")
        if not (installed / "bin/clang").is_file() or not (installed / "share/wasi-sysroot/include/wasm32-wasip2/stdio.h").is_file():
            raise RuntimeError(f"Incomplete WASI SDK: {installed}")
        return installed
    with tempfile.TemporaryDirectory(prefix=".wasi-sdk-", dir=destination) as staging:
        staging = Path(staging)
        archive = staging / f"{name}.tar.gz"
        url = f"https://github.com/WebAssembly/wasi-sdk/releases/download/{pin['release']}/{archive.name}"
        urllib.request.urlretrieve(url, archive)
        with archive.open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != checksum:
            raise RuntimeError("WASI SDK archive checksum mismatch")
        with tarfile.open(archive) as stream:
            stream.extractall(staging, filter="data")
        extracted = staging / name
        if (extracted / "VERSION").read_text().splitlines()[0] != pin["version"]:
            raise RuntimeError("WASI SDK version does not match the pin")
        (extracted / marker.name).write_text(checksum + "\n")
        extracted.rename(installed)
    return installed


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    print(install(arguments.destination))
