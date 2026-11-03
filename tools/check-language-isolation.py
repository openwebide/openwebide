#!/usr/bin/env python3
"""Keep each language component's Cargo graph free of other families' parsers."""
import argparse
from pathlib import Path
import re
import subprocess

FAMILIES = {
    "rust": {"tree-sitter-rust"},
    "javascript-typescript": {"tree-sitter-javascript", "tree-sitter-typescript"},
    "python": {"tree-sitter-python"},
    "java": {"tree-sitter-java"},
    "csharp": {"tree-sitter-c-sharp"},
    "c-cpp": {"tree-sitter-c", "tree-sitter-cpp"},
    "php": {"tree-sitter-php"},
    "shell": {"tree-sitter-bash"},
    "go": {"tree-sitter-go"},
    "html": {"tree-sitter-html"},
    "css": {"tree-sitter-css"},
    "json": {"tree-sitter-json"},
    "yaml": {"tree-sitter-yaml"},
    "toml": {"tree-sitter-toml-ng"},
    "markdown": {"tree-sitter-md"},
    "ini": {"tree-sitter-ini"},
    "xml": {"tree-sitter-xml"},
    "sql": set(),
    "plain": set(),
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--clippy", action="store_true", help="also compile and lint each family")
    parser.add_argument("--tests", action="store_true", help="also run the public protocol contracts for each family")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    for family, expected in FAMILIES.items():
        selection = ["-p", "openwebide-language-syntax", "--no-default-features", "--features", family]
        graph = subprocess.check_output(
            ["cargo", "tree", *selection, "--edges", "normal", "--prefix", "none"],
            cwd=root, text=True,
        )
        actual = set(re.findall(r"^(tree-sitter-[\w-]+) v", graph, re.MULTILINE)) - {"tree-sitter-language"}
        if actual != expected:
            raise SystemExit(f"{family}: expected parsers {sorted(expected)}, resolved {sorted(actual)}")
        if args.clippy:
            subprocess.run(["cargo", "clippy", *selection, "--all-targets", "--", "-D", "warnings"], cwd=root, check=True)
        if args.tests:
            subprocess.run(["cargo", "test", *selection, "--lib"], cwd=root, check=True)
        print(f"{family}: isolated ({', '.join(sorted(actual)) or 'no parser grammar'})", flush=True)


if __name__ == "__main__":
    main()
