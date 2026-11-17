"""Bind portable compiled defaults to the source commit that produced them."""

import argparse
import hashlib
import json
from pathlib import Path
import re


def provenance(bundle):
    return bundle.with_suffix(".provenance.json")


def checksum(bundle):
    with bundle.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def transfer(bundle, commit, *, seal=False):
    if re.fullmatch(r"[0-9a-f]{40}", commit) is None:
        raise ValueError("Compiled defaults require a full source commit")
    expected = {"commit": commit, "sha256": checksum(bundle)}
    if seal:
        provenance(bundle).write_text(json.dumps(expected, indent=2) + "\n")
    elif json.loads(provenance(bundle).read_text()) != expected:
        raise ValueError("Compiled defaults do not match this commit and checksum")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("seal", "verify"))
    parser.add_argument("bundle", type=Path)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    transfer(args.bundle, args.commit, seal=args.operation == "seal")
