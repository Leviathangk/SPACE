"""Expose existing framework test files without changing their assertions."""

load("@rules_shell//shell:sh_test.bzl", "sh_test")

def smoke_unit_tests():
    targets = []
    for path in native.glob(["tests/test_*.py"]):
        name = path.removeprefix("tests/").removesuffix(".py")
        sh_test(
            name = name,
            env_inherit = ["PATH"],
            srcs = ["//ci/benchmarks/kind-bazel:unit.sh"],
            args = ["space_smoke/" + path],
            data = [":sources", "//:smoke_test_support"],
            size = "small",
            # This first adapter uses the repository's locked uv environment;
            # it requires installed uv/Python and doesn't claim hermetic caching.
            tags = ["external", "no-sandbox", "no-remote"],
        )
        targets.append(":" + name)
    native.test_suite(name = "unit", tests = targets)
