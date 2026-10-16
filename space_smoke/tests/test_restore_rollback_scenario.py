"""Contracts of the restore-rollback late-retry scenario the bench cannot exercise.

A passing bench run shows the fixed build's evidence passes. These pin the
rest off-bench: the evidence a build that accepts the late retry leaves fails
with the named reason, the scenario refuses to judge when the fault did not
cause the failure, the adapter expands aggregated kube events, and the Python
side names the same hook variable and message as the Rust hook.
"""

from __future__ import annotations

import json
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

from space_smoke import adapters
from space_smoke.adapters import KindConfig, KindHarness
from space_smoke.capabilities import (
    RestoreActivation,
    SandboxPodView,
    SnapshotArtifactRow,
)
from space_smoke.restore_rollback import (
    INJECTED_FAULT_MESSAGE,
    FaultedAttempt,
    RestoreRollbackAssertions,
    judge_late_retry,
)
from space_smoke.runtime import CommandResult, InvariantViolation, ResourceScope

SANDBOX_ID = "01a14600-0000-7000-8000-000000000001"
ORIGINAL = SnapshotArtifactRow("snap-original", "full", "ready", "artifact-original")
ATTEMPT = FaultedAttempt("worker-a", 2, "space-sbx-x-g2", "ConnectionError: 502")
HOOK_SOURCE = (
    Path(__file__).resolve().parents[2]
    / "crates/libs/sbx/mgr/server/src/service/restore_prepare_fault.rs"
)


def activation(generation: int, artifact: str = "", boot: str = "snapshot") -> RestoreActivation:
    return RestoreActivation(
        SANDBOX_ID, "worker-a", boot, "local_only", False, str(generation), artifact
    )


class _Fixture:
    def __init__(self) -> None:
        self.sandbox = SimpleNamespace(sandbox_id=SANDBOX_ID)
        self.evidence: dict[str, object] = {}

    def record(self, name: str, value: object) -> None:
        self.evidence[name] = value

    def verify(self, condition: bool, message: str, **evidence: object) -> None:
        self.evidence.update(evidence)
        if not condition:
            raise InvariantViolation(message)


class _Control:
    def __init__(self, rows, activations, pods=()) -> None:
        self.rows = list(rows)
        self.activations = list(activations)
        self.pods = list(pods)

    def snapshot_artifact_rows(self, _sandbox_id):
        return self.rows

    def restore_activations(self, _sandbox_id, **_kw):
        return self.activations

    def sandbox_pods(self, _sandbox_id):
        return self.pods


def verdict(control: _Control) -> None:
    assertions = RestoreRollbackAssertions(_Fixture(), control, ResourceScope())  # type: ignore[arg-type]
    assertions.expect_original_snapshot_restored(ORIGINAL, ATTEMPT)


class Verdict(unittest.TestCase):
    def test_the_fixed_build_evidence_passes(self) -> None:
        verdict(_Control([ORIGINAL], [activation(1, boot="fresh"), activation(3, "artifact-original")]))

    def test_a_build_that_accepts_the_late_retry_fails_on_the_new_snapshot(self) -> None:
        second = SnapshotArtifactRow("snap-second", "full", "ready", "artifact-second")
        with self.assertRaisesRegex(InvariantViolation, "new snapshot was published"):
            verdict(
                _Control(
                    [ORIGINAL, second],
                    [activation(2, "artifact-original"), activation(3, "artifact-second")],
                )
            )

    def test_a_pending_new_snapshot_row_also_fails(self) -> None:
        pending = SnapshotArtifactRow("snap-second", "full", "creating", "")
        with self.assertRaisesRegex(InvariantViolation, "new snapshot was published"):
            verdict(_Control([ORIGINAL, pending], [activation(3, "artifact-original")]))

    def test_a_live_rolled_back_attempt_fails_even_without_a_new_row(self) -> None:
        with self.assertRaisesRegex(InvariantViolation, "went live on a late kubelet retry"):
            verdict(_Control([ORIGINAL], [activation(2, "artifact-original"), activation(3, "artifact-original")]))

    def test_a_successor_on_another_artifact_fails(self) -> None:
        with self.assertRaisesRegex(InvariantViolation, "original snapshot's artifact"):
            verdict(_Control([ORIGINAL], [activation(3, "artifact-other")]))

    def test_no_successor_activation_fails(self) -> None:
        with self.assertRaisesRegex(InvariantViolation, "no restore after the rolled-back attempt"):
            verdict(_Control([ORIGINAL], [activation(1, boot="fresh")]))

    def test_a_successor_pod_naming_another_snapshot_fails(self) -> None:
        pod = SandboxPodView(
            "space-sbx-x-g3", "Running", "worker-a", "3", "suspend", "worker-a",
            "s3://bucket/suspend-snapshots/snap-other", False,
        )
        with self.assertRaisesRegex(InvariantViolation, "names a snapshot other than the original"):
            verdict(_Control([ORIGINAL], [activation(3, "artifact-original")], [pod]))


class LateRetryJudgement(unittest.TestCase):
    injected = f"sbx-manager Start [space-err=Internal]: prepare failed: message: \"{INJECTED_FAULT_MESSAGE}\""
    refused = "sbx-manager Start [space-err=FailedPrecondition]: activation attempt a for execution generation 2 was rolled back"

    def test_a_refused_retry_counts(self) -> None:
        self.assertIsNone(judge_late_retry([self.injected, self.refused], [], 2))

    def test_an_activated_retry_counts(self) -> None:
        self.assertIsNone(judge_late_retry([self.injected], [activation(2)], 2))

    def test_no_retry_yet_is_named(self) -> None:
        self.assertIn("not retried", judge_late_retry([self.injected], [activation(1)], 2) or "")

    def test_a_failure_the_fault_did_not_cause_refuses_to_judge(self) -> None:
        other = "sbx-manager Start [space-err=Internal]: prepare failed: disk full"
        self.assertIn("not the injected fault", judge_late_retry([other, self.refused], [], 2) or "")

    def test_no_failure_at_all_refuses_to_judge(self) -> None:
        self.assertIn("no start failure", judge_late_retry([], [], 2) or "")


class _ScriptedKubectl:
    def __init__(self, results) -> None:
        self.results = list(results)
        self.calls: list[tuple[str, ...]] = []

    def run(self, command, **_options) -> CommandResult:
        self.calls.append(tuple(command))
        return self.results.pop(0)


class Adapter(unittest.TestCase):
    def test_start_failures_expand_aggregated_events_in_time_order(self) -> None:
        events = {
            "items": [
                {"message": "rolled back", "count": 3, "firstTimestamp": "2026-01-01T00:00:20Z"},
                {"message": "injected", "count": 1, "firstTimestamp": "2026-01-01T00:00:05Z"},
            ]
        }
        kubectl = _ScriptedKubectl([CommandResult(("kubectl",), 0, json.dumps(events), "")])
        harness = KindHarness(kubectl, KindConfig())
        self.assertEqual(
            harness.sandbox_start_failures("space-sbx-x-g2"),
            ["injected", "rolled back", "rolled back", "rolled back"],
        )
        self.assertTrue(any("reason=FailedCreatePodSandBox" in arg for arg in kubectl.calls[0]))

    def test_arming_refuses_a_worker_without_the_hook(self) -> None:
        harness = KindHarness(_ScriptedKubectl([]), KindConfig())
        with (
            mock.patch.object(KindHarness, "_require_isolated_kind"),
            mock.patch.object(KindHarness, "sandbox_pool_nodes", return_value=["worker-a"]),
            mock.patch.object(KindHarness, "_node_service_env", return_value={}),
        ):
            with self.assertRaisesRegex(Exception, "predates the restore-prepare fault hook"):
                harness.arm_restore_prepare_fault(SANDBOX_ID)


class HookNamesMatchTheRustSide(unittest.TestCase):
    def test_the_variable_and_message_are_the_rust_hook_constants(self) -> None:
        source = HOOK_SOURCE.read_text()
        self.assertIn(f'"{adapters.RESTORE_PREPARE_FAULT_DIR_ENV}"', source)
        self.assertIn(f'"{INJECTED_FAULT_MESSAGE}"', source)


if __name__ == "__main__":
    unittest.main()
