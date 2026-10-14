#!/usr/bin/env python3
"""Audit compiler actions reachable from the complete image, including cache hits."""

import argparse
import json
import subprocess
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--bazel", default="bazel")
    args = parser.parse_args()
    root = args.source.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    graph_file = args.output / "action-graph.json"
    with graph_file.open("w") as stream:
        subprocess.run(
            [
                args.bazel,
                "aquery",
                "deps(//ci/benchmarks/kind-bazel/artifacts:image_tarball)",
                "--include_commandline",
                "--include_param_files",
                "--output=jsonproto",
            ],
            cwd=root,
            stdout=stream,
            check=True,
        )
    graph = json.loads(graph_file.read_text())
    fragments = {value["id"]: value for value in graph["pathFragments"]}
    paths = {}

    def path(identifier):
        if identifier not in paths:
            value = fragments[identifier]
            paths[identifier] = (
                path(value["parentId"]) + "/" if value.get("parentId") else ""
            ) + value["label"]
        return paths[identifier]

    artifacts = {
        value["id"]: path(value["pathFragmentId"]) for value in graph["artifacts"]
    }
    producers = {
        output: action
        for action in graph["actions"]
        for output in action.get("outputIds", [])
    }
    dep_sets = {value["id"]: value for value in graph["depSetOfFiles"]}
    memo = {}

    def expand(identifier):
        if identifier not in memo:
            value = dep_sets[identifier]
            memo[identifier] = set(value.get("directArtifactIds", []))
            memo[identifier].update(
                artifact
                for child in value.get("transitiveDepSetIds", [])
                for artifact in expand(child)
            )
        return memo[identifier]

    roots = [
        identifier
        for identifier, filename in artifacts.items()
        if filename.endswith("/ci/benchmarks/kind-bazel/artifacts/load/tarball.tar")
    ]
    if len(roots) != 1:
        raise SystemExit("Expected one final image archive in the action graph")
    visited, reachable = set(), set()
    pending = list(roots)
    while pending:
        identifier = pending.pop()
        if identifier in visited:
            continue
        visited.add(identifier)
        action = producers.get(identifier)
        if action is None:
            continue
        reachable.add(id(action))
        for dep_set in action.get("inputDepSetIds", []):
            pending.extend(expand(dep_set))
    targets = {target["id"]: target["label"] for target in graph["targets"]}
    units = []
    for action in graph["actions"]:
        if action["mnemonic"] != "Rustc" or id(action) not in reachable:
            continue
        params = {
            value["execPath"]: value["arguments"]
            for value in action.get("paramFiles", [])
        }
        arguments = []
        for argument in action["arguments"]:
            if argument.startswith("@") and argument[1:] in params:
                arguments.extend(params[argument[1:]])
            else:
                arguments.append(argument)
        units.append(
            {
                "mnemonic": "Rustc",
                "targetLabel": targets[action["targetId"]],
                "commandArgs": arguments,
            }
        )
    units_file = args.output / "reachable-units.json"
    units_file.write_text(json.dumps(units))
    report_file = args.output / "parity.json"
    with (args.output / "audit.log").open("w") as stream:
        result = subprocess.run(
            [
                "python3",
                "ci/benchmarks/controller-bazel/audit.py",
                str(units_file),
                "--workspace",
                str(root),
                "--cargo-profile",
                str(
                    root
                    / ".cache/controller-benchmark/cargo-profile/benchmark-profile.jsonl"
                ),
                "--output",
                str(report_file),
            ],
            cwd=root,
            stdout=stream,
            stderr=subprocess.STDOUT,
            check=False,
        )
    report = json.loads(report_file.read_text())
    print(
        json.dumps(
            {
                key: len(value) if isinstance(value, list) else value
                for key, value in report.items()
            }
        )
    )
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
