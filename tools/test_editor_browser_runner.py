"""A failed browser group must not suppress the remaining verified inventory."""
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "editor_browser_runner", Path(__file__).with_name("run-editor-browser-tests.py")
)
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class EditorBrowserRunnerTests(unittest.TestCase):
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


if __name__ == "__main__":
    unittest.main()
