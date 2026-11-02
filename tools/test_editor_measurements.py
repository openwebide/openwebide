"""Memory snapshots retain complete process coverage across Chrome churn."""
import importlib.util
import json
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "editor_measurements", Path(__file__).with_name("measure-editor-view.py")
)
measurements = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measurements)
memory_spec = importlib.util.spec_from_file_location(
    "editor_memory", Path(__file__).with_name("editor_memory.py")
)
editor_memory = importlib.util.module_from_spec(memory_spec)
memory_spec.loader.exec_module(editor_memory)


class ScriptResultTests(unittest.TestCase):
    def test_scoped_scripts_preserve_arguments_and_leave_caller_payload_unchanged(self):
        anchor = {"element-6066-11e4-a52e-4f735466cecf": "owned-root"}
        browser = SimpleNamespace(call=Mock(return_value=anchor))
        scoped = editor_memory.ScopedScriptResults(browser)
        original = {"script": "return [arguments.length, ...arguments];", "args": ["文😀", None, {"value": 3}]}
        scoped.call("POST", "/execute/sync", original, timeout=7)
        args, kwargs = browser.call.call_args
        self.assertEqual(kwargs, {"timeout": 7})
        self.assertEqual(args[2]["args"][-1], anchor)
        script = "const body=" + json.dumps(args[2]) + "; process.stdout.write(JSON.stringify(new Function(body.script).apply(null, body.args)));"
        result = subprocess.check_output(["node", "-e", script], text=True)
        self.assertEqual(json.loads(result), [3, "文😀", None, {"value": 3}])
        self.assertEqual(original["args"], ["文😀", None, {"value": 3}])

    def test_other_commands_and_async_callback_arguments_are_untouched(self):
        browser = SimpleNamespace(call=Mock(return_value={"element-6066-11e4-a52e-4f735466cecf": "owned-root"}), log=object())
        scoped = editor_memory.ScopedScriptResults(browser)
        body = {"script": "arguments[0](true);", "args": []}
        scoped.call("POST", "/execute/async", body)
        self.assertIs(browser.call.call_args.args[2], body)
        self.assertIs(scoped.log, browser.log)
        scoped.call("DELETE", "/session", session=False)
        browser.call.assert_called_with("DELETE", "/session", None, session=False)


class RetainedMemoryTests(unittest.TestCase):
    def test_collects_every_owned_heap_and_reports_missing_workers_without_inventing_them(self):
        for kinds in [["page"], ["page", "worker", "service_worker", "shared_worker", "iframe"]]:
            client = Mock()
            def call(method, params=None, session_id=None):
                if method == "Target.getTargets":
                    return {"targetInfos": [{"type": kind, "targetId": kind} for kind in kinds] + [{"type": "browser", "targetId": "root"}]}
                if method == "Target.attachToTarget":
                    return {"sessionId": params["targetId"]}
                if method == "Runtime.getHeapUsage":
                    return {"usedSize": 123, "backingStorageSize": 456}
                return {}
            client.call.side_effect = call
            browser = SimpleNamespace(log=SimpleNamespace(name="/tmp/owned-browser/chromedriver.log"))
            with self.subTest(kinds=kinds), patch.object(editor_memory, "Cdp", return_value=client):
                result = editor_memory.collect_garbage(browser)
            self.assertEqual([item["targetType"] for item in result], kinds)
            self.assertTrue(all(item["heap"]["usedSize"] == 123 for item in result))
            for kind in kinds:
                client.call.assert_any_call("HeapProfiler.collectGarbage", session_id=kind)
                client.call.assert_any_call("Target.detachFromTarget", {"sessionId": kind})
            self.assertEqual(sum(call.args[0] == "HeapProfiler.collectGarbage" for call in client.call.call_args_list), len(kinds))
            client.socket.close.assert_called_once_with()

    def test_failed_collection_detaches_and_does_not_report_partial_success(self):
        client = Mock()
        def call(method, params=None, session_id=None):
            if method == "Target.getTargets":
                return {"targetInfos": [{"type": "page", "targetId": "one"}]}
            if method == "Target.attachToTarget":
                return {"sessionId": "one"}
            if method == "HeapProfiler.collectGarbage":
                raise RuntimeError("collection failed")
            return {}
        client.call.side_effect = call
        browser = SimpleNamespace(log=SimpleNamespace(name="/tmp/owned-browser/chromedriver.log"))
        with patch.object(editor_memory, "Cdp", return_value=client), self.assertRaisesRegex(RuntimeError, "collection failed"):
            editor_memory.collect_garbage(browser)
        client.call.assert_any_call("Target.detachFromTarget", {"sessionId": "one"})
        client.socket.close.assert_called_once_with()


class DebuggerTransportTests(unittest.TestCase):
    def client(self, data=b""):
        client = object.__new__(editor_memory.Cdp)
        client.socket = Mock()
        client.socket.recv.return_value = b""
        client.buffer = data
        client.sequence = 0
        return client

    def test_client_masks_short_and_extended_messages(self):
        for length in [20, 200, 70000]:
            payload = b"a" * length
            client = self.client()
            with self.subTest(length=length), patch.object(editor_memory.os, "urandom", return_value=b"mask"):
                client.send(payload)
            frame = client.socket.sendall.call_args.args[0]
            self.assertEqual(frame[0], 0x81)
            self.assertTrue(frame[1] & 0x80)
            marker = frame[1] & 127
            if marker < 126:
                size, at = marker, 2
            elif marker == 126:
                size, at = int.from_bytes(frame[2:4], "big"), 4
            else:
                size, at = int.from_bytes(frame[2:10], "big"), 10
            self.assertEqual(size, length)
            mask = frame[at:at + 4]
            decoded = bytes(value ^ mask[index % 4] for index, value in enumerate(frame[at + 4:]))
            self.assertEqual(decoded, payload)

    def test_server_fragmentation_unicode_and_ping(self):
        first = b'{"result":"'
        last = '文😀"}'.encode()
        client = self.client(b"\x89\x02ok" + bytes([0x01, len(first)]) + first + bytes([0x80, len(last)]) + last)
        self.assertEqual(client.receive(), {"result": "文😀"})
        self.assertEqual(client.socket.sendall.call_args.args[0][0], 0x8a)

    def test_extended_server_messages(self):
        for length in [200, 70000]:
            value = {"value": "a" * length}
            payload = json.dumps(value).encode()
            size = bytes([126]) + len(payload).to_bytes(2, "big") if len(payload) < 65536 else bytes([127]) + len(payload).to_bytes(8, "big")
            self.assertEqual(self.client(b"\x81" + size + payload).receive(), value)

    def test_closed_connections_and_oversized_frames_fail(self):
        with self.assertRaisesRegex(AssertionError, "closed"):
            self.client().receive()
        with self.assertRaises(AssertionError):
            self.client(b"\x81\x7f" + (64 * 1024 * 1024 + 1).to_bytes(8, "big")).receive()

    def test_handshake_eof_fails_instead_of_waiting_forever(self):
        connection = Mock()
        connection.recv.return_value = b""
        with patch.object(Path, "read_text", return_value="1234\n/devtools/browser/owned\n"), \
                patch.object(editor_memory.socket, "create_connection", return_value=connection), \
                self.assertRaisesRegex(AssertionError, "handshake"):
            editor_memory.Cdp(Path("/tmp/owned-browser"))
        connection.close.assert_called_once_with()

    def test_handshake_checks_the_server_nonce(self):
        for accept, valid in [(b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", True), (b"S3pPLMBiTxaQ9kYGzzhZRbK+xOo=", False)]:
            connection = Mock()
            connection.recv.return_value = b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n"
            with self.subTest(valid=valid), patch.object(Path, "read_text", return_value="1234\n/devtools/browser/owned\n"), \
                    patch.object(editor_memory.socket, "create_connection", return_value=connection), \
                    patch.object(editor_memory.os, "urandom", return_value=b"the sample nonce"):
                if valid:
                    client = editor_memory.Cdp(Path("/tmp/owned-browser"))
                    self.assertIs(client.socket, connection)
                    client.socket.close()
                else:
                    with self.assertRaises(AssertionError):
                        editor_memory.Cdp(Path("/tmp/owned-browser"))
            connection.close.assert_called_once_with()


class MemorySnapshotTests(unittest.TestCase):
    def setUp(self):
        roles = patch.object(measurements, "chrome_process_role", return_value="unknown")
        roles.start()
        self.addCleanup(roles.stop)

    def test_complete_snapshot_includes_descendants_and_excludes_driver_and_neighbors(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n3 2 20\n4 0 40\n"), \
                patch.object(Path, "read_text", autospec=True, side_effect=lambda path: f"Pss: {int(path.parts[2]) * 10} kB\n"):
            result = measurements.process_memory(1)
        self.assertEqual(result, {"chrome_processes": 2, "chrome_summed_rss_kib": 30,
                                  "chrome_pss_kib": 50, "memorySnapshotAttempts": 1,
                                  "chrome_memory_by_role": {"unknown": {"processes": 2, "rssKiB": 30, "pssKiB": 50}}})

    def test_retired_process_requires_a_fresh_complete_snapshot(self):
        with patch.object(measurements.subprocess, "check_output", side_effect=["1 0 5\n2 1 10\n", "1 0 5\n3 1 20\n"]), \
                patch.object(Path, "read_text", side_effect=[FileNotFoundError(), "Pss: 30 kB\n"]), \
                patch.object(Path, "is_dir", return_value=True), \
                patch.object(measurements.time, "sleep") as sleep:
            result = measurements.process_memory(1)
        self.assertEqual(result, {"chrome_processes": 1, "chrome_summed_rss_kib": 20,
                                  "chrome_pss_kib": 30, "memorySnapshotAttempts": 2,
                                  "chrome_memory_by_role": {"unknown": {"processes": 1, "rssKiB": 20, "pssKiB": 30}}})
        sleep.assert_called_once_with(.01)

    def test_role_totals_cover_owned_processes_and_keep_partial_groups_unavailable(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n3 2 20\n4 2 30\n5 0 90\n"), \
                patch.object(measurements, "chrome_process_role", side_effect=["browser", "renderer", "renderer"]), \
                patch.object(Path, "read_text", side_effect=["Pss: 5 kB\n", "Pss: 15 kB\n", PermissionError()]):
            result = measurements.process_memory_snapshot(1)
        self.assertIsNone(result["chrome_pss_kib"])
        self.assertEqual(result["chrome_memory_by_role"], {
            "browser": {"processes": 1, "rssKiB": 10, "pssKiB": 5},
            "renderer": {"processes": 2, "rssKiB": 50, "pssKiB": None},
        })

    def test_persistent_unreadable_process_never_reports_partial_pss_as_complete(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n3 1 20\n"), \
                patch.object(Path, "read_text", side_effect=["Pss: 10 kB\n", PermissionError()] * 3), \
                patch.object(Path, "is_dir", return_value=True), \
                patch.object(measurements.time, "sleep"):
            result = measurements.process_memory(1)
        self.assertIsNone(result["chrome_pss_kib"])
        self.assertIsNone(result["chrome_memory_by_role"]["unknown"]["pssKiB"])
        self.assertEqual(result["chrome_processes"], 2)
        self.assertEqual(result["memorySnapshotAttempts"], 3)

    def test_hosts_without_proc_keep_pss_unavailable_without_retrying(self):
        with patch.object(measurements.subprocess, "check_output", return_value="1 0 5\n2 1 10\n"), \
                patch.object(Path, "read_text", side_effect=FileNotFoundError()), \
                patch.object(Path, "is_dir", return_value=False), \
                patch.object(measurements.time, "sleep") as sleep:
            result = measurements.process_memory(1)
        self.assertIsNone(result["chrome_pss_kib"])
        self.assertIsNone(result["chrome_memory_by_role"]["unknown"]["pssKiB"])
        self.assertEqual(result["memorySnapshotAttempts"], 1)
        sleep.assert_not_called()


class ProcessRoleTests(unittest.TestCase):
    def test_reports_only_known_roles_without_arguments(self):
        for args, role in [(b"chrome\0--type=renderer\0--token=private", "renderer"),
                           (b"chrome --type=renderer --token=private\0", "renderer"),
                           (b"chrome\0--type=gpu-process", "gpu"),
                           (b"chrome\0--type=utility", "utility"),
                           (b"chrome\0--type=new-kind", "other"),
                           (b"/opt/chrome\0--private=value", "browser"),
                           (b"/usr/lib/chromium/chromium\0--private=value", "browser"),
                           (b"crashpad_handler\0--private=value", "other")]:
            with self.subTest(role=role), patch.object(Path, "read_bytes", return_value=args):
                self.assertEqual(measurements.chrome_process_role(2), role)

    def test_retired_process_has_unknown_role(self):
        with patch.object(Path, "read_bytes", side_effect=FileNotFoundError()):
            self.assertEqual(measurements.chrome_process_role(2), "unknown")
