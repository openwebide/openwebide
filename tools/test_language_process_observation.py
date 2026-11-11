from pathlib import Path
import io
import json
import runpy
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from language_process_observation import process_residency, service_tree


class ProcessObservationTests(unittest.TestCase):
    def test_watcher_owned_server_and_typescript_children_are_one_service(self):
        rows = {10: (1, 100, '/app/openwebide-bridge'),
                20: (10, 30, '/app/openwebide-bridge'),
                21: (20, 200, '/tools/node'),
                22: (21, 300, '/tools/node'),
                23: (21, 5, '/bin/sh'),
                24: (23, 400, '/tools/node'),
                90: (1, 999, '/tools/node')}
        sample = process_residency(rows, 10, 123)
        self.assertEqual(sample['server_pids'], [21])
        self.assertEqual(sample['bridge_bytes'], 100)
        self.assertEqual(sample['children_bytes'], 935)
        self.assertEqual(service_tree(sample, {21}, 10), {20, 21, 22, 23, 24})
        self.assertEqual(sample['time'], 123)

    def test_direct_server_still_works_and_unrelated_node_is_excluded(self):
        rows = {10: (1, 100, 'openwebide-bridge'), 11: (10, 200, 'node'),
                12: (11, 300, 'node'), 90: (1, 999, 'node')}
        sample = process_residency(rows, 10, 0)
        self.assertEqual(sample['server_pids'], [11])
        self.assertEqual(service_tree(sample, {11}, 10), {11, 12})
        self.assertEqual(sample['children_bytes'], 500)

    def test_selecting_one_mode_does_not_include_another_watchers_service(self):
        rows = {10: (1, 100, 'openwebide-bridge'), 20: (10, 30, 'openwebide-bridge'),
                21: (20, 200, 'node'), 22: (21, 300, 'node'),
                30: (10, 40, 'openwebide-bridge'), 31: (30, 400, 'node'),
                32: (31, 500, 'node')}
        sample = process_residency(rows, 10, 0)
        self.assertEqual(sample['server_pids'], [21, 31])
        self.assertEqual(service_tree(sample, {31}, 10), {30, 31, 32})

    def test_missing_host_and_missing_selected_server_reject_measurement(self):
        with self.assertRaises(AssertionError):
            process_residency({}, 10, 0)
        sample = process_residency({10: (1, 100, 'bridge')}, 10, 0)
        with self.assertRaises(AssertionError):
            service_tree(sample, {21}, 10)
        with self.assertRaises(AssertionError):
            service_tree(sample, set(), 10)

    def test_native_and_container_adapters_report_the_same_owned_processes(self):
        tools = Path(__file__).resolve().parent
        rows = {10: (1, 100 * 1024, '/app/openwebide-bridge'),
                20: (10, 30 * 1024, '/app/openwebide-bridge'),
                21: (20, 200 * 1024, '/tools/node'),
                22: (21, 300 * 1024, '/tools/node'),
                90: (1, 999 * 1024, '/tools/node')}
        process_list = '\n'.join(
            f'{pid} {parent} {size // 1024} {program}'
            for pid, (parent, size, program) in rows.items())
        native_type = runpy.run_path(str(tools / 'measure-language-handoff-client.py'))['Residency']
        container_type = runpy.run_path(str(tools / 'language_container_support.py'))['ContainerResidency']

        class Container:
            def processes(self):
                return rows

        native = native_type(10)
        container = container_type(Container(), 10)
        with patch('subprocess.check_output', return_value=process_list):
            native_sample = native.sample()
        container_sample = container.sample()
        native_sample.pop('time')
        container_sample.pop('time')
        self.assertIsNone(container_sample.pop('container_stats'))
        self.assertEqual(native_sample, container_sample)
        self.assertEqual(native_sample['server_pids'], [21])
        self.assertEqual(native_sample['children_bytes'], 530 * 1024)

    def test_pressure_requires_actual_process_exit_after_reparenting(self):
        tools = Path(__file__).resolve().parent
        environment_type = runpy.run_path(str(tools / 'language_container_support.py'))['ContainerEnvironment']
        environment = object.__new__(environment_type)
        environment.name = 'owned-measurement-test'
        environment.metadata = {}
        environment.cgroup = lambda: {'memory.current': '123'}
        # The watcher has exited, but its server is still alive under PID 1.
        environment.processes = Mock(side_effect=[{21: (1, 200, 'node')}, {}])
        residency = SimpleNamespace(sample=Mock(return_value={'children': []}))
        events = 'max 0\noom 0\noom_kill 0'
        process = SimpleNamespace(
            stdin=Mock(), stderr=io.StringIO(''), returncode=0,
            stdout=[json.dumps({'state': state, 'memory_events': events}) + '\n'
                    for state in ['ready', 'holding', 'released']],
            poll=lambda: None, wait=Mock(return_value=0))
        with patch('subprocess.Popen', return_value=process), patch('time.sleep'):
            result = environment.evict_under_pressure({20, 21}, residency)
        self.assertEqual(environment.processes.call_count, 2)
        self.assertEqual(residency.sample.call_count, 2)
        self.assertEqual(result['evicted_container_process_pids'], [])
        self.assertTrue(result['released'])


if __name__ == '__main__':
    unittest.main()
