#!/usr/bin/env python3
"""Build Pages from a temporary snapshot; never maintain copies of the docs."""

import argparse
import re
import os
import shutil
import subprocess
import sys
import tempfile
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

import yaml
from navigation import validate_navigation

from credits import render as render_credits

ROOT = Path(__file__).resolve().parent.parent
REPO_URL = "https://github.com/openwebide/openwebide"


def rewrite_readme_links(text):
    """Only the repository root README becomes the site's getting-started page."""
    return re.sub(r"\]\(((?:\.\./)*|(?:\./)?)README\.md(?=[)#])", r"](\1getting-started.md", text)


class PageLinks(HTMLParser):
    def __init__(self, path):
        super().__init__()
        self.ids = set()
        self.links = []
        self.feed(path.read_text())

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if "id" in attributes:
            self.ids.add(attributes["id"])
        for key in ("href", "src"):
            if key in attributes:
                self.links.append(attributes[key])


def validate_links(output):
    pages = {path: PageLinks(path) for path in output.rglob("*.html")}
    errors = []
    for path, page in pages.items():
        for link in page.links:
            url = urlsplit(link)
            if url.scheme or url.netloc:
                continue
            target = (path.parent / unquote(url.path)).resolve() if url.path else path
            if target.is_dir():
                target /= "index.html"
            if not target.is_relative_to(output) or not target.exists():
                errors.append(f"{path.relative_to(output)}: missing asset/page {link}")
            elif url.fragment and target in pages and unquote(url.fragment) not in pages[target].ids:
                errors.append(f"{path.relative_to(output)}: missing anchor {link}")
    if errors:
        raise ValueError("Broken generated site links:\n" + "\n".join(errors))


def shared_styles():
    css = (ROOT / "frontend/styles.css").read_text()
    selectors = [":root", '.btn', '.btn:hover:not(:disabled)', '.btn:focus-visible', '.btn.send']
    rules = []
    for selector in selectors:
        match = re.search("^" + re.escape(selector) + r" \{[^}]*\}", css, re.MULTILINE)
        if match is None:
            raise ValueError(f"Missing shared style: {selector}")
        rules.append(match.group())
    light = re.search(r':root\[data-theme="light"\] \{([^}]*)\}', css)
    if light is None:
        raise ValueError("Missing shared light palette")
    rules.append("@media (prefers-color-scheme: light) { :root {" + light[1] + "} }")
    return "\n".join(rules)


def build_playground(destination):
    env = dict(os.environ)
    env["NO_COLOR"] = "true"
    common = Path(subprocess.check_output(["git", "rev-parse", "--git-common-dir"], cwd=ROOT, text=True).strip())
    if not common.is_absolute():
        common = ROOT / common
    env.setdefault("CARGO_TARGET_DIR", str(common.resolve().parent / "target"))
    subprocess.run(["trunk", "build", "--release", "--config", str(ROOT / "frontend/playground/Trunk.toml"), "--dist", str(destination), "--public-url", "./"], cwd=ROOT, env=env, check=True)


def build(output, site_url, playground_dist=None):
    output = output.resolve()
    marker = output / ".openwebide-site"
    legacy_marker = output / ".open-webide-site"
    if output.exists() and any(output.iterdir()) and not (marker.is_file() or legacy_marker.is_file()):
        raise ValueError(f"Refusing to replace non-site output directory: {output}")
    with tempfile.TemporaryDirectory(prefix="openwebide-docs-") as temporary:
        staging = Path(temporary)
        if playground_dist is None:
            playground_dist = staging / "playground"
            build_playground(playground_dist)
        if not (playground_dist / "index.html").is_file() or not list(playground_dist.glob("*.wasm")):
            raise ValueError("The panel playground must be a compiled Rust/WASM bundle")
        content = staging / "content"
        shutil.copytree(ROOT / "docs", content / "docs")
        credits = content / "docs/open-source-software.md"
        credits.write_text(credits.read_text().replace("<!-- dependency-inventory -->", render_credits(ROOT)))
        for name in ("README.md", "CHANGELOG.md", "LICENSE"):
            shutil.copy2(ROOT / name, content / name)
        shutil.copy2(ROOT / "site/index.md", content / "index.md")
        shutil.copytree(ROOT / "site/assets", content / "assets")
        shutil.copytree(playground_dist, content / "tools/panel-playground")
        (content / "fonts").mkdir()
        for font in (ROOT / "frontend/fonts").glob("*.woff2"):
            shutil.copy2(font, content / "fonts" / font.name)
        # Preserve README image paths without bundling the application itself.
        (content / "frontend/pwa").mkdir(parents=True)
        shutil.copy2(ROOT / "frontend/pwa/favicon.svg", content / "frontend/pwa/favicon.svg")
        (content / "assets/shared.css").write_text(shared_styles())
        # MkDocs treats README.md as an index; reserve index.md for the homepage.
        (content / "README.md").rename(content / "getting-started.md")
        for path in content.rglob("*.md"):
            path.write_text(rewrite_readme_links(path.read_text()))

        navigation = validate_navigation(
            yaml.safe_load((ROOT / "site/navigation.yml").read_text()), content
        )
        config = {
            "site_name": "Open WebIDE",
            "site_description": "A self-hosted browser IDE for coding with your own models.",
            "site_url": site_url,
            "repo_url": REPO_URL,
            "use_directory_urls": False,
            "docs_dir": str(content),
            "site_dir": str(output),
            "theme": {"name": None, "custom_dir": str(ROOT / "site/theme")},
            "plugins": [],
            "markdown_extensions": ["fenced_code", "tables", "toc", "attr_list", "md_in_html"],
            "validation": {"links": {"not_found": "warn", "anchors": "warn"}},
            "nav": navigation,
        }
        config_path = staging / "mkdocs.yml"
        config_path.write_text(yaml.safe_dump(config, sort_keys=False))
        try:
            subprocess.run([sys.executable, "-m", "mkdocs", "build", "--strict", "--config-file", str(config_path)], check=True)
        finally:
            if output.is_dir():
                marker.touch()
        # Copy discovery files explicitly: dot directories must survive the build.
        shutil.copytree(ROOT / "site/static", output, dirs_exist_ok=True)
        validate_links(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="Generated output directory (outside the repository)")
    parser.add_argument("--site-url", default="https://openwebide.com/")
    parser.add_argument("--playground-dist", type=Path, help="Reuse an already compiled playground bundle")
    args = parser.parse_args()
    destination = args.output.resolve()
    if destination == ROOT or ROOT in destination.parents or destination in ROOT.parents:
        parser.error("Use an output directory outside the repository to keep generated files out of source control")
    build(destination, args.site_url, args.playground_dist)
