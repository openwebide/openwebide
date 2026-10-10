#!/usr/bin/env python3
"""Run every compiled editor WASM test in bounded, independently checked groups.

Use Cargo's current test artifacts and the official runner's test inventory.
Each group starts a fresh browser with the existing runner/readiness deadlines.
The expensive font matrices retain their independent runs. --check verifies the
complete selection plan without executing tests; --artifact accepts built WASM.
"""
import argparse
import json
from pathlib import Path
import shutil
import subprocess


MATRICES = {
    "bounded_paragraph_geometry_preserves_fonts_features_and_whitespace_in_both_modes",
    "paragraph_limit_geometry_matches_complete_rows_in_both_modes",
}
ROOT = Path(__file__).resolve().parent.parent
FRONTEND = ROOT / "frontend"


def inventory(runner, artifact, arguments=()):
    output = subprocess.check_output(
        [runner, str(artifact), "editor::", *arguments, "--list"], text=True, cwd=FRONTEND
    )
    names = [line.removesuffix(": test") for line in output.splitlines() if line.endswith(": test")]
    assert len(names) == len(set(names)), f"Duplicate test names in {artifact}"
    return set(names)


def artifacts():
    result = subprocess.run(
        ["cargo", "test", "-p", "openwebide-frontend", "--release",
         "--target", "wasm32-unknown-unknown", "--no-run", "--message-format=json"],
        stdout=subprocess.PIPE, text=True, check=True, cwd=ROOT,
    )
    files = set()
    for line in result.stdout.splitlines():
        record = json.loads(line)
        executable = record.get("executable")
        if record.get("reason") == "compiler-artifact" and record.get("profile", {}).get("test") and executable and executable.endswith(".wasm"):
            files.add(Path(executable))
    assert files, "Cargo returned no current frontend WASM test artifacts"
    return sorted(files)


def plan(runner, files, maximum):
    runs = []
    found_matrices = []
    total = 0
    for artifact in files:
        names = inventory(runner, artifact)
        total += len(names)
        matrices = {name for name in names if name.rsplit("::", 1)[-1] in MATRICES}
        found_matrices.extend(name.rsplit("::", 1)[-1] for name in matrices)
        ordinary = sorted(names - matrices)
        groups = (len(ordinary) + maximum - 1) // maximum
        assigned = set()
        for group in range(groups):
            selected = set(ordinary[group::groups])
            # The runner's --skip uses substring matching. Independently list
            # each actual selection so collisions cannot silently omit a test.
            arguments = [argument for name in sorted(names - selected) for argument in ("--skip", name)]
            actual = inventory(runner, artifact, arguments)
            assert actual == selected, f"Runner selection differs for {artifact}, group {group + 1}"
            assert not assigned & actual, "A test was assigned to multiple groups"
            assigned |= actual
            runs.append((artifact, ["editor::", *arguments], len(actual)))
        for name in sorted(matrices):
            output = subprocess.check_output([runner, str(artifact), name, "--exact", "--list"], text=True, cwd=FRONTEND)
            assert output.splitlines() == [f"{name}: test"], f"Matrix selection differs: {name}"
            assigned.add(name)
            runs.append((artifact, [name, "--exact"], 1))
        assert assigned == names, f"Some editor tests are unassigned in {artifact}"
    assert total, "No editor tests found"
    assert sorted(found_matrices) == sorted(MATRICES), "Each independent font matrix must appear exactly once"
    assert sum(count for _, _, count in runs) == total, "Incomplete editor test plan"
    return runs, total


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifact", type=Path, action="append")
    parser.add_argument("--max-tests", type=int, default=64)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    assert args.max_tests > 0, "Group size must be positive"
    runner = shutil.which("wasm-bindgen-test-runner")
    assert runner, "wasm-bindgen-test-runner is required"
    files = [path.resolve() for path in args.artifact] if args.artifact else artifacts()
    runs, total = plan(runner, files, args.max_tests)
    print(json.dumps({"editorTests": total, "browserRuns": len(runs), "planOnly": args.check}), flush=True)
    for artifact, arguments, count in runs:
        print(json.dumps({"artifact": str(artifact), "tests": count, "matrix": "--exact" in arguments}), flush=True)
        if not args.check:
            subprocess.run([runner, str(artifact), *arguments], check=True, cwd=FRONTEND)


if __name__ == "__main__":
    main()
