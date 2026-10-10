"""Memory snapshots retain complete process coverage across Chrome churn."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "editor_measurements", Path(__file__).with_name("measure-editor-view.py")
)
measurements = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measurements)


class MemorySnapshotTests(unittest.TestCase):
    def test_complete_snapshot_includes_descendants_and_excludes_driver_and_neighbors(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n3 2 20\n4 0 40\n"), \
                patch.object(Path, "read_text", autospec=True, side_effect=lambda path: f"Pss: {int(path.parts[2]) * 10} kB\n"):
            result = measurements.process_memory(1)
        self.assertEqual(result, {"chrome_processes": 2, "chrome_summed_rss_kib": 30,
                                  "chrome_pss_kib": 50, "memorySnapshotAttempts": 1})

    def test_retired_process_requires_a_fresh_complete_snapshot(self):
        with patch.object(measurements.subprocess, "check_output", side_effect=["1 0 5\n2 1 10\n", "1 0 5\n3 1 20\n"]), \
                patch.object(Path, "read_text", side_effect=[FileNotFoundError(), "Pss: 30 kB\n"]), \
                patch.object(Path, "is_dir", return_value=True), \
                patch.object(measurements.time, "sleep") as sleep:
            result = measurements.process_memory(1)
        self.assertEqual(result, {"chrome_processes": 1, "chrome_summed_rss_kib": 20,
                                  "chrome_pss_kib": 30, "memorySnapshotAttempts": 2})
        sleep.assert_called_once_with(.01)

    def test_persistent_unreadable_process_never_reports_partial_pss_as_complete(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n3 1 20\n"), \
                patch.object(Path, "read_text", side_effect=["Pss: 10 kB\n", PermissionError()] * 3), \
                patch.object(Path, "is_dir", return_value=True), \
                patch.object(measurements.time, "sleep"):
            result = measurements.process_memory(1)
        self.assertIsNone(result["chrome_pss_kib"])
        self.assertEqual(result["chrome_processes"], 2)
        self.assertEqual(result["memorySnapshotAttempts"], 3)

    def test_hosts_without_proc_keep_pss_unavailable_without_retrying(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n"), \
                patch.object(Path, "read_text", side_effect=FileNotFoundError()), \
                patch.object(Path, "is_dir", return_value=False), \
                patch.object(measurements.time, "sleep") as sleep:
            result = measurements.process_memory(1)
        self.assertIsNone(result["chrome_pss_kib"])
        self.assertEqual(result["memorySnapshotAttempts"], 1)
        sleep.assert_not_called()
