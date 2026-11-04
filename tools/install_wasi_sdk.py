#!/usr/bin/env python3
"""Install the pinned public C/C++ toolchain for sandboxed WASI plugin builds."""
import argparse
import copy
import hashlib
import json
import os
import platform
from pathlib import Path
import sys
import tarfile
import tempfile
import urllib.request

HAS_EXTRACTION_FILTER = callable(getattr(tarfile, "data_filter", None))


def extract_sdk_archive(stream, staging: Path, name: str):
    if HAS_EXTRACTION_FILTER:
        stream.extractall(staging, filter="data")
        return
    # Debian 12 ships Python 3.11.2, before extraction filters were backported.
    # Check each member against the live filesystem before extracting it, so
    # earlier archive symlinks cannot redirect a later write out of the SDK.
    root = (staging / name).resolve()

    def checked(member):
        if Path(member.name).is_absolute():
            raise ValueError("WASI SDK archive contains an absolute path")
        target = (staging / member.name).resolve()
        if not target.is_relative_to(root):
            raise ValueError("WASI SDK archive path escapes its root")
        if not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
            raise ValueError("Unsupported WASI SDK archive entry")
        if member.issym() or member.islnk():
            if Path(member.linkname).is_absolute():
                raise ValueError("WASI SDK archive contains an absolute link")
            # The existing final component might itself be a symlink that this
            # entry replaces; relative links use its original directory.
            parent = (staging / member.name).parent.resolve() if member.issym() else staging
            link = (parent / member.linkname).resolve()
            if not link.is_relative_to(root):
                raise ValueError("WASI SDK archive link escapes its root")
        result = copy.copy(member)
        result.mode = member.mode & 0o755
        result.uid, result.gid = os.getuid(), os.getgid()
        result.uname = result.gname = ""
        return result

    links = []
    for member in stream:
        if member.islnk():
            checked(member)
            links.append(member)
        else:
            stream.extract(checked(member), staging)
    # Resolve forward hard links only after their checked file targets exist;
    # tarfile's recursive forward-link extraction would bypass these checks.
    while links:
        pending = []
        for member in links:
            member = checked(member)
            if (staging / member.linkname).is_file():
                stream.extract(member, staging)
            else:
                pending.append(member)
        if len(pending) == len(links):
            raise ValueError("WASI SDK archive contains an unresolved hard link")
        links = pending


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
            extract_sdk_archive(stream, staging, name)
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
