"""Preserve the separate supervisor profile without duplicating unchanged deps."""

load("@rules_cc//cc/common:cc_info.bzl", "CcInfo")
load("@rules_rust//rust/private:providers.bzl", "BuildInfo", "CrateInfo", "DepInfo")

def _flag_impl(ctx):
    return []

supervisor_flag = rule(implementation = _flag_impl, build_setting = config.bool(flag = True))

def _normal_impl(settings, attr):
    return {"//ci/benchmarks/controller-bazel:supervisor_profile": False}

def _supervisor_impl(settings, attr):
    return {"//ci/benchmarks/controller-bazel:supervisor_profile": True}

_normal = transition(implementation = _normal_impl, inputs = [], outputs = ["//ci/benchmarks/controller-bazel:supervisor_profile"])
_supervisor = transition(implementation = _supervisor_impl, inputs = [], outputs = ["//ci/benchmarks/controller-bazel:supervisor_profile"])

def _alias_impl(ctx):
    target = ctx.attr.actual[0]
    default = target[DefaultInfo]
    return [DefaultInfo(files = default.files, runfiles = default.default_runfiles)] + [target[provider] for provider in [CrateInfo, DepInfo, BuildInfo, CcInfo, OutputGroupInfo] if provider in target]

def _alias(transition):
    return rule(
        implementation = _alias_impl,
        attrs = {
            "actual": attr.label(mandatory = True, cfg = transition),
            "_allowlist_function_transition": attr.label(default = "@bazel_tools//tools/allowlists/function_transition_allowlist"),
        },
    )

normal_profile = _alias(_normal)
supervisor_profile = _alias(_supervisor)
