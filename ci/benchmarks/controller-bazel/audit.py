#!/usr/bin/env python3
"""Compare executed Bazel rustc units with Cargo's independent artifact profile."""

import argparse
import json
import re
from pathlib import Path


def objects(path):
    text = path.read_text()
    decoder = json.JSONDecoder()
    offset = 0
    while offset < len(text):
        while offset < len(text) and text[offset].isspace():
            offset += 1
        if offset == len(text):
            return
        value, offset = decoder.raw_decode(text, offset)
        if isinstance(value, list):
            yield from value
        else:
            yield value


def codegen(arguments):
    options = {}
    for index, arg in enumerate(arguments):
        value = None
        if arg == "-C":
            value = arguments[index + 1]
        elif arg.startswith("-C"):
            value = arg[2:]
        elif arg.startswith("--codegen="):
            value = arg.removeprefix("--codegen=")
        if value and "=" in value:
            key, setting = value.split("=", 1)
            options[key] = setting
    return options


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("execution_log", type=Path)
    parser.add_argument(
        "--cargo-profile",
        type=Path,
        default=Path(
            ".cache/controller-benchmark/cargo-profile/benchmark-profile.jsonl"
        ),
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--workspace", type=Path, required=True)
    args = parser.parse_args()
    expected = {}
    by_crate = {}
    for artifact in objects(args.cargo_profile):
        if artifact.get("reason") != "compiler-artifact" or artifact["target"][
            "kind"
        ] == ["custom-build"]:
            continue
        name, version = artifact["package_id"].split("#")[-1].rsplit("@", 1)
        key = name + "-" + version.replace("+", "-")
        expected.setdefault(key, set()).add(
            (tuple(sorted(artifact["features"])), artifact["profile"]["opt_level"])
        )
        by_crate.setdefault(artifact["target"]["name"].replace("-", "_"), set()).add(
            key
        )
    matched = 0
    seen = set()
    differences = []
    ignored_tools = 0
    for action in objects(args.execution_log):
        if action.get("mnemonic") != "Rustc":
            continue
        arguments = []
        for argument in action.get("commandArgs", []):
            if argument.startswith("@") and argument.endswith(".params"):
                response = args.workspace / argument[1:]
                arguments.extend(response.read_text().splitlines())
            else:
                arguments.append(argument)
        crate = next(
            (
                arg.removeprefix("--crate-name=")
                for arg in arguments
                if arg.startswith("--crate-name=")
            ),
            None,
        )
        if (
            crate is None
            or crate in {"build_script_build"}
            or any(arg.endswith("build.rs") for arg in arguments)
        ):
            continue
        label = action.get("targetLabel", "")
        external = re.search(r"crates__(.*)-(\d[^/]*)//", label)
        if external:
            name, version = external.groups()
            key = name + "-" + version
        else:
            keys = by_crate.get(crate, set())
            if len(keys) != 1:
                ignored_tools += 1
                continue
            key = next(iter(keys))
        if key not in expected:
            differences.append({"label": label, "unexpected_package": key})
            continue
        features = tuple(
            sorted(
                arg[9:-1]
                for arg in arguments
                if arg.startswith('feature="') and arg.endswith('"')
            )
        )
        flags = codegen(arguments)
        actual = (features, flags.get("opt-level", "0"))
        if actual not in expected[key]:
            differences.append(
                {
                    "label": label,
                    "package": key,
                    "actual": actual,
                    "expected": sorted(expected[key]),
                }
            )
        else:
            matched += 1
            seen.add((key, actual))
        if flags.get("embed-bitcode") != "no" or flags.get("strip") != "symbols":
            differences.append({"label": label, "codegen": flags})
    missing = sorted(
        (key, variant)
        for key, variants in expected.items()
        for variant in variants
        if (key, variant) not in seen
    )
    report = {
        "matched_units": matched,
        "missing_units": missing,
        "ignored_bazel_tools": ignored_tools,
        "differences": differences,
    }
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    if not matched or differences or missing:
        raise SystemExit("Compiler-unit parity failed; do not interpret timings")


if __name__ == "__main__":
    main()
