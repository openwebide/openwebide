"""Remove cached wasm-bindgen snippets not referenced by this build's modules.

wasm-bindgen emits literal module paths. Trunk can reuse a binding output
directory across binaries/features and copy obsolete snippets into staging.
The generated service-worker cache list is output, not a dependency oracle.
"""
from pathlib import Path
from html.parser import HTMLParser
import re
import sys
from urllib.parse import urlsplit


class SnippetPreloads(HTMLParser):
    def __init__(self, text, root, removed):
        super().__init__()
        self.root = root
        self.removed = removed
        self.ranges = []
        self.offsets = [0]
        for line in text.splitlines(keepends=True):
            self.offsets.append(self.offsets[-1] + len(line))

    def handle_starttag(self, tag, attributes):
        attributes = dict(attributes)
        if tag != "link" or "modulepreload" not in attributes.get("rel", "").lower().split():
            return
        reference = urlsplit(attributes.get("href", ""))
        if reference.scheme or reference.netloc:
            return
        target = (self.root / reference.path.lstrip("/")).resolve()
        if target in self.removed:
            line, column = self.getpos()
            start = self.offsets[line - 1] + column
            self.ranges.append((start, start + len(self.get_starttag_text())))


def prune(directory):
    root = Path(directory).resolve()
    snippets = root / "snippets"
    if not snippets.is_dir():
        return
    references = re.compile(r"(['\"`])((?:\.{1,2}/|/)[^'\"`\r\n]+\.js(?:[?#][^'\"`\r\n]*)?)\1")
    manifest = root / "service-worker.js"
    pending = [path.resolve() for path in root.glob("*.js") if path != manifest]
    reachable = set()
    while pending:
        module = pending.pop()
        if module in reachable or module == manifest or not module.is_relative_to(root):
            continue
        reachable.add(module)
        for _, reference in references.findall(module.read_text()):
            path = urlsplit(reference).path
            target = (root / path.lstrip("/") if path.startswith("/") else module.parent / path).resolve()
            if target.is_relative_to(root) and target.is_file():
                pending.append(target)
    removed = set()
    for path in snippets.rglob("*.js"):
        if path.resolve() not in reachable:
            removed.add(path.resolve())
            path.unlink()
    index = root / "index.html"
    if removed and index.is_file():
        text = index.read_text()
        preloads = SnippetPreloads(text, root, removed)
        preloads.feed(text)
        for start, end in reversed(preloads.ranges):
            text = text[:start] + text[end:]
        index.write_text(text)


if __name__ == "__main__":
    prune(sys.argv[1])
