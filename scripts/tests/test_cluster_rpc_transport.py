from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


LIBRARY = Path(__file__).resolve().parents[1] / "lib/space-cluster-rpc.sh"


class ClusterRpcTransportTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.capture = self.root / "calls.jsonl"
        self.ca = self.root / "ca.pem"
        self.ca.touch()
        grpcurl = self.root / "grpcurl"
        grpcurl.write_text(
            '#!/usr/bin/env python3\n'
            'import json, os, sys\n'
            'with open(os.environ["RPC_CAPTURE"], "a") as output:\n'
            '    output.write(json.dumps(sys.argv[1:]) + "\\n")\n'
            'sys.exit(int(os.environ.get("RPC_EXIT", "0")))\n'
        )
        grpcurl.chmod(0o755)
        self.env = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("SPACE_INTERNAL_RPC_", "SPACE_CLUSTER_API_", "NODE_API_ENDPOINT"))
        }
        self.env.update({
            "PATH": f"{self.root}:{self.env['PATH']}",
            "RPC_CAPTURE": str(self.capture),
            "NODE_API_ENDPOINT": "rpc.example.test:9800",
            "SPACE_INTERNAL_RPC_TLS_CA": str(self.ca),
            "SPACE_CLUSTER_API_TLS_SERVER_NAME": "cluster.example.test",
        })

    def invoke(self, **overrides):
        return subprocess.run([
            "bash", "-euc",
            'source "$1"; space_cluster_rpc_configure; '
            'space_cluster_rpc example.Cluster/Check -H "authorization: Bearer synthetic" -d "{}"',
            "rpc-test", str(LIBRARY),
        ], env={**self.env, **overrides}, text=True, capture_output=True)

    def calls(self):
        return [json.loads(line) for line in self.capture.read_text().splitlines()] if self.capture.exists() else []

    def test_tls_verifies_ca_and_service_identity_for_bare_and_https_addresses(self):
        for endpoint in ("rpc.example.test:9800", "https://rpc.example.test:9800"):
            with self.subTest(endpoint=endpoint):
                result = self.invoke(NODE_API_ENDPOINT=endpoint)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.calls()[-1], [
                    "-cacert", str(self.ca), "-authority", "cluster.example.test",
                    "-H", "authorization: Bearer synthetic", "-d", "{}",
                    "rpc.example.test:9800", "example.Cluster/Check",
                ])

    def test_explicit_https_still_requires_tls_with_insecure_development_exception(self):
        result = self.invoke(NODE_API_ENDPOINT="https://rpc.example.test:9800", SPACE_INTERNAL_RPC_INSECURE_DEV="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-cacert", self.calls()[0])
        self.assertNotIn("-plaintext", self.calls()[0])

    def test_plaintext_requires_explicit_development_exception(self):
        for endpoint in ("rpc.example.test:9800", "http://rpc.example.test:9800"):
            with self.subTest(endpoint=endpoint):
                result = self.invoke(NODE_API_ENDPOINT=endpoint, SPACE_INTERNAL_RPC_INSECURE_DEV="true", SPACE_INTERNAL_RPC_TLS_CA="")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("-plaintext", self.calls()[-1])
                self.assertEqual(self.calls()[-1][-2], "rpc.example.test:9800")

    def test_invalid_configuration_never_sends_a_bearer(self):
        for overrides in (
            {"NODE_API_ENDPOINT": "http://rpc.example.test:9800"},
            {"NODE_API_ENDPOINT": "ftp://rpc.example.test:9800"},
            {"SPACE_INTERNAL_RPC_TLS_CA": ""},
            {"SPACE_INTERNAL_RPC_TLS_CA": str(self.root / "missing.pem")},
            {"SPACE_INTERNAL_RPC_INSECURE_DEV": "yes"},
        ):
            with self.subTest(overrides=overrides):
                self.assertNotEqual(self.invoke(**overrides).returncode, 0)
                self.assertEqual(self.calls(), [])

    def test_failed_tls_call_does_not_retry_as_plaintext(self):
        result = self.invoke(RPC_EXIT="42")
        self.assertEqual(result.returncode, 42)
        self.assertEqual(len(self.calls()), 1)
        self.assertIn("-cacert", self.calls()[0])
        self.assertNotIn("-plaintext", self.calls()[0])


if __name__ == "__main__":
    unittest.main()
