"""Fictional Git and language responses for the documentation capture origin."""

from difflib import unified_diff
import hashlib
import json
import re


DEMO_REPOSITORY = "https://example.invalid/harbor-language-tools.git"


def rust_provider():
    return {
        "source": {"repository": DEMO_REPOSITORY, "path": "rust", "commit": "a" * 40},
        "digest": "b" * 64,
        "host_id": "demo-host",
        "manifest": {
            "schemaVersion": 1, "publisher": "example", "name": "demo-rust", "version": "0.1.0",
            "displayName": "Demo Rust", "description": "Syntax highlighting for Rust source files",
            "license": "MIT", "compatibility": {"pluginApi": 5},
            "executable": {"manifest": "Cargo.toml", "library": "demo_rust", "sdkVersion": "0.1.0", "capabilities": []},
            "contributions": {"languages": [{"id": "rust", "name": "Rust", "extensions": ["rs"],
                                             "features": {"syntax": True}}]},
        },
    }


def plugin_marketplaces(provider, timestamp):
    source = {"repository": DEMO_REPOSITORY, "reference": "", "path": "marketplace.json"}
    rust = provider["manifest"]
    entries = [
        {
            **{key: rust[key] for key in ("publisher", "name", "displayName", "description")},
            "categories": ["languages"],
            "releases": [{"version": rust["version"], "source": {"commit": "a" * 40, "path": "rust"}}],
        },
        {
            "publisher": "example", "name": "demo-toml", "displayName": "Demo TOML",
            "description": "Syntax highlighting for TOML configuration files",
            "categories": ["languages"],
            "releases": [{"version": "0.1.0", "source": {"commit": "a" * 40, "path": "toml"}}],
        },
    ]
    return {"revision": 1, "sources": [source], "catalogs": [{
        "source": source, "commit": "a" * 40, "fetched_at": timestamp,
        "catalog": {"schemaVersion": 1, "name": "Harbor Language Tools", "plugins": entries},
    }]}


# A vocabulary for this exact fixture, rather than an implementation of Rust parsing.
RUST_TOKENS = re.compile(
    r"(?P<Comment>//.*)|(?P<Attribute>#\[[^\]]+\])|"
    r"(?P<Keyword>\b(?:use|crate|pub|fn|if|return|let|as|mod|super)\b)|"
    r"(?P<Type>\b(?:Reading|Option|f64|None|Some)\b)|"
    r"(?P<Function>\b(?:average_temperature|empty_forecast_has_no_average)\b)|"
    r"(?P<Macro>\bassert_eq!)"
)


def token_rows(source):
    rows = []
    for line in source.split("\n"):
        spans = []
        previous = 0
        for token in RUST_TOKENS.finditer(line):
            if previous < token.start():
                spans.append({"end": len(line[:token.start()].encode()), "kind": "Plain"})
            spans.append({"end": len(line[:token.end()].encode()), "kind": token.lastgroup})
            previous = token.end()
        if previous < len(line):
            spans.append({"end": len(line.encode()), "kind": "Plain"})
        rows.append(spans)
    return rows


def rust_invocation(request, path, source):
    arguments = json.loads(request["call"]["arguments"])
    output = {key: arguments[key] for key in ("version", "document")}
    if arguments["operation"] == "close":
        output["operation"] = "closed"
    else:
        output.update({key: arguments[key] for key in ("request", "revision", "language", "sourceDigest")})
        ready = (arguments["path"] == path and arguments["language"] == "rust"
                 and arguments["sourceDigest"] == hashlib.sha256(source).hexdigest())
        output.update(operation="analyzed", status={"type": "unsupported"}, analysis=None)
        if ready:
            output.update(status={"type": "ready", "incremental": False},
                          analysis={"highlights": token_rows(source.decode()), "folds": [], "structure": None})
    return {"id": "2" * 32, "step": {"status": "complete", "ok": True, "content": json.dumps(output), "summary": ""}}


COMMITS = [("a4c9b12" + "0" * 33, "Add harbor weather readings", "Maya Chen"),
           ("d46e2a1" + "0" * 33, "Define the station reading", "Robin Ellis"),
           ("b193f81" + "0" * 33, "Start the forecast service", "Maya Chen")]


def git_history():
    commits = []
    for index, (commit, subject, author) in enumerate(COMMITS):
        date = f"2026-11-{14 - index:02}T12:00:00Z"
        commits.append({
            "hash": commit, "parents": [COMMITS[index + 1][0]] if index + 1 < len(COMMITS) else [],
            "author": author, "author_email": "team@example.invalid", "authored_at": date,
            "committer": author, "committer_email": "team@example.invalid", "committed_at": date,
            "subject": subject, "message": subject, "refs": ["main"] if index == 0 else [],
        })
    return {"commits": commits, "has_more": False,
            "refs": [{"name": "main", "hash": COMMITS[0][0], "kind": "branch"}]}


def git_commit_diff(path, before, after):
    diff = unified_diff(before.splitlines(keepends=True), after.splitlines(keepends=True), f"a/{path}", f"b/{path}")
    return {"truncated": False, "files": [{"path": path, "status": "M", "previous_path": None}],
            "diff": f"diff --git a/{path} b/{path}\n" + "".join(diff)}


def pending_monitor(project_id, timestamp):
    return {
        "id": 7, "revision": 1, "project_id": project_id, "next_run": timestamp,
        "host_id": "demo-host", "host_available": True, "last_run": None,
        "draft": {
            "title": "Monitor", "prompt": "Check whether the forecast service tests passed.",
            "session_id": 1, "session_target": "existing", "enabled": True,
            "schedule": {"kind": "once", "at": timestamp},
        },
    }
