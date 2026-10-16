"""A rolled-back restore attempt stays rolled back when kubelet retries it late.

When a restore's prepare fails after its activation claim, sandbox-manager
rolls the attempt back: the sandbox is suspended again on its original
snapshot. Kubelet does not know that. It keeps retrying the same pod, with the
same activation attempt id, until the pod is deleted. The contract is that
those late retries are refused, so the attempt never goes live, its teardown
publishes no new snapshot, and the next connect restores the original one.

A build that accepts the late retry activates the rolled-back attempt. Deleting
its pod then suspends a sandbox the user never resumed and publishes a second
snapshot, and the next connect restores that one instead. The user's data
happens to survive in that case, so the marker file alone cannot tell the two
builds apart; the snapshot rows and the activated artifact can.

The one fault is sandbox-manager's restore-prepare test hook
(`crates/libs/sbx/mgr/server/src/service/restore_prepare_fault.rs`), armed by a
marker file and consumed when it fires; the control surface is
[`RestoreRollbackControl`].
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import TYPE_CHECKING

from space_smoke.capabilities import (
    RestoreActivation,
    RestoreRollbackControl,
    SnapshotArtifactRow,
)
from space_smoke.runtime import ResourceScope, ScenarioTimedOut, SmokeConfigurationError

if TYPE_CHECKING:
    from space_smoke.fixtures import KindEcw

# Must match `INJECTED_FAULT_MESSAGE` in `restore_prepare_fault.rs`.
INJECTED_FAULT_MESSAGE = "test hook: injected restore prepare failure"
# The fixed build's refusal of a late retry of a rolled-back attempt.
ROLLED_BACK_REFUSAL = "was rolled back"


@dataclass(frozen=True)
class FaultedAttempt:
    """The restore attempt the fault failed, as the control plane recorded it."""

    node_name: str
    generation: int
    pod_name: str
    connect_outcome: str


def _log_since_time() -> str:
    # A minute of slack for clock skew between this host and the workers.
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 60))


def judge_late_retry(
    start_failures: list[str], activations: list[RestoreActivation], generation: int
) -> str | None:
    """None when kubelet retried the faulted attempt; otherwise why not.

    The first start failure must be the injected one, or the scenario failed
    something other than what it guards. A retry shows either as a later start
    failure (the fixed build refuses it) or as an activation of the faulted
    generation (a build that accepts it).
    """

    if not start_failures:
        return "the faulted pod recorded no start failure"
    if INJECTED_FAULT_MESSAGE not in start_failures[0]:
        return f"the first start failure was not the injected fault: {start_failures[0][:300]}"
    retried = len(start_failures) >= 2 or any(
        a.execution_generation == str(generation) for a in activations
    )
    if not retried:
        return "kubelet has not retried the rolled-back activation attempt yet"
    return None


class RestoreRollbackAssertions:
    """Drive and judge one rolled-back restore and its late kubelet retry."""

    def __init__(
        self,
        fixture: KindEcw,
        control: RestoreRollbackControl,
        scope: ResourceScope,
    ) -> None:
        self._fixture = fixture
        self._control = control
        self._scope = scope
        self._since_time = _log_since_time()
        self._armed: tuple[str, ...] = ()

    @property
    def _sandbox_id(self) -> str:
        return self._fixture.sandbox.sandbox_id

    def original_snapshot(self, *, timeout: float = 180) -> SnapshotArtifactRow:
        """The one ready snapshot the suspend published; all later checks compare to it.

        A pause returns before the backup publishes, so this waits for the row
        to leave `creating`.
        """

        deadline = time.monotonic() + timeout
        while True:
            rows = self._control.snapshot_artifact_rows(self._sandbox_id)
            if (rows and rows[-1].state != "creating") or time.monotonic() > deadline:
                break
            time.sleep(2)
        self._fixture.verify(
            len(rows) == 1 and rows[0].state == "ready" and bool(rows[0].local_artifact_id),
            "the suspend did not leave exactly one ready snapshot with a local artifact",
            snapshot_rows=[r.__dict__ for r in rows],
        )
        self._fixture.record("original_snapshot", rows[0].__dict__)
        return rows[0]

    def arm_fault(self) -> None:
        """Arm one restore-prepare failure for this sandbox on every worker."""

        self._scope.defer(
            "disarm the restore-prepare fault",
            lambda: self._control.disarm_restore_prepare_fault(self._sandbox_id),
        )
        self._armed = self._control.arm_restore_prepare_fault(self._sandbox_id)
        present = self._control.armed_restore_prepare_faults(self._sandbox_id)
        self._fixture.verify(
            bool(self._armed) and present == self._armed,
            "the restore-prepare fault marker is not present on every worker it was armed on",
            armed=self._armed,
            present=present,
        )

    def connect_through_fault(self) -> FaultedAttempt:
        """Connect once with the fault armed, then disarm it and find the faulted attempt.

        The connect is expected to fail, but its outcome is recorded rather
        than asserted: a build that accepts a late retry can activate the
        attempt while the connect is still waiting, and the snapshot checks
        that follow are the verdict on that build.
        """

        try:
            self._fixture.sandbox.connect(timeout=self._fixture._sandbox_timeout)
            outcome = "connected"
        except (ScenarioTimedOut, SmokeConfigurationError):
            raise
        except Exception as error:  # noqa: BLE001 - the outcome is evidence, not the verdict
            outcome = f"{type(error).__name__}: {str(error)[:300]}"
        remaining = self._control.armed_restore_prepare_faults(self._sandbox_id)
        self._control.disarm_restore_prepare_fault(self._sandbox_id)
        fired = tuple(node for node in self._armed if node not in remaining)
        self._fixture.record("first_connect_outcome", outcome)
        self._fixture.verify(
            len(fired) == 1,
            "the restore-prepare fault did not fire exactly once, so no attempt was rolled back by it",
            armed=self._armed,
            still_armed=remaining,
        )
        executions = self._control.sandbox_executions(self._sandbox_id)
        latest = executions[-1] if executions else None
        self._fixture.verify(
            latest is not None and latest.generation >= 2 and bool(latest.pod_name),
            "no restore execution row follows the original one",
            executions=[e.__dict__ for e in executions],
        )
        assert latest is not None
        attempt = FaultedAttempt(fired[0], latest.generation, latest.pod_name, outcome)
        self._fixture.record("faulted_attempt", attempt.__dict__)
        return attempt

    def wait_for_late_retry(self, attempt: FaultedAttempt, *, timeout: float = 120) -> None:
        """Wait until kubelet has retried the faulted attempt at least once."""

        deadline = time.monotonic() + timeout
        while True:
            failures = self._control.sandbox_start_failures(attempt.pod_name)
            activations = self._control.restore_activations(
                self._sandbox_id, since_time=self._since_time
            )
            verdict = judge_late_retry(failures, activations, attempt.generation)
            if verdict is None or time.monotonic() > deadline:
                break
            time.sleep(3)
        self._fixture.record("start_failure_count", len(failures))
        self._fixture.record("start_failures", [m[-200:] for m in failures])
        self._fixture.verify(verdict is None, f"no late retry to judge: {verdict}")

    def tear_down_attempt(self, attempt: FaultedAttempt, *, timeout: float = 120) -> None:
        """Delete the faulted attempt's pod, as a late teardown would, and wait it out."""

        if any(p.pod_name == attempt.pod_name for p in self._control.sandbox_pods(self._sandbox_id)):
            self._control.delete_sandbox_pod(attempt.pod_name)
        deadline = time.monotonic() + timeout
        while any(
            p.pod_name == attempt.pod_name for p in self._control.sandbox_pods(self._sandbox_id)
        ):
            self._fixture.verify(
                time.monotonic() < deadline,
                "the faulted attempt's pod was not gone within the teardown window",
                pod_name=attempt.pod_name,
            )
            time.sleep(2)

    def expect_original_snapshot_restored(
        self, original: SnapshotArtifactRow, attempt: FaultedAttempt
    ) -> None:
        """The verdict: one snapshot row, restored from its own artifact, by a later attempt."""

        rows = self._control.snapshot_artifact_rows(self._sandbox_id)
        self._fixture.verify(
            [r.snapshot_id for r in rows] == [original.snapshot_id],
            "a new snapshot was published after the rolled-back restore; its late retry "
            "went live and its teardown suspended it again",
            original_snapshot_id=original.snapshot_id,
            snapshot_rows=[r.__dict__ for r in rows],
        )
        activations = self._control.restore_activations(
            self._sandbox_id, since_time=self._since_time
        )
        self._fixture.record("activations", [a.__dict__ for a in activations])
        faulted = [a for a in activations if a.execution_generation == str(attempt.generation)]
        self._fixture.verify(
            not faulted,
            "the rolled-back activation attempt went live on a late kubelet retry",
            faulted_generation=attempt.generation,
        )
        successors = [
            a
            for a in activations
            if a.execution_generation.isdigit() and int(a.execution_generation) > attempt.generation
        ]
        self._fixture.verify(
            bool(successors),
            "no restore after the rolled-back attempt reported an activation",
            faulted_generation=attempt.generation,
        )
        successor = max(successors, key=lambda a: int(a.execution_generation))
        self._fixture.verify(
            successor.boot_type == "snapshot"
            and successor.local_artifact_id == original.local_artifact_id,
            "the successor restore did not activate the original snapshot's artifact",
            original_local_artifact_id=original.local_artifact_id,
            successor=successor.__dict__,
        )
        pods = [
            p
            for p in self._control.sandbox_pods(self._sandbox_id)
            if p.execution_generation == successor.execution_generation
        ]
        for pod in pods:
            self._fixture.verify(
                not pod.restore_storage_uri or original.snapshot_id in pod.restore_storage_uri,
                "the successor pod names a snapshot other than the original",
                original_snapshot_id=original.snapshot_id,
                restore_storage_uri=pod.restore_storage_uri,
            )
