"""A failed browser group must not suppress the remaining verified inventory."""
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "editor_browser_runner", Path(__file__).with_name("run-editor-browser-tests.py")
)
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class EditorBrowserRunnerTests(unittest.TestCase):
    def test_export_stages_every_current_wasm_test_from_cargo(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            files = [root / "frontend-a.wasm", root / "components-b.wasm"]
            for artifact in files:
                artifact.write_bytes(artifact.name.encode())
            records = [
                {"reason": "compiler-artifact", "profile": {"test": True}, "executable": str(artifact)}
                for artifact in files
            ]
            records.append({"reason": "compiler-artifact", "profile": {"test": False}, "executable": "app.wasm"})
            destination = root / "export"
            result = subprocess.CompletedProcess([], 0, "\n".join(map(json.dumps, records)))
            with patch("sys.argv", ["runner", "--export-artifacts", str(destination)]), \
                    patch.object(RUNNER.subprocess, "run", return_value=result) as run:
                RUNNER.main()
            run.assert_called_once()
            self.assertIn("--no-run", run.call_args.args[0])
            self.assertEqual(
                {artifact.name: artifact.read_bytes() for artifact in destination.iterdir()},
                {artifact.name: artifact.read_bytes() for artifact in files},
            )

    def test_diagnostics_preserve_commands_and_continue_after_replay_failure(self):
        local = "editor::wrapped_paragraph_edits_reuse_exact_source_owned_geometry_local"
        remote = "editor::wrapped_paragraph_edits_reuse_exact_source_owned_geometry_remote"
        groups = [(Path("tests.wasm"), ["editor::", "--skip", "second"], 1),
                  (Path("tests.wasm"), [local, "--exact"], 1),
                  (Path("tests.wasm"), [remote, "--exact"], 1)]
        with tempfile.TemporaryDirectory(prefix="editor driver logs ") as directory, \
                patch("sys.argv", ["runner", "--artifact", "tests.wasm", "--diagnostics-dir", directory]), \
                patch.dict(os.environ, {"CHROMEDRIVER_ARGS": "--allowed-ips=127.0.0.1", "WASM_BINDGEN_TEST_TIMEOUT": "300"}), \
                patch.object(RUNNER.shutil, "which", return_value="runner"), \
                patch.object(RUNNER, "plan", return_value=(groups, 3)), \
                patch.object(RUNNER.subprocess, "run", side_effect=[
                    subprocess.CompletedProcess([], 0),
                    subprocess.CompletedProcess([], 1),
                    subprocess.CompletedProcess([], 0),
                ]) as run, \
                patch("builtins.print"):
            with self.assertRaisesRegex(SystemExit, "1 editor browser runs failed"):
                RUNNER.main()
            self.assertEqual(run.call_count, 3)
            self.assertIsNone(run.call_args_list[0].kwargs["env"])
            paths = []
            for call, name in zip(run.call_args_list[1:], [local, remote]):
                self.assertEqual(call.args[0], ["runner", "tests.wasm", name, "--exact"])
                environment = call.kwargs["env"]
                self.assertEqual(environment["WASM_BINDGEN_TEST_TIMEOUT"], "300")
                arguments = shlex.split(environment["CHROMEDRIVER_ARGS"])
                self.assertIn("--allowed-ips=127.0.0.1", arguments)
                self.assertIn("--verbose", arguments)
                path = Path(next(arg.removeprefix("--log-path=") for arg in arguments if arg.startswith("--log-path=")))
                self.assertEqual(path.parent, Path(directory).resolve())
                paths.append(path)
            self.assertEqual(len(set(paths)), 2)

    def test_failure_runs_remaining_groups_and_still_fails(self):
        groups = [(Path("tests.wasm"), ["editor::", "--skip", "second"], 1),
                  (Path("tests.wasm"), ["second", "--exact"], 1)]
        with patch("sys.argv", ["runner", "--artifact", "tests.wasm"]), \
                patch.object(RUNNER.shutil, "which", return_value="runner"), \
                patch.object(RUNNER, "plan", return_value=(groups, 2)), \
                patch.object(RUNNER.subprocess, "run", side_effect=[
                    subprocess.CompletedProcess([], 1),
                    subprocess.CompletedProcess([], 0),
                ]) as run, \
                patch("builtins.print"):
            with self.assertRaisesRegex(SystemExit, "1 editor browser runs failed"):
                RUNNER.main()
        self.assertEqual(run.call_count, 2)
        self.assertEqual(run.call_args_list[1].args[0],
                         ["runner", "tests.wasm", "second", "--exact"])

    def test_sharding_partitions_runs_completely_and_disjointly(self):
        groups = [
            (Path("tests.wasm"), ["editor::", "--skip", "a"], 10),
            (Path("tests.wasm"), ["editor::", "--skip", "b"], 10),
            (Path("tests.wasm"), ["first_matrix", "--exact"], 1),
            (Path("tests.wasm"), ["second_matrix", "--exact"], 1),
        ]
        executed = []
        for shard in ("1/2", "2/2"):
            with patch("sys.argv", ["runner", "--artifact", "tests.wasm", "--shard", shard]), \
                    patch.object(RUNNER.shutil, "which", return_value="runner"), \
                    patch.object(RUNNER, "plan", return_value=(groups, 22)), \
                    patch.object(RUNNER.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)) as run, \
                    patch("builtins.print"):
                RUNNER.main()
            executed.extend(call.args[0] for call in run.call_args_list)
        self.assertEqual(len(executed), 4)
        self.assertCountEqual(
            executed,
            [["runner", "tests.wasm", *arguments] for _, arguments, _ in groups],
        )


if __name__ == "__main__":
    unittest.main()
