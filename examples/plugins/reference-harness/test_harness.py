#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys
import unittest


ROOT = pathlib.Path(__file__).parent
SCRIPT = ROOT / "reference_harness.py"


class ReferenceHarnessTest(unittest.TestCase):
    def setUp(self):
        self.process = subprocess.Popen(
            [sys.executable, str(SCRIPT)],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            text=True,
        )

    def tearDown(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.process.stdin.close()
        self.process.stdout.close()

    def request(self, request_id, method, params=None):
        self.process.stdin.write(json.dumps({
            "jsonrpc": "2.0", "id": request_id, "method": method,
            "params": params or {},
        }) + "\n")
        self.process.stdin.flush()
        return json.loads(self.process.stdout.readline())

    def test_reference_echoes_scope_and_requires_matching_release(self):
        for index, ownership in enumerate(("existing", "managed"), start=1):
            key = {"allocationId": f"a{index}", "dispatchId": f"d{index}"}
            prepared = self.request(
                f"prepare-{index}",
                "agent/prepare",
                {**key, "agentName": "agent", "workspace": "/tmp", "template": {
                    "values": {
                        "reference_target": f"target-{index}",
                        "reference_ownership": ownership,
                    },
                }},
            )
            self.assertEqual(prepared["result"], {"target": f"target-{index}", "ownership": ownership})
            wrong_release = self.request(
                f"wrong-release-{index}", "agent/release",
                {**key, "target": "another-target", "outcome": "completed"},
            )
            self.assertIn("error", wrong_release)
            released = self.request(
                f"release-{index}", "agent/release",
                {**key, "target": f"target-{index}", "outcome": "completed"},
            )
            self.assertEqual(released["result"], {"released": True})

        missing = self.request(
            "prepare-missing", "agent/prepare",
            {"allocationId": "a3", "dispatchId": "d3", "template": {"values": {"reference_ownership": "managed"}}},
        )
        self.assertIn("error", missing)

        shutdown = self.request("shutdown", "shutdown")
        self.assertEqual(shutdown["result"], {"accepted": True})
        self.process.wait(timeout=2)


if __name__ == "__main__":
    unittest.main()
