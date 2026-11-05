#!/usr/bin/env python3
"""Install the checksum-pinned C# server without personal NuGet feeds or tools."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request
import xml.etree.ElementTree as ET

TOOLS = Path(__file__).resolve().parent / "csharp-language-server"


def install(tool_path, package=None):
    versions = json.loads((TOOLS / "versions.json").read_text())
    sdks = subprocess.check_output(["dotnet", "--list-sdks"], text=True)
    if not any(line.startswith("10.") for line in sdks.splitlines()):
        raise RuntimeError("Install the .NET 10 SDK before setting up csharp-ls")
    tool_path = tool_path.resolve()
    binary = tool_path / ("csharp-ls.exe" if os.name == "nt" else "csharp-ls")
    with tempfile.TemporaryDirectory(prefix="openwebide-csharp-tools-") as directory:
        state = Path(directory)
        feed = state / "feed"
        feed.mkdir()
        archive = feed / f"csharp-ls.{versions['server']}.nupkg"
        if package is not None:
            shutil.copyfile(package, archive)
        else:
            with urllib.request.urlopen(versions["package"], timeout=30) as response:
                with archive.open("wb") as destination:
                    shutil.copyfileobj(response, destination)
        with archive.open("rb") as source:
            actual = hashlib.file_digest(source, "sha512").hexdigest()
        if actual != versions["sha512"]:
            raise RuntimeError("The csharp-ls package checksum does not match the pinned release")
        # A local feed containing only the verified, dependency-free tool package
        # makes installation independent of user credentials and registry config.
        configuration = ET.Element("configuration")
        sources = ET.SubElement(configuration, "packageSources")
        ET.SubElement(sources, "clear")
        ET.SubElement(sources, "add", {"key": "verified-release", "value": str(feed)})
        config = state / "NuGet.Config"
        ET.ElementTree(configuration).write(config, encoding="utf-8", xml_declaration=True)
        environment = {
            **os.environ,
            "DOTNET_CLI_HOME": str(state / "cli"),
            "NUGET_PACKAGES": str(state / "packages"),
            "DOTNET_CLI_TELEMETRY_OPTOUT": "1",
            "DOTNET_NOLOGO": "1",
            "DOTNET_SKIP_FIRST_TIME_EXPERIENCE": "1",
            "DOTNET_GENERATE_ASPNET_CERTIFICATE": "false",
        }
        operation = "update" if binary.exists() else "install"
        subprocess.run([
            "dotnet", "tool", operation, "csharp-ls", "--version", versions["server"],
            "--tool-path", str(tool_path), "--configfile", str(config),
        ], cwd=state, env=environment, check=True)
    # Check after removing installation-only state: the deployed tool must not
    # depend on that temporary NuGet cache or CLI home.
    result = subprocess.check_output([str(binary), "--version"], text=True)
    if not re.search(rf"\b{re.escape(versions['server'])}(?:\.0)?\b", result):
        raise RuntimeError("Installed csharp-ls reports a different version")
    print(result.strip())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tool-path", type=Path, default=TOOLS / "installed")
    parser.add_argument("--package", type=Path, help="Use a cached copy of the pinned NuGet package")
    args = parser.parse_args()
    install(args.tool_path, args.package)


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"C# server setup failed: {error}", file=sys.stderr)
        sys.exit(1)
