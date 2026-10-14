#!/usr/bin/env python3
"""Measure the same retention edit in prepared before/after source trees."""

import argparse
import json
import time
from pathlib import Path

import benchmark

OLD_PATH = Path("crates/libs/cluster/controller/server/src/tombstone_retention.rs")
NEW_PATH = Path("crates/libs/cluster/controller/tombstone-retention/lib.rs")
ORIGINAL = "const DEFAULT_MAX_RECORDS_PER_TICK: u32 = 1_000;"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--backend", choices=["bazel", "cargo"], required=True)
    parser.add_argument("--bazel", default="bazel")
    parser.add_argument("--builder", default="space-controller-benchmark")
    parser.add_argument("--disk-cache", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--batch-limit", type=int, default=1001)
    parser.add_argument(
        "--edit-kind", choices=["production", "tests"], default="production"
    )
    args = parser.parse_args()
    if args.backend == "bazel" and args.disk_cache is None:
        parser.error("--disk-cache is required for Bazel")
    if not 0 < args.batch_limit <= 0xFFFF_FFFF or args.batch_limit == 1000:
        parser.error("batch-limit must fit u32 and differ from the original 1000")
    edit = f"const DEFAULT_MAX_RECORDS_PER_TICK: u32 = {args.batch_limit:_};"
    args.output.mkdir(parents=True, exist_ok=True)
    originals = {}
    records = []
    try:
        for layout, workspace, relative in [
            ("before", args.before.resolve(), OLD_PATH),
            ("after", args.after.resolve(), NEW_PATH),
        ]:
            if args.edit_kind == "tests":
                relative = (
                    OLD_PATH.parent / "tombstone_retention/tests.rs"
                    if layout == "before"
                    else NEW_PATH.with_name("retention.test.rs")
                )
            target = workspace / relative
            original = target.read_text()
            if args.edit_kind == "production" and original.count(ORIGINAL) != 1:
                raise SystemExit(
                    f"expected one unchanged retention batch limit in {target}"
                )
            originals[target] = original
            cache = workspace / ".cache/controller-benchmark"
            cache.mkdir(parents=True, exist_ok=True)
            seed = args.output.resolve() / "empty-compiler-cache"
            seed.mkdir(exist_ok=True)
            for phase in ["warmup", "edit"]:
                if phase == "edit":
                    target.write_text(
                        original.replace(ORIGINAL, edit)
                        if args.edit_kind == "production"
                        else original
                        + f"\n// Benchmark test-source invalidation {args.batch_limit}.\n"
                    )
                started = time.perf_counter()
                if args.backend == "bazel":
                    result = benchmark.bazel(
                        workspace, args.bazel, args.disk_cache.resolve()
                    )
                    filenames = [
                        "bazel-build.log",
                        "bazel-actions.json",
                        "bazel-profile.json",
                    ]
                else:
                    result = benchmark.cargo(workspace, args.builder, seed)
                    filenames = ["cargo-build.log", "cargo-image.log"]
                elapsed = time.perf_counter() - started
                record = {
                    "backend": args.backend,
                    "layout": layout,
                    "phase": phase,
                    "seconds": elapsed,
                    "returncode": result.returncode,
                    "workspace": str(workspace),
                    "edit_path": str(relative),
                    "edit_kind": args.edit_kind,
                    "edit": {"before": ORIGINAL, "after": edit}
                    if args.edit_kind == "production"
                    else {"added_test_comment": args.batch_limit},
                    "protocol": "same checkout, warmed local caches, full image archive; no separate bazel fetch",
                }
                records.append(record)
                for filename in filenames:
                    source = cache / filename
                    if source.exists():
                        (args.output / f"{layout}-{phase}-{filename}").write_bytes(
                            source.read_bytes()
                        )
                (args.output / "timings.json").write_text(
                    json.dumps(records, indent=2) + "\n"
                )
                print(json.dumps(record), flush=True)
                if result.returncode:
                    raise SystemExit(result.returncode)
    finally:
        for target, original in originals.items():
            target.write_text(original)


if __name__ == "__main__":
    main()
