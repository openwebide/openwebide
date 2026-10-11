#!/usr/bin/env python3
"""Validate release versions, extract changelog notes and package native bridges."""

import argparse
import hashlib
import re
import shutil
import tarfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
RELEASE_VERSION = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-(?P<channel>alpha|rc)\.(?:0|[1-9][0-9]*))?"
)
TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}


def release_channel(current):
    match = RELEASE_VERSION.fullmatch(current)
    if match is None:
        raise ValueError("Expected major.minor.patch, optionally followed by -alpha.N or -rc.N")
    return {'alpha': 'nightly', 'rc': 'preview', None: 'stable'}.get(match['channel'], match['channel'])


def version(root=ROOT):
    cargo = tomllib.loads((root / "Cargo.toml").read_text())
    spin = tomllib.loads((root / "spin.toml").read_text())
    current = cargo["workspace"]["package"]["version"]
    release_channel(current)
    if spin["application"]["version"] != current:
        raise ValueError("Cargo.toml and spin.toml versions differ")
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    for member in cargo["workspace"]["members"]:
        package = tomllib.loads((root / member / "Cargo.toml").read_text())["package"]
        declared = package["version"]
        independent_sdk = package["name"] == "openwebide-plugin-sdk"
        expected = declared if independent_sdk else current
        if independent_sdk:
            release_channel(expected)
        elif declared != {"workspace": True} and declared != current:
            raise ValueError(f"{member} does not use workspace version {current}")
        locked = [p for p in lock["package"] if p["name"] == package["name"] and "source" not in p]
        if len(locked) != 1 or locked[0]["version"] != expected:
            raise ValueError(f"Cargo.lock version for {package['name']} differs")
    return current


def notes(changelog, section, allow_empty=False):
    headings = list(re.finditer(r"^## \[([^\]]+)\].*$", changelog, re.MULTILINE))
    matching = [index for index, heading in enumerate(headings) if heading[1] == section]
    if len(matching) != 1:
        raise ValueError(f"Expected exactly one changelog section [{section}]")
    index = matching[0]
    start = headings[index].end()
    end = headings[index + 1].start() if index + 1 < len(headings) else len(changelog)
    body = changelog[start:end].strip()
    if not body and not allow_empty:
        raise ValueError(f"Changelog section [{section}] is empty")
    return body + "\n"


def validate(root, tag=None):
    current = version(root)
    if tag is not None and tag != f"v{current}":
        raise ValueError(f"Tag must match workspace version v{current}")
    section = current if tag is not None else "Unreleased"
    body = notes((root / "CHANGELOG.md").read_text(), section, allow_empty=tag is None)
    if tag is not None and len(body) > 125000:
        raise ValueError("Release notes exceed GitHub's 125000-character limit; link to the full changelog instead")
    return current, body


def package_bridge(root, target, binary, output):
    current = version(root)
    if target not in TARGETS:
        raise ValueError(f"Unsupported release target: {target}")
    if not binary.is_file() or binary.is_symlink():
        raise ValueError("Bridge binary must be a regular file")
    output.mkdir(parents=True, exist_ok=True)
    name = f"openwebide-bridge-v{current}-{target}"
    archive = output / f"{name}.tar.gz"
    if archive.exists():
        raise ValueError(f"Refusing to replace {archive}")
    with tarfile.open(archive, "w:gz") as bundle:
        for source, destination, mode in [
            (binary, "openwebide-bridge", 0o755),
            (root / "LICENSE", "LICENSE", 0o644),
            (root / "docs/execution-bridge.md", "execution-bridge.md", 0o644),
        ]:
            info = bundle.gettarinfo(str(source), arcname=f"{name}/{destination}")
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mode = mode
            with source.open("rb") as stream:
                bundle.addfile(info, stream)
    return archive


def checksums(directory):
    files = sorted(path for path in directory.iterdir() if path.is_file() and path.name != "SHA256SUMS")
    if not files:
        raise ValueError("No release assets to checksum")
    lines = []
    for path in files:
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        lines.append(f"{digest}  {path.name}\n")
    (directory / "SHA256SUMS").write_text("".join(lines))


def prepare_install_files(root, output):
    current = version(root)
    channel = release_channel(current)
    output.mkdir(parents=True, exist_ok=True)
    image = "ghcr.io/openwebide/openwebide:"
    for source, destination in [
        ("docker-compose.release.yml", "docker-compose.yml"),
        ("docker-compose.https.yml", "docker-compose.https.yml"),
        ("docker-compose.ssh.yml", "docker-compose.ssh.yml"),
        ("deploy/openwebide.image", "openwebide.image"),
        ("deploy/openwebide.container", "openwebide.container"),
        ("docker/tailscale/serve-config.sh", "serve-config.sh"),
    ]:
        path = output / destination
        if path.exists():
            raise ValueError(f"Refusing to replace {path}")
        shutil.copy2(root / source, path)
        if destination.endswith(".yml"):
            path.write_text(path.read_text().replace(image + "stable", image + "v" + current))
        elif destination == "openwebide.image":
            path.write_text(path.read_text().replace(image + "stable", image + channel))


def github_outputs(current):
    channel = release_channel(current)
    return f"version={current}\nchannel={channel}\nprerelease={str(channel != 'stable').lower()}\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("validate")
    check.add_argument("--tag")
    check.add_argument("--notes", type=Path)
    check.add_argument("--github-output", type=Path)
    package = sub.add_parser("package-bridge")
    package.add_argument("--target", required=True, choices=sorted(TARGETS))
    package.add_argument("--binary", required=True, type=Path)
    package.add_argument("--output", required=True, type=Path)
    checksum = sub.add_parser("checksums")
    checksum.add_argument("directory", type=Path)
    install = sub.add_parser("prepare-install-files")
    install.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        if args.command == "validate":
            current, body = validate(ROOT, args.tag)
            if args.notes:
                args.notes.write_text(body)
            if args.github_output:
                with args.github_output.open('a') as stream:
                    stream.write(github_outputs(current))
            print(current)
        elif args.command == "package-bridge":
            print(package_bridge(ROOT, args.target, args.binary, args.output))
        elif args.command == 'prepare-install-files':
            prepare_install_files(ROOT, args.output)
        else:
            checksums(args.directory)
    except (ValueError, OSError, KeyError) as error:
        parser.exit(1, f"Release validation failed: {error}\n")


if __name__ == "__main__":
    main()
