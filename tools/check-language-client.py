#!/usr/bin/env python3
"""Reject syntax engines, plugin runtimes and parser workers in the production client."""
import argparse
import json
import re
from html.parser import HTMLParser
from pathlib import Path
import subprocess
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
HOST_PACKAGES = {
    "tree-sitter", "openwebide-language-grammars", "openwebide-language-syntax",
    "openwebide-plugin-runtime", "wasmtime",
}


class ModulePreloads(HTMLParser):
    def __init__(self):
        super().__init__()
        self.references = []

    def handle_starttag(self, tag, attributes):
        attributes = dict(attributes)
        if tag == "link" and "modulepreload" in attributes.get("rel", "").lower().split():
            self.references.append(attributes.get("href", ""))


def check(dependencies_only=False):
    output = subprocess.check_output([
        "cargo", "tree", "-p", "openwebide-frontend", "--locked",
        "--target", "wasm32-unknown-unknown", "--edges", "normal,build",
        "--prefix", "none", "--format", "{p}",
    ], cwd=ROOT, text=True)
    packages = {line.split()[0] for line in output.splitlines() if line.strip()}
    forbidden = sorted(name for name in packages
                       if name in HOST_PACKAGES or name.startswith(("tree-sitter-", "wasmtime-")))
    assert not forbidden, f"Host execution dependencies in the production frontend: {forbidden}"
    if not dependencies_only:
        dist = ROOT / "frontend/dist"
        assert (dist / "index.html").is_file(), "Build the production frontend first"
        preloads = ModulePreloads()
        preloads.feed((dist / "index.html").read_text())
        for reference in preloads.references:
            location = urlsplit(reference)
            if not location.scheme and not location.netloc:
                assert (dist / location.path.lstrip("/")).is_file(), f"Missing preloaded module: {reference}"
        binaries = list(dist.glob("openwebide-frontend-*_bg.wasm"))
        assert binaries, "Missing production frontend WASM"
        assert all(not re.search(br"openwebide-editor-ready:[0-9]+:frames:[0-9]+", binary.read_bytes())
                   for binary in binaries), "Test parser worker in production frontend WASM"
        modules = list(dist.glob("openwebide-frontend-*.js"))
        assert modules, "Missing production frontend module"
        assert all("editor_worker_bundle_url" not in module.read_text()
                   for module in dist.rglob("*.js")), "Test syntax engine in production frontend module"
        assert not list(dist.glob("editor-worker*.js")), "Parser worker bootstrap in production output"
        assert "editor-worker" not in (dist / "service-worker.js").read_text(), "Parser worker in PWA cache"
    return {"target": "wasm32-unknown-unknown", "packages": len(packages),
            "hostExecutionDependencies": forbidden, "artifactsChecked": not dependencies_only}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dependencies-only", action="store_true",
                        help="Inspect Cargo's production graph before the frontend bundle is built")
    args = parser.parse_args()
    print(json.dumps(check(args.dependencies_only)))
