"""Experimental Cargo-derived first-party targets for the controller benchmark."""

load("@crates//:data.bzl", "DEP_DATA")
load("@rules_rs//rs:cargo_build_script.bzl", "cargo_build_script")
load("@rules_rs//rs:rust_binary.bzl", "rust_binary")
load("@rules_rs//rs:rust_library.bzl", "rust_library")
load("@rules_rs//rs:rust_proc_macro.bzl", "rust_proc_macro")
load(":closure.bzl", "RELEASE_EXTERNAL")
load(":features.bzl", "RELEASE_FEATURES")
load(":units.bzl", "CARGO_UNITS", "HOST_PACKAGES", "SUPERVISOR_UNITS", "SUPERVISOR_VARIANT_PATHS", "TARGET_PACKAGES")
load(":variants.bzl", "normal_profile")

_PREFIX = "//.cache/controller-benchmark/resolver/"

def _label(value):
    return "//" + value.removeprefix(_PREFIX) if value.startswith(_PREFIX) else value

def _dependency(value, aliases):
    label = _label(value)
    package = label.split(":")[0].removeprefix("//")
    if package in SUPERVISOR_VARIANT_PATHS or not SUPERVISOR_VARIANT_PATHS:
        return label
    if label not in aliases:
        name = "normal_" + label.replace("@", "_").replace("/", "_").replace(":", "_").replace("+", "_")
        normal_profile(name = name, actual = label)
        aliases[label] = ":" + name
    return aliases[label]

def _labels(data, kind):
    return data[kind] + [label for values in data.get(kind + "_by_platform", {}).values() for label in values]

def _deps(data, kind, aliases):
    shared = [_dependency(value, aliases) for value in data[kind] if not value.startswith("@crates//:") or value in RELEASE_EXTERNAL]
    branches = {platform: [_dependency(value, aliases) for value in deps if not value.startswith("@crates//:") or value in RELEASE_EXTERNAL] for platform, deps in data.get(kind + "_by_platform", {}).items()}
    if branches:
        branches["//conditions:default"] = []
        return shared + select(branches)
    return shared

def space_crate(name, version, crate_root, cargo_name, rust_name, cargo_env, binary = False, proc_macro = False, build_script = False):
    package = native.package_name()
    data = DEP_DATA[".cache/controller-benchmark/resolver/" + package]
    unit = CARGO_UNITS[cargo_name + "-" + version]
    supervisor = SUPERVISOR_UNITS.get(cargo_name + "-" + version, unit)
    features = select({
        "//ci/benchmarks/controller-bazel:supervisor_exec_configuration": supervisor.get("host", supervisor.get("target", [])),
        "//ci/benchmarks/controller-bazel:supervisor_target_configuration": supervisor.get("target", supervisor.get("host", [])),
        "//ci/benchmarks/controller-bazel:exec_configuration": unit.get("host", unit.get("target", [])),
        "//conditions:default": unit.get("target", unit.get("host", [])),
    })
    dependency_aliases = {}
    deps = _deps(data, "deps", dependency_aliases)
    build_deps = _deps(data, "build_deps", dependency_aliases) if build_script else []
    runtime_labels = {_label(value): True for value in _labels(data, "deps")}
    build_labels = {_label(value): True for value in _labels(data, "build_deps")}
    crate_aliases = {dependency_aliases.get(_label(key), _label(key)): value for key, value in data["aliases"].items() if _label(key) in runtime_labels}
    build_aliases = {dependency_aliases.get(_label(key), _label(key)): value for key, value in data["aliases"].items() if _label(key) in build_labels}

    # These files only participate under cfg(test); including them in production
    # action inputs causes rebuilds when a test changes.
    test_files = ["**/tests.rs", "**/tests/**", "**/*.test.rs", "tests/**", "**/*.snap", "**/*.snap.new"]
    if build_script:
        cargo_build_script(
            name = "build_script",
            srcs = ["build.rs"],
            crate_name = data["crate_name"] + "_build_script",
            rustc_env = cargo_env,
            version = version,
            edition = data["edition"],
            crate_features = features,
            deps = build_deps,
            aliases = build_aliases,
            tools = ["@benchmark_protoc//:bin/protoc"],
            build_script_env = {
                "PROTOC": "$(execpath @benchmark_protoc//:bin/protoc)",
                "CARGO_MANIFEST_DIR": package,
                "CARGO_WORKSPACE_DIR": "../" * len(package.split("/")),
            },
            data = ["//:build_inputs", "@benchmark_protoc//:includes", "Cargo.toml"] + native.glob(["src/**", "migrations/**"], exclude = test_files, allow_empty = True),
        )
        deps += [":build_script"]
    rule = rust_binary if binary else rust_proc_macro if proc_macro else rust_library
    rule(
        name = name,
        crate_name = rust_name,
        rustc_env = cargo_env,
        crate_root = crate_root,
        srcs = native.glob(["src/**/*.rs"], exclude = test_files),
        compile_data = ["//:build_inputs"] + native.glob(["**"], exclude = ["BUILD.bazel"] + test_files),
        version = version,
        edition = data["edition"],
        crate_features = features,
        aliases = crate_aliases,
        deps = deps,
        visibility = ["//visibility:public"],
    )
