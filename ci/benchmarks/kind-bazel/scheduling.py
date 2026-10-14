#!/usr/bin/env python3
"""Compare fixed duration-balanced shards with Bazel scheduling on equal workers."""

import argparse
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))


def label(surface, scenario_id):
    return (
        "//ci/benchmarks/kind-bazel:" + surface + "__" + scenario_id.replace(".", "__")
    )


def plan(args):
    from space_smoke.scenarios import REGISTRY
    from space_smoke.sharding import select_shard

    eligible = tuple(
        scenario
        for scenario in REGISTRY.select(backend="kind")
        if scenario.telemetry_mode in (None, "otel")
    )
    unknown = set(args.exclude) - {scenario.id for scenario in eligible}
    if unknown or (args.exclude and not args.exclusion_reason):
        raise SystemExit(
            "Every exclusion needs a known scenario ID and an explicit reason"
        )
    partitions = [[] for _ in range(args.workers)]
    for surface in ("ecw", "native"):
        selected = tuple(
            scenario for scenario in eligible if surface in scenario.api_surfaces
        )
        for slot in range(args.workers):
            # Preserve the existing shard assignments, then exclude unsupported
            # cases identically in both arms. Never silently report them as green.
            for scenario in select_shard(
                selected, index=slot + 1, count=args.workers, api_surface=surface
            ):
                if scenario.id not in args.exclude:
                    partitions[slot].append(label(surface, scenario.id))
    targets = [target for partition in partitions for target in partition]
    if not targets or len(targets) != len(set(targets)):
        raise SystemExit(
            "Comparison inventory must be nonempty with no duplicate targets"
        )
    return {
        "workers": args.workers,
        "eligible_pairs": sum(len(scenario.api_surfaces) for scenario in eligible),
        "selected_pairs": len(targets),
        "exclusions": {name: args.exclusion_reason for name in args.exclude},
        "partitions": partitions,
        "targets": sorted(targets),
        "baseline": "existing duration-balanced assignments; same per-scenario process/fixture adapter as candidate",
        "boundary": "fresh isolated stacks for each arm; serial arms on one host; same source and artifact image",
    }


def records(directory, targets, mode):
    found = {}
    for target in targets:
        name = target.split(":")[-1]
        path = (
            directory / "targets" / (name + ".log")
            if mode == "fixed"
            else ROOT / "bazel-testlogs/ci/benchmarks/kind-bazel" / name / "test.log"
        )
        if not path.exists():
            continue
        for line in path.read_text().splitlines():
            if not line.startswith('{"surface":'):
                continue
            value = json.loads(line)
            if label(value["surface"], value["scenario"]) != target or target in found:
                raise ValueError("Duplicate or mismatched scenario timing record")
            found[target] = value
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bazel", default="bazel")
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--source-archive", type=Path)
    parser.add_argument("--artifact-archive", type=Path)
    parser.add_argument("--cgroup-parent")
    parser.add_argument("--runner-image", default="space-bazel-kind-runner:local")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--exclude", action="append", default=[])
    parser.add_argument("--exclusion-reason")
    parser.add_argument("--plan-only", action="store_true")
    parser.add_argument(
        "--dynamic-first",
        action="store_true",
        help="Reverse arm order for a repeat trial",
    )
    args = parser.parse_args()
    if args.workers < 1:
        parser.error("workers must be positive")
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    inventory = plan(args)
    (args.output / "inventory.json").write_text(json.dumps(inventory, indent=2) + "\n")
    fixed_plan = args.output / "fixed-plan.json"
    fixed_plan.write_text(json.dumps(inventory["partitions"], indent=2) + "\n")
    print(
        json.dumps(
            {
                key: inventory[key]
                for key in ("workers", "eligible_pairs", "selected_pairs", "exclusions")
            }
        ),
        flush=True,
    )
    if args.plan_only:
        return 0
    if args.source_archive is None or args.artifact_archive is None:
        parser.error("source-archive and artifact-archive are required for execution")
    results = {"inventory": inventory, "valid": False, "measurements": {}}
    summary = args.output / "comparison.json"
    for mode in ("dynamic", "fixed") if args.dynamic_first else ("fixed", "dynamic"):
        directory = args.output / mode
        if directory.exists():
            parser.error("Use a new output directory for every paired trial")
        command = [
            sys.executable,
            str(ROOT / "ci/benchmarks/kind-bazel/pool.py"),
            "run",
            "--bazel",
            args.bazel,
            "--workers",
            str(args.workers),
            "--prewarm",
            "--stop-on-failure",
            "--source-archive",
            str(args.source_archive.resolve()),
            "--artifact-archive",
            str(args.artifact_archive.resolve()),
            "--runner-image",
            args.runner_image,
            "--output",
            str(directory),
        ]
        if args.cgroup_parent:
            command.extend(["--cgroup-parent", args.cgroup_parent])
        if mode == "fixed":
            command.extend(["--fixed-plan", str(fixed_plan)])
        command.extend(inventory["targets"])
        print("Starting " + mode, flush=True)
        with (args.output / (mode + ".log")).open("w") as stream:
            run = subprocess.run(
                command, cwd=ROOT, stdout=stream, stderr=subprocess.STDOUT, check=False
            )
        timings = records(directory, inventory["targets"], mode)
        metrics_path = directory / "run.json"
        metrics = json.loads(metrics_path.read_text()) if metrics_path.exists() else {}
        complete = (
            run.returncode == 0
            and set(timings) == set(inventory["targets"])
            and all(value["returncode"] == 0 for value in timings.values())
        )
        results["measurements"][mode] = {
            "complete": complete,
            "metrics": metrics,
            "scenarios": timings,
        }
        summary.write_text(json.dumps(results, indent=2) + "\n")
        if not complete:
            print(
                "Incomplete " + mode + "; no scheduling speedup can be claimed",
                flush=True,
            )
            return 1
    fixed = results["measurements"]["fixed"]["metrics"]
    dynamic = results["measurements"]["dynamic"]["metrics"]
    if any(
        fixed[key] != dynamic[key]
        for key in (
            "source_sha256",
            "artifact_sha256",
            "workers",
            "cgroup_parent",
        )
    ):
        raise SystemExit("Paired fixtures do not match")
    results.update(
        valid=True,
        execution_reduction_percent=100
        * (1 - dynamic["execution_seconds"] / fixed["execution_seconds"]),
        total_reduction_percent=100 * (1 - dynamic["seconds"] / fixed["seconds"]),
    )
    summary.write_text(json.dumps(results, indent=2) + "\n")
    print(
        json.dumps(
            {
                key: value
                for key, value in results.items()
                if key not in ("inventory", "measurements")
            }
        ),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
